//! Stufe 2 der Schwärz-Bühne: HTML sanitizen + Findings im DOM verorten (WP-G).
//!
//! Reine Logik ohne OS-Abhängigkeit — das Clipboard-HTML kommt aus
//! [`crate::clipboard`], die Findings aus [`crate::detection`], hier passiert
//! nur String-/DOM-Arbeit. Drei Schritte:
//!
//! 1. [`sanitize`]: ammonia-Allowlist. **Alle Remote-Referenzen fliegen raus**
//!    (Tracking-Pixel in HTML-Mails vs. No-Outbound-Versprechen); `data:`-Bilder
//!    bleiben bis 256 KB erhalten, größere werden durch einen Platzhalter
//!    ersetzt. `script`/`iframe`/`form` entfernt ammonia ohnehin.
//! 2. [`extract_plaintext`]: Textknoten in Dokumentordnung konkatenieren,
//!    Block-Grenzen als `\n` — auf diesem Plaintext läuft die bestehende
//!    Detection (Byte-Offsets der Findings beziehen sich auf GENAU diesen Text).
//! 3. [`redact`]: Findings zurück auf Textknoten-Ranges mappen und daraus
//!    beide Ausgaben bauen: `annotated_html` (Fundstellen als
//!    `<span data-sz-finding …>Original</span>` für die Marker-Animation) und
//!    `redacted_html` (Fundstellen durch das Token ersetzt).
//!
//! # Warum der Plaintext zweimal identisch entstehen muss
//!
//! Detection läuft auf dem Ergebnis von [`extract_plaintext`]; [`redact`]
//! parst dasselbe sanitisierte HTML erneut und läuft denselben Walk. Beide
//! Walks MÜSSEN denselben Plaintext produzieren, sonst zeigen die
//! Finding-Offsets ins Leere. Deshalb teilen sich beide Pfade die eine
//! [`walk_text_nodes`]-Implementierung; es gibt einen Test, der die
//! Übereinstimmung festnagelt.
//!
//! # Findings über Knotengrenzen
//!
//! `Max <b>Müller</b>` ist im Plaintext ein zusammenhängendes Finding, im DOM
//! aber zwei Textknoten. Vertrag (Konzept 4/WP-G): die Ersetzung landet im
//! **ersten** Knoten, die Rest-Anteile werden geleert. Für `annotated_html`
//! heißt das: jeder Teil bekommt seinen eigenen Span (damit der Marker-Strich
//! die komplette Fundstelle überzieht), aber nur der **erste** trägt das
//! Replacement — Fortsetzungs-Spans sind mit `data-sz-cont` markiert und
//! kollabieren in der Animation zu Leere.

use kuchikiki::traits::TendrilSink;
use kuchikiki::NodeRef;

use crate::detection::Finding;

/// Obergrenze für eingebettete `data:`-Bilder. Darüber ersetzt [`sanitize`]
/// das Bild durch einen Text-Platzhalter — riesige Inline-Bilder würden
/// Anzeige, Ablage und Clipboard aufblähen, ohne dass die Detection sie je
/// ansieht.
const DATA_IMAGE_MAX_BYTES: usize = 256 * 1024;

/// Platzhalter, der ein entferntes Bild (Remote-Referenz oder zu großes
/// `data:`-Bild) im Dokument sichtbar vertritt.
const IMAGE_PLACEHOLDER: &str = "[Bild entfernt]";

/// CSS-Properties, die ein `style`-Attribut überleben dürfen. Bewusst nur
/// Typografie/Farbe/Abstände — nichts, was URLs laden könnte (`background`
/// mit `url(…)` wäre ein Outbound-Kanal) oder Layout-Traps stellt
/// (`position: fixed` über der ganzen Bühne).
const ALLOWED_STYLE_PROPS: &[&str] = &[
    "color",
    "background-color",
    "font-family",
    "font-size",
    "font-weight",
    "font-style",
    "font-variant",
    "text-decoration",
    "text-decoration-line",
    "text-align",
    "vertical-align",
    "line-height",
    "letter-spacing",
    "white-space",
    "margin",
    "margin-top",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "padding",
    "padding-top",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "border",
    "border-top",
    "border-bottom",
    "border-left",
    "border-right",
    "border-collapse",
    "border-spacing",
    "width",
    "height",
    "max-width",
    "list-style-type",
];

/// Elemente, deren Grenzen im Plaintext als Zeilenumbruch zählen. Muss alles
/// abdecken, was optisch einen Block bildet — sonst klebt die Detection Wörter
/// über Absatzgrenzen zusammen („…GrüßeMax Mustermann").
const BLOCK_ELEMENTS: &[&str] = &[
    "p", "div", "br", "hr", "li", "ul", "ol", "dl", "dt", "dd", "table", "thead", "tbody",
    "tfoot", "tr", "caption", "blockquote", "pre", "h1", "h2", "h3", "h4", "h5", "h6",
];

/// Tabellenzellen: keine ganze Zeile, aber eine Wortgrenze — als Tab getrennt,
/// damit „Name | Telefon"-Spalten nicht zu einem Wort verschmelzen.
const CELL_ELEMENTS: &[&str] = &["td", "th"];

// =================================================================== Sanitize

/// Sanitisiert Clipboard-HTML auf eine strikte Allowlist.
///
/// - Struktur-/Format-Tags bleiben (siehe Builder unten), `script`, `iframe`,
///   `form`, Event-Handler etc. entfernt ammonia.
/// - **Attribute:** nur gefiltertes `style`, `colspan`/`rowspan` auf Zellen
///   und `src` auf Bildern. Bewusst kein `href`, kein `title`, kein `alt`:
///   Attribute laufen NICHT durch die Detection — eine Mail-Adresse in einem
///   `href="mailto:…"` würde ungeschwärzt überleben. Der Link-Text bleibt
///   als Text erhalten.
/// - **`src`:** ausschließlich `data:image/…` bis [`DATA_IMAGE_MAX_BYTES`].
///   `http(s)`-Bilder (Tracking-Pixel!) verlieren ihr `src` und werden im
///   DOM-Schritt durch [`IMAGE_PLACEHOLDER`] ersetzt.
/// - **`style`:** nur Properties aus [`ALLOWED_STYLE_PROPS`], und jede
///   Declaration mit `url(`/`expression(` fliegt komplett raus.
pub fn sanitize(html: &str) -> String {
    use std::borrow::Cow;

    // Volle Setter statt `add_*`: `Builder::empty()` leert nur die Tag-Liste,
    // Default-Attribute (title!) und link_rel blieben sonst aktiv.
    let mut builder = ammonia::Builder::empty();
    builder
        .tags(
            [
                "a", "b", "strong", "i", "em", "u", "s", "strike", "del", "ins", "sub",
                "sup", "span", "font", "p", "div", "br", "hr", "blockquote", "pre", "code",
                "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "dl", "dt", "dd",
                "table", "thead", "tbody", "tfoot", "tr", "td", "th", "caption", "img",
            ]
            .into_iter()
            .collect(),
        )
        .generic_attributes(["style"].into_iter().collect())
        .tag_attributes(
            [
                ("td", ["colspan", "rowspan"].into_iter().collect()),
                ("th", ["colspan", "rowspan"].into_iter().collect()),
                ("img", ["src"].into_iter().collect()),
            ]
            .into_iter()
            .collect(),
        )
        .link_rel(None)
        // `data:` ist das einzige erlaubte URL-Schema — trifft nur `img src`,
        // andere URL-Attribute sind gar nicht erst in der Allowlist.
        .url_schemes(["data"].into_iter().collect())
        .attribute_filter(|element, attribute, value| match (element, attribute) {
            ("img", "src") => {
                if value.starts_with("data:image/") && value.len() <= DATA_IMAGE_MAX_BYTES {
                    Some(Cow::Borrowed(value))
                } else {
                    None
                }
            }
            (_, "style") => {
                let filtered = filter_style(value);
                if filtered.is_empty() {
                    None
                } else {
                    Some(Cow::Owned(filtered))
                }
            }
            _ => Some(Cow::Borrowed(value)),
        });

    builder.clean(html).to_string()
}

/// Filtert ein `style`-Attribut auf die Property-Allowlist. Declarations mit
/// `url(`/`expression(` werden unabhängig von der Property verworfen (Belt &
/// Suspenders — die erlaubten Properties laden ohnehin keine URLs).
fn filter_style(style: &str) -> String {
    style
        .split(';')
        .filter_map(|decl| {
            let (prop, value) = decl.split_once(':')?;
            let prop = prop.trim().to_ascii_lowercase();
            let value = value.trim();
            let value_lower = value.to_ascii_lowercase();
            if !ALLOWED_STYLE_PROPS.contains(&prop.as_str()) {
                return None;
            }
            if value_lower.contains("url(") || value_lower.contains("expression(") {
                return None;
            }
            Some(format!("{prop}: {value}"))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

// =================================================================== DOM-Walk

/// Ein Textknoten im Dokument mit seiner Byte-Range im extrahierten Plaintext.
struct TextSlice {
    node: NodeRef,
    /// Byte-Offset des Knoten-Texts im Plaintext (inklusive).
    plain_start: usize,
    /// Byte-Offset des Knoten-Endes im Plaintext (exklusive).
    plain_end: usize,
}

/// Läuft den DOM in Dokumentordnung ab und liefert `(plaintext, slices)`.
/// Block-Grenzen ([`BLOCK_ELEMENTS`]) erzeugen synthetische `\n` im Plaintext
/// (ohne zugehörigen Knoten), Zellgrenzen ([`CELL_ELEMENTS`]) ein Tab.
///
/// Ersetzt außerdem — als Vorbereitung, nicht Teil des Mappings — jedes
/// `img` ohne `src` (von [`sanitize`] entfernte Remote-/Riesen-Bilder) durch
/// einen [`IMAGE_PLACEHOLDER`]-Textknoten, damit der Platzhalter durch alle
/// weiteren Schritte wie normaler Text läuft.
fn walk_text_nodes(root: &NodeRef) -> (String, Vec<TextSlice>) {
    replace_srcless_images(root);

    let mut plaintext = String::new();
    let mut slices = Vec::new();
    walk_recursive(root, &mut plaintext, &mut slices);
    (plaintext, slices)
}

/// `img` ohne `src` → Text-Platzhalter. Muss VOR dem Text-Walk laufen, damit
/// der Platzhalter im Plaintext (und damit in der Anzeige) auftaucht.
fn replace_srcless_images(root: &NodeRef) {
    let imgs: Vec<NodeRef> = root
        .inclusive_descendants()
        .filter(|n| {
            n.as_element().is_some_and(|e| {
                &*e.name.local == "img" && !e.attributes.borrow().contains("src")
            })
        })
        .collect();
    for img in imgs {
        img.insert_after(NodeRef::new_text(IMAGE_PLACEHOLDER));
        img.detach();
    }
}

fn walk_recursive(node: &NodeRef, plaintext: &mut String, slices: &mut Vec<TextSlice>) {
    for child in node.children() {
        if let Some(text) = child.as_text() {
            let content = text.borrow().to_string();
            if content.is_empty() {
                continue;
            }
            let start = plaintext.len();
            plaintext.push_str(&content);
            slices.push(TextSlice {
                node: child.clone(),
                plain_start: start,
                plain_end: plaintext.len(),
            });
            continue;
        }
        if let Some(element) = child.as_element() {
            let name = element.name.local.to_string();
            let is_block = BLOCK_ELEMENTS.contains(&name.as_str());
            let is_cell = CELL_ELEMENTS.contains(&name.as_str());
            if is_block && !plaintext.ends_with('\n') && !plaintext.is_empty() {
                plaintext.push('\n');
            }
            walk_recursive(&child, plaintext, slices);
            if is_block && !plaintext.ends_with('\n') && !plaintext.is_empty() {
                plaintext.push('\n');
            }
            if is_cell && !plaintext.ends_with(['\n', '\t']) && !plaintext.is_empty() {
                plaintext.push('\t');
            }
            continue;
        }
        // Kommentare/Doctype: nichts zu tun (ammonia strippt Kommentare eh).
        walk_recursive(&child, plaintext, slices);
    }
}

// =================================================================== Redact

/// Ergebnis von [`redact`]: beide HTML-Fassungen plus der Plaintext, auf dem
/// die Findings berechnet wurden (praktisch für Aufrufer-Assertions/Logging).
pub struct RichRedaction {
    /// Sanitisiertes HTML, Fundstellen als `<span data-sz-finding …>` markiert
    /// (Original noch sichtbar) — Input für die Marker-Animation im Frontend.
    pub annotated_html: String,
    /// Sanitisiertes HTML, Fundstellen durch ihr Replacement-Token ersetzt —
    /// das, was in Clipboard und Ablage landet.
    pub redacted_html: String,
}

/// Baut aus dem **sanitisierten** HTML und den Findings (Byte-Offsets im
/// Plaintext von [`extract_plaintext`]) die annotierte und die geschwärzte
/// HTML-Fassung.
///
/// Findings, deren Offsets sich nicht sauber auf Textknoten mappen lassen
/// (dürfte nie passieren — Detection und Walk teilen sich den Plaintext),
/// werden defensiv übersprungen statt zu panicken.
pub fn redact(sanitized_html: &str, findings: &[Finding]) -> RichRedaction {
    RichRedaction {
        annotated_html: apply_to_dom(sanitized_html, findings, RenderMode::Annotate),
        redacted_html: apply_to_dom(sanitized_html, findings, RenderMode::Redact),
    }
}

/// Extrahiert den Detection-Plaintext aus dem **sanitisierten** HTML.
/// Byte-Offsets der darauf berechneten Findings passen zu [`redact`].
pub fn extract_plaintext(sanitized_html: &str) -> String {
    let (plaintext, _slices) = walk_text_nodes(&parse(sanitized_html));
    plaintext
}

/// Parst HTML zu einem Dokument-Knoten. Kuchikikis `one()` liefert den Sink;
/// der eigentliche Baum hängt an `document_node`.
fn parse(html: &str) -> NodeRef {
    kuchikiki::parse_html().one(html).document_node
}

#[derive(Clone, Copy, PartialEq)]
enum RenderMode {
    Annotate,
    Redact,
}

/// Der gemeinsame Kern hinter beiden [`redact`]-Ausgaben: parsen, Walk,
/// Findings auf Knoten-Ranges verteilen, Textknoten zerschneiden, Body-Inhalt
/// serialisieren.
fn apply_to_dom(sanitized_html: &str, findings: &[Finding], mode: RenderMode) -> String {
    let document = parse(sanitized_html);
    let (_plaintext, slices) = walk_text_nodes(&document);

    let mut sorted: Vec<&Finding> = findings.iter().collect();
    sorted.sort_by_key(|f| f.start);

    // Pro Textknoten die Schnitte einsammeln — erst sammeln, dann mutieren:
    // der Walk darf nicht gleichzeitig den DOM umbauen.
    let mut cuts_per_slice: Vec<Vec<Cut>> = Vec::with_capacity(slices.len());
    cuts_per_slice.resize_with(slices.len(), Vec::new);

    let mut prev_end = 0usize;
    for f in sorted {
        // Defensive: rückwärts/überlappend → skip (Detection garantiert
        // Überlappungsfreiheit, aber ein Bug darf hier nicht die ganze
        // Bühne reißen).
        if f.start < prev_end || f.start > f.end {
            continue;
        }
        let mut is_first = true;
        for (i, slice) in slices.iter().enumerate() {
            if slice.plain_end <= f.start || slice.plain_start >= f.end {
                continue;
            }
            let local_start = f.start.max(slice.plain_start) - slice.plain_start;
            let local_end = f.end.min(slice.plain_end) - slice.plain_start;
            cuts_per_slice[i].push(Cut {
                local_start,
                local_end,
                finding: f.clone(),
                is_first_part: is_first,
            });
            is_first = false;
        }
        prev_end = f.end;
    }

    for (slice, cuts) in slices.iter().zip(cuts_per_slice) {
        if cuts.is_empty() {
            continue;
        }
        rebuild_text_node(slice, &cuts, mode);
    }

    serialize_body(&document)
}

/// Ein Schnitt in einem Textknoten: welcher lokale Byte-Range des Knotens
/// gehört zu welchem Finding, und ist es der erste Teil des Findings (nur der
/// trägt das Replacement — Fortsetzungs-Teile werden geleert/kollabiert).
struct Cut {
    local_start: usize,
    local_end: usize,
    finding: Finding,
    is_first_part: bool,
}

/// Zerschneidet einen Textknoten an den Cut-Grenzen und ersetzt ihn durch die
/// Folge aus Text-Resten und Finding-Knoten (Annotations-Span bzw. Token-Text).
/// Die Cuts sind aufsteigend sortiert und überlappungsfrei (Vorbedingung aus
/// [`apply_to_dom`] — Findings sind es, und lokale Ranges erben das).
fn rebuild_text_node(slice: &TextSlice, cuts: &[Cut], mode: RenderMode) {
    let original_text = match slice.node.as_text() {
        Some(t) => t.borrow().to_string(),
        None => return,
    };

    let mut replacement_nodes: Vec<NodeRef> = Vec::new();
    let mut pos = 0usize;
    for cut in cuts {
        // Defensive gegen kaputte lokale Ranges (dürfte nie greifen).
        if cut.local_start < pos
            || cut.local_end > original_text.len()
            || cut.local_start > cut.local_end
        {
            continue;
        }
        if cut.local_start > pos {
            replacement_nodes.push(NodeRef::new_text(&original_text[pos..cut.local_start]));
        }
        let part = &original_text[cut.local_start..cut.local_end];
        match mode {
            RenderMode::Annotate => {
                replacement_nodes.push(annotation_span(part, cut));
            }
            RenderMode::Redact => {
                // Ersetzung nur im ersten Teil; Fortsetzungs-Teile werden
                // geleert (Vertrag: „Ersetzung in den ersten Knoten,
                // Rest-Anteile leeren").
                if cut.is_first_part {
                    replacement_nodes.push(NodeRef::new_text(&cut.finding.token));
                }
            }
        }
        pos = cut.local_end;
    }
    if pos < original_text.len() {
        replacement_nodes.push(NodeRef::new_text(&original_text[pos..]));
    }

    for node in replacement_nodes.into_iter().rev() {
        slice.node.insert_after(node);
    }
    slice.node.detach();
}

/// Baut den Annotations-Span für einen Finding-Teil:
/// `<span data-sz-finding data-original data-replacement data-entity-type
/// data-confidence>Teil-Text</span>`; Fortsetzungs-Teile tragen zusätzlich
/// `data-sz-cont` und ein leeres Replacement.
fn annotation_span(part_text: &str, cut: &Cut) -> NodeRef {
    use html5ever::{local_name, namespace_url, ns, LocalName, QualName};
    use kuchikiki::{Attribute, ExpandedName};

    let attr = |name: &str, value: &str| {
        (
            ExpandedName::new(ns!(), LocalName::from(name)),
            Attribute {
                prefix: None,
                value: value.to_string(),
            },
        )
    };

    let mut attributes = vec![
        attr("data-sz-finding", ""),
        attr("data-entity-type", &cut.finding.entity_type),
        attr("data-confidence", &format!("{:.2}", cut.finding.confidence)),
    ];
    if cut.is_first_part {
        attributes.push(attr("data-original", &cut.finding.original));
        attributes.push(attr("data-replacement", &cut.finding.token));
    } else {
        attributes.push(attr("data-sz-cont", ""));
        attributes.push(attr("data-replacement", ""));
    }

    let span = NodeRef::new_element(
        QualName::new(None, ns!(html), local_name!("span")),
        attributes,
    );
    span.append(NodeRef::new_text(part_text));
    span
}

/// Serialisiert den `<body>`-Inhalt des geparsten Dokuments (kuchikiki wickelt
/// Fragmente beim Parsen in `<html><head/><body>…`). Fallback: ganzes Dokument.
fn serialize_body(document: &NodeRef) -> String {
    let body = match document.select_first("body") {
        Ok(b) => b.as_node().clone(),
        Err(()) => return document.to_string(),
    };
    let mut out = Vec::new();
    for child in body.children() {
        if child.serialize(&mut out).is_err() {
            log::warn!("richtext: serialize eines Body-Kindes fehlgeschlagen");
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

// =================================================================== Tests

#[cfg(test)]
mod tests {
    use super::*;

    /// Baut ein Finding über die erste Fundstelle von `needle` im Plaintext.
    /// Panict, wenn `needle` fehlt — Test-Fehler, kein Produktions-Pfad.
    fn finding_for(plaintext: &str, needle: &str, token: &str, entity_type: &str) -> Finding {
        let start = plaintext
            .find(needle)
            .unwrap_or_else(|| panic!("'{needle}' nicht in {plaintext:?}"));
        Finding {
            entity_type: entity_type.to_string(),
            original: needle.to_string(),
            token: token.to_string(),
            start,
            end: start + needle.len(),
            confidence: 0.9,
        }
    }

    // ----------------------------------------------------- Sanitize

    #[test]
    fn sanitize_strips_script_and_event_handlers() {
        let dirty = r#"<p onclick="evil()">Hi</p><script>alert(1)</script>"#;
        let clean = sanitize(dirty);
        assert!(!clean.contains("script"), "got: {clean}");
        assert!(!clean.contains("onclick"), "got: {clean}");
        assert!(clean.contains("<p>Hi</p>"), "got: {clean}");
    }

    #[test]
    fn sanitize_removes_tracking_pixel_src() {
        // Der Klassiker in HTML-Mails: 1x1-Pixel mit Remote-URL. Das `src`
        // muss fallen (No-Outbound), der Platzhalter erscheint erst im
        // DOM-Schritt (siehe redact_replaces_srcless_image_with_placeholder).
        let dirty = r#"<p>Hallo</p><img src="https://tracker.example/p.gif">"#;
        let clean = sanitize(dirty);
        assert!(!clean.contains("http"), "Remote-Referenz überlebt: {clean}");
        assert!(!clean.contains("src="), "src-Attribut überlebt: {clean}");
    }

    #[test]
    fn sanitize_keeps_small_data_image() {
        let dirty = r#"<img src="data:image/png;base64,iVBORw0KGgo=">"#;
        let clean = sanitize(dirty);
        assert!(
            clean.contains("data:image/png;base64,iVBORw0KGgo="),
            "kleines data:-Bild muss bleiben: {clean}"
        );
    }

    #[test]
    fn sanitize_drops_oversized_data_image() {
        let huge = format!(
            r#"<img src="data:image/png;base64,{}">"#,
            "A".repeat(DATA_IMAGE_MAX_BYTES + 1)
        );
        let clean = sanitize(&huge);
        assert!(!clean.contains("data:image"), "Riesen-Bild muss fallen");
    }

    #[test]
    fn sanitize_strips_href_and_title() {
        // Attribute laufen nicht durch die Detection — eine Mail-Adresse in
        // href="mailto:…" würde ungeschwärzt überleben. Linktext bleibt.
        let dirty = r#"<a href="mailto:max@example.de" title="Max">Kontakt</a>"#;
        let clean = sanitize(dirty);
        assert!(!clean.contains("max@example.de"), "got: {clean}");
        assert!(!clean.contains("title="), "got: {clean}");
        assert!(clean.contains("Kontakt"), "got: {clean}");
    }

    #[test]
    fn sanitize_filters_style_to_allowlist() {
        let dirty = r#"<p style="color: red; background: url(https://x.example/a.png); position: fixed">T</p>"#;
        let clean = sanitize(dirty);
        assert!(clean.contains("color: red"), "got: {clean}");
        assert!(!clean.contains("url("), "got: {clean}");
        assert!(!clean.contains("position"), "got: {clean}");
    }

    #[test]
    fn filter_style_drops_url_even_on_allowed_prop() {
        // Belt & Suspenders: selbst wenn eine erlaubte Property eine URL
        // schmuggeln will, fliegt die ganze Declaration.
        assert_eq!(filter_style("color: url(https://x.example)"), "");
        assert_eq!(filter_style("color: red; width: 10px"), "color: red; width: 10px");
    }

    // ----------------------------------------------------- Plaintext-Extraktion

    #[test]
    fn plaintext_inserts_newlines_at_block_boundaries() {
        let html = sanitize("<p>Mit freundlichen Grüßen</p><p>Max Mustermann</p>");
        let text = extract_plaintext(&html);
        assert!(
            text.contains("Grüßen\n"),
            "Absatzgrenze muss \\n sein, sonst klebt die Detection Wörter zusammen: {text:?}"
        );
    }

    #[test]
    fn plaintext_separates_table_cells() {
        let html = sanitize("<table><tr><td>Name</td><td>Telefon</td></tr></table>");
        let text = extract_plaintext(&html);
        assert!(
            text.contains("Name\t"),
            "Zellgrenze muss Trenner haben: {text:?}"
        );
    }

    #[test]
    fn plaintext_decodes_entities() {
        let html = sanitize("<p>M&uuml;ller &amp; S&ouml;hne</p>");
        let text = extract_plaintext(&html);
        assert!(text.contains("Müller & Söhne"), "got: {text:?}");
    }

    #[test]
    fn plaintext_of_word_like_html_survives() {
        // Word/Outlook-typisch: Namespace-Tags (o:p), mso-Styles, Kommentare.
        // Alles Unbekannte fällt, der Text bleibt.
        let word = r#"<html><head><meta charset="utf-8"><style>p{mso-style-name:x}</style></head>
            <body><!--[if mso]>conditional<![endif]--><p class="MsoNormal" style="mso-margin-top-alt:auto">
            Sehr geehrter Herr <b>M&uuml;ller</b>,<o:p></o:p></p></body></html>"#;
        let clean = sanitize(word);
        let text = extract_plaintext(&clean);
        assert!(text.contains("Sehr geehrter Herr Müller,"), "got: {text:?}");
        assert!(!clean.contains("mso-"), "mso-Style überlebt: {clean}");
        assert!(!text.contains("conditional"), "Kommentar-Inhalt überlebt: {text:?}");
    }

    // ----------------------------------------------------- Redact/Annotate

    #[test]
    fn redact_single_node_finding() {
        let html = sanitize("<p>Sehr geehrter Herr Müller, hallo</p>");
        let text = extract_plaintext(&html);
        let f = finding_for(&text, "Herr Müller", "«P_a4b»", "person");
        let out = redact(&html, &[f]);

        // annotated: Original im Span mit allen data-Attributen.
        assert!(
            out.annotated_html.contains("data-sz-finding"),
            "got: {}",
            out.annotated_html
        );
        assert!(out.annotated_html.contains(">Herr Müller</span>"));
        assert!(out.annotated_html.contains(r#"data-replacement="«P_a4b»""#));
        assert!(out.annotated_html.contains(r#"data-entity-type="person""#));

        // redacted: Token statt Original, kein Span.
        assert!(out.redacted_html.contains("«P_a4b»"));
        assert!(!out.redacted_html.contains("Müller"));
        assert!(!out.redacted_html.contains("data-sz-finding"));
    }

    #[test]
    fn redact_finding_across_tag_boundary() {
        // "Max <b>Mustermann</b>" — ein Finding, zwei Textknoten.
        let html = sanitize("<p>Von Max <b>Mustermann</b> gesendet</p>");
        let text = extract_plaintext(&html);
        let f = finding_for(&text, "Max Mustermann", "«P_x9z»", "person");
        let out = redact(&html, &[f]);

        // annotated: zwei Spans — erster mit Replacement, zweiter als
        // Fortsetzung markiert; der <b>-Rahmen bleibt stehen.
        assert_eq!(
            out.annotated_html.matches("data-sz-finding").count(),
            2,
            "got: {}",
            out.annotated_html
        );
        assert_eq!(out.annotated_html.matches("data-sz-cont").count(), 1);
        assert!(out.annotated_html.contains("<b>"));

        // redacted: Token einmal (im ersten Knoten), der Name ist komplett
        // weg, das <b> bleibt (leer) stehen.
        assert_eq!(out.redacted_html.matches("«P_x9z»").count(), 1);
        assert!(!out.redacted_html.contains("Max"));
        assert!(!out.redacted_html.contains("Mustermann"));
        assert!(out.redacted_html.contains("gesendet"));
    }

    #[test]
    fn redact_keeps_surrounding_formatting() {
        let html = sanitize(r#"<p style="color: red">IBAN <i>DE89370400440532013000</i> Ende</p>"#);
        let text = extract_plaintext(&html);
        let f = finding_for(&text, "DE89370400440532013000", "«I_k2m»", "iban");
        let out = redact(&html, &[f]);
        assert!(out.redacted_html.contains("color: red"), "got: {}", out.redacted_html);
        assert!(out.redacted_html.contains("<i>«I_k2m»</i>"), "got: {}", out.redacted_html);
    }

    #[test]
    fn redact_replaces_srcless_image_with_placeholder() {
        let html = sanitize(r#"<p>Text</p><img src="https://tracker.example/p.gif">"#);
        let text = extract_plaintext(&html);
        assert!(text.contains(IMAGE_PLACEHOLDER), "got: {text:?}");
        let out = redact(&html, &[]);
        assert!(out.redacted_html.contains(IMAGE_PLACEHOLDER));
        assert!(!out.redacted_html.contains("<img"));
    }

    #[test]
    fn redact_handles_umlauts_around_finding() {
        // Mehr-Byte-Zeichen direkt an den Finding-Grenzen — Byte-Slicing darf
        // nie mitten in ein UTF-8-Zeichen schneiden.
        let html = sanitize("<p>Grüße an Max über Köln</p>");
        let text = extract_plaintext(&html);
        let f = finding_for(&text, "Max", "«P_q»", "person");
        let out = redact(&html, &[f]);
        assert!(out.redacted_html.contains("Grüße an «P_q» über Köln"));
    }

    #[test]
    fn redact_multiple_findings_in_one_node() {
        let html = sanitize("<p>Max und Eva kommen</p>");
        let text = extract_plaintext(&html);
        let f1 = finding_for(&text, "Max", "«P_a»", "person");
        let f2 = finding_for(&text, "Eva", "«P_b»", "person");
        let out = redact(&html, &[f1, f2]);
        assert!(out.redacted_html.contains("«P_a» und «P_b» kommen"));
        assert_eq!(out.annotated_html.matches("data-sz-finding").count(), 2);
    }

    #[test]
    fn redact_with_no_findings_returns_sanitized_equivalent() {
        let html = sanitize("<p>Nur <b>Text</b></p>");
        let out = redact(&html, &[]);
        assert!(out.annotated_html.contains("Nur <b>Text</b>"));
        assert_eq!(out.annotated_html, out.redacted_html);
    }

    #[test]
    fn full_pipeline_with_real_detection() {
        // End-to-End über die echte Detection: E-Mail in formatiertem HTML.
        let html = sanitize("<p>Schreib an <b>max.mustermann@example.de</b> bitte</p>");
        let text = extract_plaintext(&html);
        let findings = crate::detection::detect(&text);
        assert!(
            findings.iter().any(|f| f.entity_type == "email"),
            "Detection findet die Mail nicht: {text:?}"
        );
        let out = redact(&html, &findings);
        assert!(!out.redacted_html.contains("max.mustermann@example.de"));
        assert!(out.redacted_html.contains("<b>"));
    }

    #[test]
    fn annotated_escapes_attribute_values() {
        // data-original landet in einem Attribut — HTML-Metazeichen im
        // Original dürfen das Markup nicht aufbrechen (macht der Serializer).
        let html = sanitize(r#"<p>Mail an x"&<b@example.de jetzt</p>"#);
        let text = extract_plaintext(&html);
        // Hand-gebautes Finding mit fiesen Zeichen im Original.
        let needle = r#"x"&"#;
        if let Some(start) = text.find(needle) {
            let f = Finding {
                entity_type: "email".into(),
                original: needle.into(),
                token: "«E_t»".into(),
                start,
                end: start + needle.len(),
                confidence: 0.9,
            };
            let out = redact(&html, &[f]);
            // Parsebar bleiben: der Serializer escapet Quotes in Attributen.
            assert!(out.annotated_html.contains("data-original"));
            assert!(!out.annotated_html.contains(r#"data-original="x"&""#));
        }
    }
}
