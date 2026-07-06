//! Schwärz-Bühne — Capture-Flow des zweiten, sichtbaren Workflows.
//!
//! Anders als der Smart-Paste-Hotkey ([`crate::hotkey`]), der unsichtbar in
//! die Ziel-App zurückpastet, holt die Bühne die aktuelle Text-Markierung per
//! synthetischem Strg+C/Cmd+C ab, schwärzt sie und **bringt das eigene Fenster
//! nach vorn**. Fokus-Übernahme ist hier ausdrücklich erwünscht — der User
//! will das Ergebnis sehen und in der Ablage behalten.
//!
//! # Warum ein paralleler Pfad statt Umbau von `hotkey.rs`
//!
//! Der bestehende Smart-Paste-Flow ist die primäre UX und bleibt unangetastet.
//! Die Bühne teilt sich nur die Low-Level-Bausteine (enigo-Sequenz-Robustheit,
//! Clipboard-Helper, Detection-Signaturen), trifft aber eigene Entscheidungen:
//! **nur Forward** (kein Reverse — die Bühne übersetzt nie zurück), Clipboard
//! wird sofort geschrieben, ein Event treibt die Frontend-Animation.
//!
//! # Testbarkeit
//!
//! Die reizvolle Logik (Fallback-Entscheidung, Größen-Cap, Segment-Bau mit
//! UTF-8-Grenzen) liegt in **reinen Funktionen** ([`classify_capture`],
//! [`build_segments`], [`aggregate_entity_counts`], [`classify_stage_hotkey`]).
//! Die enigo-/Clipboard-/Tauri-Seite ([`capture`]) ist bewusst dünn — sie ruft
//! nur OS-APIs auf und delegiert jede Entscheidung an die reinen Funktionen.

use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::detection::{self, Finding};
use crate::settings::Settings;
use crate::{secrets, storage};

/// Hartes Größen-Limit für Clipboard-Inhalte (10 MB) — identisch zum
/// Smart-Paste-Pfad. Oberhalb machen wir keine Detection, sondern zeigen einen
/// Fehler-State: Regex-Iteration über zweistellige MB-Blobs würde den Prozess
/// einfrieren. Reale PII-Texte sind selten >50 KB.
const MAX_CLIPBOARD_BYTES: usize = 10 * 1024 * 1024;

/// Eigener Cap für Bild-Rohbytes (64 MB). Der 10-MB-Text-Cap passt hier
/// nicht: Windows liefert Clipboard-Bilder als UNKOMPRIMIERTES CF_DIB —
/// ein normaler 4K-Screenshot sind ~33 MB, ein 1440p-Screenshot ~15 MB.
/// Die eigentliche Arbeit begrenzt [`crate::imaging::MAX_IMAGE_DIMENSION`]
/// (Pixel-Kanten), dieser Byte-Cap fängt nur Absurdes ab.
const MAX_IMAGE_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Poll-Intervall beim Warten auf das Ergebnis des synthetischen Copy.
const POLL_INTERVAL: Duration = Duration::from_millis(30);

/// Wartezeit auf eine Clipboard-Änderung **pro Copy-Versuch**. Bewusst kurz —
/// die Robustheit kommt aus der Wiederholung ([`COPY_ATTEMPTS`]), nicht aus
/// einem langen Einzel-Budget.
const POLL_BUDGET_PER_ATTEMPT: Duration = Duration::from_millis(450);

/// Anzahl synthetischer Copy-Versuche, bevor der Fallback auf den vorhandenen
/// Clipboard-Inhalt greift. Deckt träge Ziel-Apps ab und den Fall, dass der
/// erste Versuch von noch gedrückten Hotkey-Tasten kontaminiert wurde.
const COPY_ATTEMPTS: u32 = 3;

/// Maximale Wartezeit auf das physische Loslassen der Hotkey-Modifier, bevor
/// das synthetische Copy rausgeht (siehe [`wait_for_modifiers_released`]).
/// Nur auf Plattformen mit Hardware-Check (macOS/Windows) — sonst wäre die
/// Konstante unused (CI verbietet Warnings).
#[cfg(any(target_os = "macos", target_os = "windows"))]
const MODIFIER_RELEASE_BUDGET: Duration = Duration::from_millis(1000);

/// Anzeige-Cap: die Segmente decken maximal die ersten 8 000 **Zeichen**
/// Originaltext ab. Ablage und Clipboard enthalten immer den vollständigen
/// geschwärzten Text — der Cap betrifft nur die Bühnen-Anzeige, damit ein
/// versehentlich kopierter Riesentext das Frontend nicht lahmlegt.
const SEGMENT_CHAR_CAP: usize = 8000;

/// Anzeige-Cap für annotiertes HTML (**Bytes**, Stufe 2). Der Zeichen-Cap
/// allein reicht nicht: `data:`-Bilder bis 256 KB und schweres Markup können
/// trotz weniger Text-Zeichen ein Multi-MB-DOM erzeugen, das per `{@html}`
/// das Bühnen-Fenster einfriert. Darüber fällt NUR die Anzeige auf den
/// Plain-Preview zurück — Clipboard/Ablage behalten die volle formatierte
/// Fassung.
const ANNOTATED_HTML_DISPLAY_CAP_BYTES: usize = 1024 * 1024;

/// Ein Anzeige-Segment für die Marker-Animation. Das Backend schneidet den
/// Text selbst in Segmente, weil `Finding.start/end` **Byte**-Offsets sind,
/// das Frontend aber in UTF-16-Code-Units rechnet — Offset-Arithmetik im
/// JavaScript wäre eine Fehlerquelle. Serialisiert als intern getaggtes Enum
/// (`{"kind":"text",…}` / `{"kind":"finding",…}`), exakt nach Vertrag 2.2.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Segment {
    Text {
        content: String,
    },
    Finding {
        original: String,
        replacement: String,
        entity_type: String,
        confidence: f32,
    },
}

/// Der `stage://job`-Event-Payload (Backend → Main-Window). Wird emittiert,
/// **nachdem** Detection, Clipboard-Write und Ablage-Eintrag durch sind — das
/// Frontend animiert nur noch und trifft keine Entscheidungen mehr.
#[derive(Debug, Clone, Serialize)]
struct StageJob {
    /// `case_id` des Forwards (reversibel) bzw. eine reine Job-Referenz im
    /// Strict-Mode. Im Strict-Mode existiert **kein** Mapping — die ID dient
    /// nur der Zuordnung Event↔Log, nicht der Rückübersetzung.
    job_id: String,
    /// `"reversible"` | `"strict"`.
    mode: String,
    /// ID des bereits angelegten Ablage-Eintrags; `null` bei 0 Findings oder
    /// im Fehler-State.
    stash_id: Option<i64>,
    finding_count: usize,
    /// `true`, wenn die Anzeige-Segmente wegen des Zeichen-Caps gekürzt wurden.
    truncated: bool,
    /// Anzeige-Reihenfolge = Array-Reihenfolge.
    segments: Vec<Segment>,
    /// `"plain"` | `"html"` (Stufe 2) | `"image"` (Stufe 3). Bei `"html"`
    /// trägt `annotated_html` die Anzeige; `segments` ist dann leer — außer
    /// die Anzeige fällt wegen der Caps auf den Plain-Preview zurück
    /// (`truncated: true`). Bei `"image"` tragen `image_data_url` + `boxes`
    /// die Anzeige, `segments` den erkannten Text darunter.
    content_kind: String,
    /// Sanitisiertes HTML mit `data-sz-finding`-Spans für die
    /// Marker-Animation. Nur bei `content_kind: "html"`.
    annotated_html: Option<String>,
    /// Metadaten-freie PNG-Kopie des ORIGINALS als `data:`-URL — die Bühne
    /// zeigt sie und animiert die Balken darüber. Bewusst KEIN Temp-File:
    /// das Original berührt nie die Platte (dokumentierte Abweichung vom
    /// Konzept-Vertrag Abschnitt 5). Nur bei `content_kind: "image"`.
    image_data_url: Option<String>,
    /// Schwärz-Boxen (normiert 0–1, Ursprung oben links) für die
    /// Balken-Animation über dem Bild. Nur bei `content_kind: "image"`.
    boxes: Vec<crate::imaging::RedactionBox>,
    /// Ehrlichkeits-Flag (Vertrag Stufe 3): das Ergebnis basiert auf
    /// Texterkennung — was OCR nicht liest, bleibt sichtbar. Das UI zeigt
    /// dann den Prüf-Warnhinweis.
    ocr_based: bool,
}

/// Ergebnis der reinen Klassifikation des gecaptureten Clipboard-Texts.
/// Trennt die drei Fälle, damit sie separat geloggt werden können — das Event
/// ist für `Empty` und `TooLarge` aber identisch (Fehler-State).
#[derive(Debug, PartialEq)]
enum CaptureDecision {
    /// Brauchbarer Text → Detection fahren.
    Proceed(String),
    /// Leer oder nicht-textueller Clipboard-Inhalt → Fehler-State.
    Empty,
    /// Über dem 10-MB-Cap → Fehler-State, keine Detection (enthält die Größe
    /// nur fürs Logging).
    TooLarge(usize),
}

/// Reine Entscheidung, ob der (evtl. gecapturete) Clipboard-Text weiter durch
/// die Detection läuft. Kein OS-Zugriff — damit unit-testbar.
fn classify_capture(text: Option<String>) -> CaptureDecision {
    match text {
        Some(t) if t.is_empty() => CaptureDecision::Empty,
        Some(t) if t.len() > MAX_CLIPBOARD_BYTES => CaptureDecision::TooLarge(t.len()),
        Some(t) => CaptureDecision::Proceed(t),
        None => CaptureDecision::Empty,
    }
}

/// Der aufbereitete Bühnen-Inhalt nach der Format-Entscheidung (Stufe 2/3).
enum StageContent {
    /// Plain-Text-Flow wie in Stufe 1.
    Plain(String),
    /// HTML-Flow: `sanitized` ist das bereinigte HTML, `parsed` der bereits
    /// geparste Baum inkl. Detection-Plaintext (Offsets passen zu
    /// `richtext::redact`). `text_flavor` ist der ORIGINALE Plain-Text-Flavor
    /// der Quell-App (falls brauchbar) — er liefert den Text-Fallback für
    /// Clipboard/Ablage, damit z. B. Listennummern erhalten bleiben, die die
    /// HTML-Plaintext-Ableitung nicht synthetisiert.
    Html {
        sanitized: String,
        parsed: crate::richtext::ParsedRichText,
        text_flavor: Option<String>,
    },
    /// Bild-Flow (Stufe 3): rohe Bild-Bytes (PNG/JPEG/BMP/TIFF) aus
    /// Clipboard oder Datei-Drop — Dekodierung/OCR passieren erst im Flow,
    /// damit Fehler dort einheitlich in den Fehler-State laufen.
    Image(Vec<u8>),
}

// `ParsedRichText` hat kein `Debug`/`PartialEq` (DOM-Baum) — für Test-
// Fehlermeldungen reicht die Variante plus Textgehalt.
impl std::fmt::Debug for StageContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StageContent::Plain(t) => f.debug_tuple("Plain").field(t).finish(),
            StageContent::Html { parsed, text_flavor, .. } => f
                .debug_struct("Html")
                .field("plaintext", &parsed.plaintext())
                .field("text_flavor", text_flavor)
                .finish(),
            StageContent::Image(bytes) => f
                .debug_struct("Image")
                .field("bytes", &bytes.len())
                .finish(),
        }
    }
}

/// Entscheidet zwischen HTML-, Plain- und Bild-Flow. HTML gewinnt, wenn ein
/// HTML-Flavor anliegt, unter dem Größen-Cap bleibt und nach dem Sanitizing
/// noch **sichtbarer** Text übrig ist — der von uns injizierte
/// „[Bild entfernt]"-Platzhalter zählt nicht (ein Nur-Bild-HTML muss auf den
/// Text-Flavor zurückfallen, sonst bliebe dessen Inhalt ungescannt). Das
/// Bild greift als LETZTES: nur wenn weder Text noch HTML brauchbar sind
/// (typisch Screenshot im Clipboard — der hat gar keinen Text-Flavor).
///
/// Sanitizing passiert bewusst HIER, vor jeder weiteren Verarbeitung: ab
/// diesem Punkt existiert das Original-HTML (mit Tracking-Pixeln, Scripts,
/// href-Adressen) im Flow nicht mehr.
fn prepare_content(
    text: Option<String>,
    html: Option<String>,
    image: Option<Vec<u8>>,
) -> Result<StageContent, CaptureDecision> {
    let (text_flavor, text_error) = match classify_capture(text) {
        CaptureDecision::Proceed(t) => (Some(t), None),
        other => (None, Some(other)),
    };

    if let Some(raw_html) = html {
        if !raw_html.is_empty() && raw_html.len() <= MAX_CLIPBOARD_BYTES {
            let sanitized = crate::richtext::sanitize(&raw_html);
            let parsed = crate::richtext::parse_sanitized(&sanitized);
            if crate::richtext::has_visible_text(parsed.plaintext()) {
                return Ok(StageContent::Html {
                    sanitized,
                    parsed,
                    text_flavor,
                });
            }
            log::info!(
                "stage: HTML-Flavor ohne sichtbaren Text (nur Bilder?) — Fallback auf Plain-Text"
            );
        } else if raw_html.len() > MAX_CLIPBOARD_BYTES {
            log::info!(
                "stage: HTML-Flavor {} Bytes über Cap — Fallback auf Plain-Text",
                raw_html.len()
            );
        }
    }
    if let Some(t) = text_flavor {
        return Ok(StageContent::Plain(t));
    }
    // Invariante aus Stufe 1: ein ÜBERGROSSER Text-Flavor endet IMMER im
    // TooLarge-Fehler-State — ein Bild-Flavor daneben (Excel/Word legen
    // Bitmap-Renderings dazu) darf ihn nicht still verdrängen, sonst wird
    // der eigentlich kopierte Text durch ein geschwärztes PNG ersetzt.
    if let Some(CaptureDecision::TooLarge(n)) = text_error {
        return Err(CaptureDecision::TooLarge(n));
    }
    if let Some(bytes) = image {
        if !bytes.is_empty() && bytes.len() <= MAX_IMAGE_INPUT_BYTES {
            return Ok(StageContent::Image(bytes));
        }
        if bytes.len() > MAX_IMAGE_INPUT_BYTES {
            return Err(CaptureDecision::TooLarge(bytes.len()));
        }
    }
    Err(text_error.unwrap_or(CaptureDecision::Empty))
}

/// Erkennt, ob sich der Clipboard-Inhalt gegenüber `prev` geändert hat — die
/// Bedingung, unter der der synthetische Copy als „erfolgreich" gilt.
///
/// Änderung = jetzt liegt nicht-leerer Text da, **und** entweder war vorher
/// nichts/leeres da oder der Text unterscheidet sich vom vorherigen. Reine
/// Funktion, damit die Poll-Bedingung ohne echtes Clipboard testbar ist.
fn is_clipboard_change(prev: &Option<String>, current: &Option<String>) -> bool {
    match current {
        Some(cur) if !cur.is_empty() => match prev {
            Some(p) => cur != p,
            None => true,
        },
        _ => false,
    }
}

/// Aggregiert die Findings zu `entity_type → Anzahl` (snake_case-Werte wie in
/// `detection`, z. B. `"person"`, `"iban"`). Reine Funktion.
fn aggregate_entity_counts(findings: &[Finding]) -> std::collections::HashMap<String, u32> {
    let mut counts = std::collections::HashMap::new();
    for f in findings {
        *counts.entry(f.entity_type.clone()).or_insert(0) += 1;
    }
    counts
}

/// Schneidet `text` an den Finding-Grenzen in Anzeige-Segmente und liefert
/// zusätzlich, ob wegen des Zeichen-Caps gekürzt wurde.
///
/// # UTF-8 vs. Zeichen
///
/// `Finding.start/end` sind **Byte**-Offsets. Das Slicing (`text[..]`) läuft
/// daher byte-basiert — das ist safe, weil die Findings garantiert an
/// Zeichengrenzen liegen (Detection liefert sie so). Der 8 000er-Cap zählt
/// dagegen **Zeichen** (`chars()`), damit Umlaute/Emoji die Anzeige nicht je
/// nach Byte-Breite unterschiedlich früh kappen.
///
/// Findings werden nach `start` sortiert und überlappungsfrei erwartet
/// (Detection-Garantie). Defensiv werden dennoch Findings übersprungen, die
/// rückwärts zeigen oder über das Textende hinausragen — das verhindert Panics
/// bei fehlerhaften Offsets, statt die ganze Bühne abstürzen zu lassen.
fn build_segments(text: &str, findings: &[Finding]) -> (Vec<Segment>, bool) {
    let mut sorted: Vec<&Finding> = findings.iter().collect();
    sorted.sort_by_key(|f| f.start);

    let mut segments = Vec::new();
    let mut used_chars = 0usize; // bereits belegte Anzeige-Zeichen
    let mut pos = 0usize; // Byte-Cursor im Originaltext
    let mut truncated = false;

    // Hängt so viel von `chunk` an, wie der Cap noch zulässt. Gibt `true`
    // zurück, wenn der Cap dabei überschritten (= gekürzt) wurde.
    fn push_capped(segments: &mut Vec<Segment>, used_chars: &mut usize, chunk: &str) -> bool {
        let chunk_chars = chunk.chars().count();
        if *used_chars + chunk_chars <= SEGMENT_CHAR_CAP {
            if !chunk.is_empty() {
                segments.push(Segment::Text {
                    content: chunk.to_string(),
                });
            }
            *used_chars += chunk_chars;
            false
        } else {
            let remaining = SEGMENT_CHAR_CAP - *used_chars;
            let partial: String = chunk.chars().take(remaining).collect();
            if !partial.is_empty() {
                segments.push(Segment::Text { content: partial });
            }
            *used_chars = SEGMENT_CHAR_CAP;
            true
        }
    }

    for f in sorted {
        // Defensive Offset-Prüfung — überlappend/rückwärts/außerhalb → skip.
        if f.start < pos || f.end > text.len() || f.start > f.end {
            continue;
        }

        // Text vor dem Finding.
        let before = &text[pos..f.start];
        if push_capped(&mut segments, &mut used_chars, before) {
            truncated = true;
            break;
        }

        // Das Finding selbst ist atomar — passt es nicht mehr komplett in den
        // Cap, brechen wir davor ab (kein halbes Finding anzeigen).
        let orig = &text[f.start..f.end];
        let orig_chars = orig.chars().count();
        if used_chars + orig_chars > SEGMENT_CHAR_CAP {
            truncated = true;
            break;
        }
        segments.push(Segment::Finding {
            original: f.original.clone(),
            replacement: f.token.clone(),
            entity_type: f.entity_type.clone(),
            confidence: f.confidence,
        });
        used_chars += orig_chars;
        pos = f.end;
    }

    // Rest hinter dem letzten Finding (nur wenn nicht ohnehin schon gekürzt).
    if !truncated {
        let tail = &text[pos..];
        if push_capped(&mut segments, &mut used_chars, tail) {
            truncated = true;
        }
    }

    if truncated {
        segments.push(Segment::Text {
            content: "… [gekürzt]".to_string(),
        });
    }

    (segments, truncated)
}

/// Entscheidung, ob der Capture-Hotkey überhaupt registriert wird. Rein, damit
/// die Robustheits-Regeln (leer/Kollision) ohne Tauri testbar sind. Die
/// **Parsebarkeit** prüft erst die Registrierung selbst (`register` liefert
/// `Err`) — ein Fehlversuch schaltet das Feature aus, ohne den Smart-Paste-
/// Hotkey zu berühren.
#[derive(Debug, PartialEq)]
enum StageHotkeyDecision {
    /// Leerer String → Feature bewusst aus.
    Disabled,
    /// Identisch zum Smart-Paste-Hotkey → nicht registrierbar, Feature aus.
    Conflict,
    /// Registrierung versuchen.
    Enabled,
}

fn classify_stage_hotkey(stage: &str, smart: &str) -> StageHotkeyDecision {
    if stage.is_empty() {
        StageHotkeyDecision::Disabled
    } else if stage == smart {
        StageHotkeyDecision::Conflict
    } else {
        StageHotkeyDecision::Enabled
    }
}

/// Loggt die Hotkey-Entscheidung und registriert den Capture-Hotkey, wenn er
/// zulässig ist. Wird aus der `setup()`-Phase in `main.rs` gerufen. Der
/// bestehende Smart-Paste-Hotkey ist zu diesem Zeitpunkt bereits registriert —
/// diese Funktion darf ihn unter keinen Umständen beeinträchtigen, deshalb
/// werden alle Fehlerpfade nur geloggt (kein `?`, kein Panic).
pub fn register_stage_hotkey(app: &AppHandle, stage_hotkey: &str, smart_hotkey: &str) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    match classify_stage_hotkey(stage_hotkey, smart_hotkey) {
        StageHotkeyDecision::Disabled => {
            log::info!("stage: kein Capture-Hotkey konfiguriert (leer) — Bühne per Hotkey aus");
        }
        StageHotkeyDecision::Conflict => {
            log::warn!(
                "stage: Capture-Hotkey '{stage_hotkey}' == Smart-Paste-Hotkey — Bühne per Hotkey \
                 aus (Smart-Paste bleibt aktiv)"
            );
        }
        StageHotkeyDecision::Enabled => match app.global_shortcut().register(stage_hotkey) {
            Ok(()) => log::info!("registered stage hotkey: {stage_hotkey}"),
            Err(e) => log::warn!(
                "stage: Capture-Hotkey '{stage_hotkey}' nicht registrierbar ({e}) — Bühne per \
                 Hotkey aus (Smart-Paste bleibt aktiv)"
            ),
        },
    }
}

/// Wartet nach dem synthetischen Copy auf eine Clipboard-Änderung. Liefert den
/// geänderten Text, sobald er auftaucht, sonst `None` nach Ablauf des Budgets.
fn poll_for_change(prev: &Option<String>, budget: Duration) -> Option<String> {
    let start = Instant::now();
    while start.elapsed() < budget {
        std::thread::sleep(POLL_INTERVAL);
        let current = crate::clipboard::read_clipboard_text();
        if is_clipboard_change(prev, &current) {
            return current;
        }
    }
    None
}

/// Wartet, bis der User die physischen Hotkey-Modifier (Cmd/Ctrl/Alt/Shift)
/// losgelassen hat — maximal [`MODIFIER_RELEASE_BUDGET`].
///
/// Der Capture-Hotkey hat drei Modifier. Sind sie beim synthetischen Copy noch
/// gedrückt, kombiniert das OS sie auf HID-Ebene mit dem injizierten Event —
/// die Ziel-App sieht dann `Cmd+Option+Shift+C` statt `Cmd+C` und kopiert
/// nichts. Die synthetischen Release-Events in [`send_copy`] neutralisieren
/// physisch gehaltene Tasten NICHT zuverlässig (Beta-Befund macOS: enigo
/// meldet Ok, das Clipboard ändert sich nie). Deshalb wird hier der echte
/// Hardware-Zustand abgefragt statt blind zu schlafen.
#[cfg(target_os = "macos")]
fn wait_for_modifiers_released() {
    use objc2_app_kit::NSEvent;
    // NSEventModifierFlags-Bits: Shift 1<<17, Control 1<<18, Option 1<<19,
    // Command 1<<20 (AppKit-Konstanten, stabil seit OS X 10.0).
    const MODIFIER_MASK: usize = (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20);
    let start = Instant::now();
    while start.elapsed() < MODIFIER_RELEASE_BUDGET {
        let flags = unsafe { NSEvent::modifierFlags_class() };
        if flags.0 & MODIFIER_MASK == 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    log::warn!(
        "stage: Modifier nach {} ms noch gedrückt — Copy wird trotzdem versucht",
        MODIFIER_RELEASE_BUDGET.as_millis()
    );
}

/// Windows-Pendant: echter Hardware-Zustand via `GetAsyncKeyState` — Bit 15
/// meldet „Taste ist JETZT gedrückt". Gleiche Begründung wie auf macOS: noch
/// gehaltene Hotkey-Modifier kombinieren sich mit dem injizierten Strg+C,
/// die Ziel-App sieht dann `Strg+Alt+Shift+C` und kopiert nichts.
#[cfg(target_os = "windows")]
fn wait_for_modifiers_released() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };

    // SAFETY: GetAsyncKeyState ist ein parameterloser Zustands-Read pro
    // Virtual-Key, von beliebigen Threads aufrufbar.
    let any_modifier_down = || {
        [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
            .iter()
            .any(|vk| unsafe { (GetAsyncKeyState(vk.0 as i32) as u16) & 0x8000 != 0 })
    };

    let start = Instant::now();
    while start.elapsed() < MODIFIER_RELEASE_BUDGET {
        if !any_modifier_down() {
            return;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    log::warn!(
        "stage: Modifier nach {} ms noch gedrückt — Copy wird trotzdem versucht",
        MODIFIER_RELEASE_BUDGET.as_millis()
    );
}

/// Andere Plattformen (Linux): kein billiger Hardware-Zustands-Check
/// verfügbar. Fester Puffer — die Retry-Schleife in [`capture`] übernimmt
/// die Robustheit.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn wait_for_modifiers_released() {
    std::thread::sleep(Duration::from_millis(150));
}

/// Diagnose fürs „Copy kommt nicht an"-Debugging (Beta-Befund: enigo meldet
/// Ok, Clipboard ändert sich nie). Loggt die zwei entscheidenden Fakten:
///
/// - **`AXIsProcessTrusted`**: Ohne Accessibility-Vertrauen verwirft macOS
///   gepostete Tastatur-Events STILLSCHWEIGEND — CGEventPost liefert keinen
///   Fehler. Im Dev-Modus gehört der Prozess zur Koalition des Editors
///   (z. B. Zed), dessen Berechtigung dann zählt, nicht die der App.
/// - **Frontmost App**: Geht das Cmd+C an die falsche App (weil der Panel-
///   Klick doch aktiviert hat), steht hier Streichzeug statt der Quell-App.
#[cfg(target_os = "macos")]
fn log_capture_diagnostics() {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    let trusted = unsafe { AXIsProcessTrusted() };
    let front =
        crate::foreground::current_process_name().unwrap_or_else(|| "unbekannt".to_string());
    log::info!("stage: diagnostics — AXIsProcessTrusted={trusted}, frontmost='{front}'");
    if !trusted {
        log::warn!(
            "stage: Prozess hat KEINE Bedienungshilfen-Berechtigung — synthetische \
             Tastendrücke werden von macOS verworfen. Im Dev-Modus braucht der \
             startende Editor/Terminal die Berechtigung (Systemeinstellungen → \
             Datenschutz & Sicherheit → Bedienungshilfen)."
        );
    }
}

#[cfg(not(target_os = "macos"))]
fn log_capture_diagnostics() {}

/// Der öffentliche Einstieg: wird vom Global-Shortcut-Handler gerufen, sobald
/// der Capture-Hotkey gedrückt wurde. Führt Vertrag 2.5 Schritt 1–9 aus.
///
/// Bewusst dünn: jede Entscheidung liegt in einer reinen Funktion; hier bleibt
/// nur die OS-Choreografie (enigo, Clipboard, Detection-Aufrufe, Event, Fenster).
pub fn capture(app: &AppHandle) {
    log::info!("stage: capture-hotkey pressed");
    log_capture_diagnostics();

    // 1. Bisherigen Clipboard-Inhalt merken.
    let prev = crate::clipboard::read_clipboard_text();

    // 2./3. Synthetisches Strg+C / Cmd+C mit Retry. Vor jedem Versuch auf das
    // physische Loslassen der Hotkey-Modifier warten — sonst kontaminieren
    // die noch gedrückten Tasten das injizierte Event und die Ziel-App sieht
    // keinen Copy-Befehl. Mehrere kurze Versuche schlagen ein langes
    // Einzel-Budget: sie heilen auch träge Ziel-Apps und Timing-Pech.
    let mut captured: Option<String> = None;
    for attempt in 1..=COPY_ATTEMPTS {
        wait_for_modifiers_released();
        if let Err(e) = send_copy() {
            // Kein Abbruch: der Fallback nutzt notfalls den vorhandenen
            // Clipboard-Inhalt — „erst normal kopieren, dann Hotkey" bleibt
            // funktionsfähig, auch wenn der synthetische Copy nie durchkommt.
            log::warn!("stage: synthetic copy failed ({e}) — Versuch {attempt}/{COPY_ATTEMPTS}");
        }
        captured = poll_for_change(&prev, POLL_BUDGET_PER_ATTEMPT);
        if captured.is_some() {
            break;
        }
        log::info!("stage: Versuch {attempt}/{COPY_ATTEMPTS} ohne Clipboard-Änderung");
    }

    // 4. Fallback: vorhandenen Inhalt verwenden.
    let text_to_use = match captured {
        Some(t) => Some(t),
        None => {
            log::info!("stage: keine Clipboard-Änderung — Fallback auf vorhandenen Inhalt");
            crate::clipboard::read_clipboard_text()
        }
    };

    // Stufe 2/3: HTML- und Bild-Flavor mitnehmen, wenn die Quelle sie
    // liefert (Word/Outlook/Browser legen HTML neben den Plain-Text;
    // Screenshots liegen NUR als Bild da). Die Flavors werden nur
    // übernommen, wenn das Clipboard noch DENSELBEN Text trägt wie beim
    // Capture — sonst hat ein Clipboard-Manager/Sync-Tool den Inhalt
    // zwischen Poll-Erfolg und Flavor-Read ersetzt, und wir würden fremden
    // Inhalt schwärzen statt der Markierung des Users.
    //
    // Ohne jeglichen Text-Flavor (None) gibt es keinen Poll-Anker. Dann
    // wird NUR der Bild-Flavor gelesen — das ist die Bild-Entsprechung des
    // dokumentierten Stufe-1-Fallbacks „erst kopieren, dann Hotkey"
    // (Screenshot → Hotkey), und die Bühne ZEIGT das verarbeitete Bild,
    // der User sieht also sofort, was geschwärzt wurde. Der HTML-Flavor
    // wird hier bewusst NICHT gelesen: ein HTML-only-Clipboard ohne
    // Text-Flavor ist kein realer Nutzer-Flow, sondern fast sicher ein
    // veraltetes Fragment — das soll im Fehler-State enden statt still
    // geschwärzt zu werden.
    let (html, image) = match &text_to_use {
        Some(t)
            if crate::clipboard::read_clipboard_text().as_deref() == Some(t.as_str()) =>
        {
            (crate::clipboard::read_clipboard_html(), None)
        }
        Some(_) => {
            log::info!(
                "stage: Clipboard-Text hat sich seit dem Capture geändert — Flavors ignoriert"
            );
            (None, None)
        }
        None => (None, crate::clipboard::read_clipboard_image()),
    };
    run_stage(app, text_to_use, html, image);
}

/// Einstieg ohne synthetischen Copy: schwärzt den **aktuellen** Clipboard-
/// Inhalt. Für klickbare Einstiege (Tray-Menü, künftig Dock-Menü), bei denen
/// die Markierung nicht abgeholt werden kann — der Klick selbst nimmt der
/// Quell-App bereits den Fokus, ein synthetisches Strg+C liefe ins Leere.
pub fn capture_from_clipboard(app: &AppHandle) {
    log::info!("stage: capture from clipboard (Menü-/Button-Einstieg)");
    run_stage(
        app,
        crate::clipboard::read_clipboard_text(),
        crate::clipboard::read_clipboard_html(),
        crate::clipboard::read_clipboard_image(),
    );
}

/// Einstieg mit direkt übergebenem Text: für den Drag-&-Drop-Weg, bei dem der
/// User eine Markierung ins Fenster zieht — der Text kommt im Drop-Event mit,
/// ohne Clipboard und ohne synthetische Tastendrücke.
pub fn capture_from_text(app: &AppHandle, text: String) {
    log::info!("stage: capture from dropped text ({} bytes)", text.len());
    run_stage(app, Some(text), None, None);
}

/// Einstieg mit direkt übergebenen Bild-Bytes: für den Datei-Drop einer
/// Bilddatei ins Fenster (HTML5 `dataTransfer.files` — `dragDropEnabled`
/// ist am Main-Window aus, s. Konzept WP-J).
pub fn capture_from_image(app: &AppHandle, bytes: Vec<u8>) {
    log::info!("stage: capture from dropped image ({} bytes)", bytes.len());
    run_stage(app, None, None, Some(bytes));
}

/// Gemeinsamer Kern hinter allen Einstiegen: klassifizieren, schwärzen,
/// Ablage, Event, Fenster. Entspricht Vertrag 2.5 ab Schritt 5; Stufe 2
/// zweigt bei vorhandenem HTML-Flavor in den formatierten Flow ab, Stufe 3
/// bei Bild-Inhalten in den OCR-Flow.
fn run_stage(
    app: &AppHandle,
    text_to_use: Option<String>,
    html_to_use: Option<String>,
    image_to_use: Option<Vec<u8>>,
) {
    // 5. Format-Entscheidung + Klassifikation: leer/zu groß → Fehler-State.
    let content = match prepare_content(text_to_use, html_to_use, image_to_use) {
        Ok(c) => c,
        Err(CaptureDecision::TooLarge(n)) => {
            log::warn!(
                "stage: Clipboard {n} Bytes über {MAX_CLIPBOARD_BYTES}-Byte-Limit — Fehler-State"
            );
            emit_error_state(app);
            return;
        }
        Err(_) => {
            log::info!("stage: nichts Brauchbares im Clipboard — Fehler-State");
            emit_error_state(app);
            return;
        }
    };

    let settings = Settings::load();

    // Stufe 3: Bild-Flow ist eigenständig (OCR statt Text-Flavors).
    if let StageContent::Image(bytes) = content {
        run_stage_image(app, &bytes, &settings);
        return;
    }

    // 6. Detection — **nur Forward** (die Bühne macht kein Reverse). Beim
    // HTML-Flow läuft die Detection auf dem extrahierten Plaintext; die
    // Offsets passen damit zu `richtext::redact`.
    let (detection_text, html_ctx) = match content {
        StageContent::Plain(t) => (t, None),
        StageContent::Html {
            sanitized,
            parsed,
            text_flavor,
        } => (
            parsed.plaintext().to_string(),
            Some((sanitized, parsed, text_flavor)),
        ),
        StageContent::Image(_) => unreachable!("oben behandelt"),
    };
    let (mode, job_id, findings, redacted) = run_forward(&detection_text, &settings);
    let finding_count = findings.len();

    // Anzeige-Aufbereitung je Format. HTML: die Marker-Animation läuft über
    // dem annotierten Dokument — außer Text ODER Markup sprengen die
    // Anzeige-Caps, dann fällt NUR die Anzeige auf den (gekappten)
    // Plain-Preview zurück; Clipboard und Ablage behalten die volle
    // formatierte Fassung. Der Text-Fallback fürs Clipboard kommt aus dem
    // ORIGINALEN Plain-Text-Flavor der Quelle (geschwärzt mit derselben
    // case_id → identische Tokens), damit z. B. Listennummern erhalten
    // bleiben; nur wenn keiner anliegt, dient der HTML-Plaintext als Ersatz.
    let (content_kind, annotated_html, segments, truncated, redacted_html, fallback_text) =
        match html_ctx {
            None => {
                let (segments, truncated) = build_segments(&detection_text, &findings);
                ("plain", None, segments, truncated, None, redacted)
            }
            Some((sanitized, parsed, text_flavor)) => {
                let rich = crate::richtext::redact(parsed, &sanitized, &findings);
                let redacted_html =
                    finalize_redacted_html(rich.redacted_html, mode, finding_count);
                let fallback_text = match text_flavor {
                    Some(source_text) => {
                        redact_text_flavor(&source_text, mode == "strict", &job_id)
                    }
                    None => redacted,
                };
                if html_display_fits(&detection_text, &rich.annotated_html) {
                    (
                        "html",
                        Some(rich.annotated_html),
                        Vec::new(),
                        false,
                        Some(redacted_html),
                        fallback_text,
                    )
                } else {
                    let (segments, _) = build_segments(&detection_text, &findings);
                    (
                        "html",
                        None,
                        segments,
                        true,
                        Some(redacted_html),
                        fallback_text,
                    )
                }
            }
        };

    // 7./8. Bei ≥ 1 Finding: geschwärztes Ergebnis sofort ins Clipboard und
    // in die Ablage. Bei 0 Findings (Schritt 9): Clipboard unverändert.
    let stash_id = if finding_count >= 1 {
        let write_result = match &redacted_html {
            Some(html) => crate::clipboard::write_clipboard_html(html, &fallback_text)
                // Rich-Write fehlgeschlagen → wenigstens den Text liefern.
                .or_else(|e| {
                    log::warn!("stage: rich clipboard write failed ({e}) — Text-Fallback");
                    crate::clipboard::write_clipboard_text(&fallback_text)
                }),
            None => crate::clipboard::write_clipboard_text(&fallback_text),
        };
        if let Err(e) = write_result {
            // Clipboard-Write ist best effort — die Ablage/Anzeige stimmt
            // trotzdem, der User kann später „Nochmal kopieren" nutzen.
            log::warn!("stage: clipboard write failed: {e}");
        }
        let counts = aggregate_entity_counts(&findings);
        // `stash_insert_rich` normalisiert/kürzt den Titel selbst — wir reichen
        // den geschwärzten Text sowohl als Titel-Quelle als auch als Volltext.
        let id = storage::stash_insert_rich(
            mode,
            &fallback_text,
            &fallback_text,
            &counts,
            content_kind,
            redacted_html.as_deref(),
        );
        log::info!(
            "stage: {finding_count} Finding(s) geschwärzt, Ablage-Eintrag #{id} \
             (mode={mode}, kind={content_kind})"
        );
        Some(id)
    } else {
        log::info!("stage: keine personenbezogenen Daten gefunden — Clipboard unverändert");
        None
    };

    // 8./9. Event ans Main-Window, dann Fenster vorholen.
    let payload = StageJob {
        job_id,
        mode: mode.to_string(),
        stash_id,
        finding_count,
        truncated,
        segments,
        content_kind: content_kind.to_string(),
        annotated_html,
        image_data_url: None,
        boxes: Vec::new(),
        ocr_based: false,
    };
    emit_and_show(app, &payload);
}

/// Bild-Flow (Stufe 3, Vertrag Abschnitt 5): metadaten-freie PNG-Kopie,
/// lokale OS-OCR, Detection auf dem erkannten Text, Balken-Boxen, geschwärztes
/// PNG in Clipboard + Ablage. Die Anzeige bekommt das ORIGINAL als data:-URL
/// plus die Boxen — die Balken-Animation läuft im Frontend darüber.
fn run_stage_image(app: &AppHandle, bytes: &[u8], settings: &Settings) {
    // Dekodieren (mit Dimensions-Guard VOR dem Pixel-Decode) + Re-Encode:
    // einheitliches PNG, EXIF/GPS/XMP sind weg.
    let img = match crate::imaging::decode_image(bytes) {
        Ok(i) => i,
        Err(e) => {
            log::warn!("stage: Bild unbrauchbar ({e}) — Fehler-State");
            emit_error_state(app);
            return;
        }
    };
    let full_png = match crate::imaging::encode_png(&img) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("stage: Re-Encode fehlgeschlagen ({e}) — Fehler-State");
            emit_error_state(app);
            return;
        }
    };

    // Lokale OS-Texterkennung (Apple Vision / Windows.Media.Ocr) — auf der
    // VOLLEN Auflösung, die Genauigkeit soll nicht an der Anzeige hängen.
    let words = match crate::ocr::recognize(&full_png) {
        Ok(w) => w,
        Err(e) => {
            log::warn!("stage: Texterkennung fehlgeschlagen ({e}) — Fehler-State");
            emit_error_state(app);
            return;
        }
    };
    log::info!("stage: OCR hat {} Wörter erkannt", words.len());

    let (ocr_text, spans) = crate::imaging::assemble_text(&words);
    let (mode, job_id, findings, redacted) = run_forward(&ocr_text, settings);
    let finding_count = findings.len();
    let boxes = crate::imaging::map_findings_to_boxes(&findings, &words, &spans);
    let (segments, truncated) = build_segments(&ocr_text, &findings);

    // Bei ≥ 1 Finding: geschwärztes PNG rendern → Clipboard (Bild + Text)
    // und Ablage (PNG-Datei + Eintrag). Bei 0 Findings: nichts anfassen.
    let stash_id = if finding_count >= 1 {
        stash_redacted_image(img.clone(), &boxes, mode, &redacted, &findings, &job_id)
    } else {
        log::info!(
            "stage: keine personenbezogenen Daten im erkannten Text — Clipboard unverändert"
        );
        None
    };

    // Anzeige-Kopie GEDECKELT: für die Bühne reicht Bildschirm-Auflösung —
    // ein verlustfreies Re-Encode eines großen Fotos kann sonst zig MB
    // erreichen, und base64 (+33 %) davon würde IPC und Webview würgen.
    // Die Boxen sind normiert und bleiben von der Skalierung unberührt;
    // Clipboard/Ablage behalten die volle Auflösung.
    let display_png = match crate::imaging::encode_display_png(&img) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("stage: Anzeige-Encode fehlgeschlagen ({e}) — Fehler-State");
            emit_error_state(app);
            return;
        }
    };

    use base64::Engine as _;
    let payload = StageJob {
        job_id,
        mode: mode.to_string(),
        stash_id,
        finding_count,
        truncated,
        segments,
        content_kind: "image".to_string(),
        annotated_html: None,
        image_data_url: Some(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&display_png)
        )),
        boxes,
        ocr_based: true,
    };
    emit_and_show(app, &payload);
}

/// Rendert die Balken ins Bild, schreibt das Ergebnis ins Clipboard
/// (Bild + Text-Fallback) und legt es als PNG-Datei + Ablage-Eintrag ab.
/// Fehlerpfade nur loggen — Clipboard und Ablage sind unabhängig nützlich.
/// Fehler-Semantik richtet sich danach, ob das Clipboard schon beschrieben
/// wurde: VOR dem Clipboard-Write (Encode-Fehler) → `None`, die Bühne sagt
/// dann zu Recht „Clipboard unverändert". NACH dem Clipboard-Write
/// (Stash-Pfad/-Write) → `Some(-1)`, gleiche Semantik wie `stash_insert`
/// im Text-Flow — `stash_id: null` würde die Bühne sonst in den „Keine
/// Daten gefunden — Clipboard unverändert"-Zustand schicken, der dann
/// doppelt falsch wäre (PII gefunden UND Clipboard ersetzt).
fn stash_redacted_image(
    mut img: image::RgbaImage,
    boxes: &[crate::imaging::RedactionBox],
    mode: &str,
    redacted_text: &str,
    findings: &[Finding],
    job_id: &str,
) -> Option<i64> {
    crate::imaging::render_redactions(&mut img, boxes);
    let redacted_png = match crate::imaging::encode_png(&img) {
        Ok(p) => p,
        Err(e) => {
            // Clipboard noch unangetastet — None ist hier die ehrliche Antwort.
            log::warn!("stage: Redaction-Encode fehlgeschlagen ({e})");
            return None;
        }
    };

    // Clipboard zuerst (Vertrag: Ergebnis liegt sofort im Clipboard).
    if let Err(e) = crate::clipboard::write_clipboard_image(&redacted_png, redacted_text) {
        log::warn!("stage: clipboard image write failed: {e}");
    }

    let path = match storage::stash_image_file_path(job_id) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("stage: {e}");
            return Some(-1);
        }
    };
    if let Err(e) = std::fs::write(&path, &redacted_png) {
        log::warn!("stage: stash-PNG nicht geschrieben ({e})");
        return Some(-1);
    }
    let counts = aggregate_entity_counts(findings);
    let id = storage::stash_insert_image(
        mode,
        redacted_text,
        redacted_text,
        &counts,
        &path.to_string_lossy(),
    );
    log::info!(
        "stage: {} Finding(s) im Bild geschwärzt, Ablage-Eintrag #{id} (mode={mode})",
        findings.len()
    );
    Some(id)
}

/// Reine Anzeige-Entscheidung: passt das annotierte Dokument in die Bühne,
/// oder muss die Anzeige auf den Plain-Preview zurückfallen? Beide Caps
/// zählen — Text-Zeichen (Animations-/Segment-Last) UND HTML-Bytes
/// (DOM-/Rendering-Last durch data:-Bilder und Markup).
fn html_display_fits(plaintext: &str, annotated_html: &str) -> bool {
    plaintext.chars().count() <= SEGMENT_CHAR_CAP
        && annotated_html.len() <= ANNOTATED_HTML_DISPLAY_CAP_BYTES
}

/// Schwärzt den ORIGINALEN Plain-Text-Flavor der Quelle als Text-Fallback für
/// Clipboard/Ablage. Reversibel läuft die Detection mit derselben `case_id`
/// wie der HTML-Pfad — die HMAC-Tokens sind case-deterministisch, dieselbe
/// Fundstelle bekommt in beiden Flavors dasselbe Pseudonym.
fn redact_text_flavor(text: &str, strict: bool, case_id: &str) -> String {
    if strict {
        let findings = detection::detect_strict(text);
        detection::apply_strict_with_hint(text, &findings)
    } else {
        let findings = detection::detect_with_case(text, case_id);
        for f in &findings {
            storage::record(case_id, &f.token, &f.original);
        }
        detection::apply_tokens_with_hint(text, &findings, case_id)
    }
}

/// Hängt den LLM-Hinweis als HTML-Absatz an die geschwärzte formatierte
/// Fassung — das Pendant zu `apply_*_with_hint` im Text-Pfad (dort steckt der
/// Hinweis bereits im Text-Fallback). Ohne Findings bleibt das HTML pur.
fn finalize_redacted_html(redacted_html: String, mode: &str, finding_count: usize) -> String {
    if finding_count == 0 {
        return redacted_html;
    }
    let hint = if mode == "strict" {
        detection::LLM_HINT_STRICT
    } else {
        detection::LLM_HINT
    };
    let body = hint.trim_start().trim_start_matches("---").trim_start();
    format!("{redacted_html}<hr><p>{}</p>", html_escape_text(body))
}

/// Minimales HTML-Escaping für Text, der in Markup eingebettet wird.
fn html_escape_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Führt den Forward-Detection-Pfad aus und liefert
/// `(mode, job_id, findings, redacted_text)`.
///
/// - **Strict-Mode:** `detect_strict` + `apply_strict_with_hint`, **kein**
///   Mapping. `job_id` ist nur eine Referenz (kein Mapping), daher genügt eine
///   frische `new_case_id()`.
/// - **Reversibel:** frische `case_id`, `detect_with_case`, pro Finding
///   `storage::record`, `apply_tokens_with_hint`. Die `case_id` ist auch die
///   `job_id`.
fn run_forward(text: &str, settings: &Settings) -> (&'static str, String, Vec<Finding>, String) {
    if settings.strict_mode {
        let findings = detection::detect_strict(text);
        let redacted = detection::apply_strict_with_hint(text, &findings);
        // Reine Job-Referenz — im Strict-Mode existiert bewusst kein Mapping,
        // die ID verknüpft nichts Rückführbares.
        ("strict", secrets::new_case_id(), findings, redacted)
    } else {
        let case_id = secrets::new_case_id();
        let findings = detection::detect_with_case(text, &case_id);
        for f in &findings {
            storage::record(&case_id, &f.token, &f.original);
        }
        let redacted = detection::apply_tokens_with_hint(text, &findings, &case_id);
        ("reversible", case_id, findings, redacted)
    }
}

/// Emittiert den Fehler-State (leeres/zu großes Clipboard): leere Segmente,
/// 0 Findings, keine Ablage — und holt das Fenster trotzdem nach vorn, damit
/// der User Feedback bekommt (statt eines wirkungslosen Hotkeys).
fn emit_error_state(app: &AppHandle) {
    let mode = if Settings::load().strict_mode {
        "strict"
    } else {
        "reversible"
    };
    let payload = StageJob {
        job_id: secrets::new_case_id(),
        mode: mode.to_string(),
        stash_id: None,
        finding_count: 0,
        truncated: false,
        segments: Vec::new(),
        content_kind: "plain".to_string(),
        annotated_html: None,
        image_data_url: None,
        boxes: Vec::new(),
        ocr_based: false,
    };
    emit_and_show(app, &payload);
}

/// Sendet den `stage://job`-Event ans Main-Window und holt das Fenster nach
/// vorn (`show` + `unminimize` + `set_focus`). Fokus-Übernahme ist bei der
/// Bühne gewollt.
fn emit_and_show(app: &AppHandle, payload: &StageJob) {
    if let Err(e) = app.emit_to("main", "stage://job", payload) {
        log::warn!("stage: emit stage://job failed: {e}");
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    } else {
        log::warn!("stage: kein main-Window zum Anzeigen gefunden");
    }
}

/// Simuliert Strg+C (Cmd+C auf macOS), um die aktuelle Markierung der aktiven
/// App ins Clipboard zu holen.
///
/// **MUSS auf dem Main-Thread laufen** (gilt damit für jeden Aufrufer von
/// [`capture`]): enigo ruft auf macOS `TSMGetInputSourceProperty` auf, das
/// per `dispatch_assert_queue` die Main-Queue erzwingt — von einem anderen
/// Thread aus bricht der Prozess mit SIGTRAP ab (Crash-Report 2026-07-05,
/// macOS 26). Der Global-Shortcut-Handler läuft auf dem Main-Thread; Commands
/// müssen über `run_on_main_thread` einreihen (siehe `stage_capture` in
/// main.rs).
///
/// Gleiche Robustheits-Sequenz wie `hotkey::send_paste`: alle Modifier
/// explizit releasen (damit kein noch gehaltener Strg/Shift/Alt die Sequenz
/// kontaminiert), jedes enigo-Result loggen (damit im Tester-Log sichtbar
/// ist, ob der Copy überhaupt durchkam oder vom OS/EDR abgelehnt wurde).
/// Das Warten auf das physische Loslassen der Hotkey-Tasten erledigt der
/// Aufrufer über [`wait_for_modifiers_released`] — hier nur ein kurzer
/// Settle-Puffer.
fn send_copy() -> Result<(), String> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings as EnigoSettings};

    std::thread::sleep(Duration::from_millis(30));

    let mut enigo = Enigo::new(&EnigoSettings::default()).map_err(|e| {
        log::warn!("stage: enigo init failed: {e:?}");
        format!("{e:?}")
    })?;

    // Belt-and-suspenders: alle Modifier vor der Sequenz „löschen".
    let _ = enigo.key(Key::Control, Direction::Release);
    let _ = enigo.key(Key::Shift, Direction::Release);
    let _ = enigo.key(Key::Alt, Direction::Release);
    #[cfg(target_os = "macos")]
    {
        let _ = enigo.key(Key::Meta, Direction::Release);
    }

    // macOS nutzt Cmd (Meta), Win/Linux nutzt Strg.
    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;

    let r1 = enigo.key(modifier, Direction::Press);
    let r2 = enigo.key(Key::Unicode('c'), Direction::Click);
    let r3 = enigo.key(modifier, Direction::Release);
    if let Err(e) = &r1 {
        log::warn!("stage: enigo modifier press failed: {e:?}");
    }
    if let Err(e) = &r2 {
        log::warn!("stage: enigo C click failed: {e:?}");
    }
    if let Err(e) = &r3 {
        log::warn!("stage: enigo modifier release failed: {e:?}");
    }
    for r in [&r1, &r2, &r3] {
        if let Err(e) = r {
            return Err(format!("{e:?}"));
        }
    }
    log::info!("stage: enigo copy sequence returned Ok for all 3 calls");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(
        entity_type: &str,
        original: &str,
        token: &str,
        start: usize,
        end: usize,
    ) -> Finding {
        Finding {
            entity_type: entity_type.to_string(),
            original: original.to_string(),
            token: token.to_string(),
            start,
            end,
            confidence: 0.9,
        }
    }

    // ----------------------------------------------------- classify_capture

    #[test]
    fn classify_empty_and_none_are_empty() {
        assert_eq!(classify_capture(None), CaptureDecision::Empty);
        assert_eq!(
            classify_capture(Some(String::new())),
            CaptureDecision::Empty
        );
    }

    #[test]
    fn classify_oversized_is_too_large() {
        let big = "a".repeat(MAX_CLIPBOARD_BYTES + 1);
        assert_eq!(
            classify_capture(Some(big)),
            CaptureDecision::TooLarge(MAX_CLIPBOARD_BYTES + 1)
        );
    }

    #[test]
    fn classify_normal_text_proceeds() {
        assert_eq!(
            classify_capture(Some("Hallo Welt".into())),
            CaptureDecision::Proceed("Hallo Welt".into())
        );
    }

    // ----------------------------------------------------- is_clipboard_change

    #[test]
    fn change_detection_covers_fallback_logic() {
        // prev leer, jetzt Text → Änderung.
        assert!(is_clipboard_change(&None, &Some("x".into())));
        assert!(is_clipboard_change(&Some(String::new()), &Some("x".into())));
        // prev == current → keine Änderung (synthetischer Copy tat nichts).
        assert!(!is_clipboard_change(&Some("x".into()), &Some("x".into())));
        // jetzt leer/kein Text → keine Änderung.
        assert!(!is_clipboard_change(&Some("x".into()), &None));
        assert!(!is_clipboard_change(&None, &None));
        assert!(!is_clipboard_change(&None, &Some(String::new())));
        // unterschiedlicher Text → Änderung.
        assert!(is_clipboard_change(
            &Some("alt".into()),
            &Some("neu".into())
        ));
    }

    // ----------------------------------------------------- classify_stage_hotkey

    #[test]
    fn stage_hotkey_empty_disables() {
        assert_eq!(
            classify_stage_hotkey("", "CmdOrCtrl+Alt+B"),
            StageHotkeyDecision::Disabled
        );
    }

    #[test]
    fn stage_hotkey_equal_to_smart_conflicts() {
        assert_eq!(
            classify_stage_hotkey("CmdOrCtrl+Alt+B", "CmdOrCtrl+Alt+B"),
            StageHotkeyDecision::Conflict
        );
    }

    #[test]
    fn stage_hotkey_distinct_enables() {
        assert_eq!(
            classify_stage_hotkey("CmdOrCtrl+Alt+Shift+B", "CmdOrCtrl+Alt+B"),
            StageHotkeyDecision::Enabled
        );
    }

    // ----------------------------------------------------- aggregate_entity_counts

    #[test]
    fn entity_counts_aggregate_per_type() {
        let findings = vec![
            finding("person", "Max", "«P_a»", 0, 3),
            finding("person", "Eva", "«P_b»", 4, 7),
            finding("iban", "DE00", "«I_a»", 8, 12),
        ];
        let counts = aggregate_entity_counts(&findings);
        assert_eq!(counts.get("person"), Some(&2));
        assert_eq!(counts.get("iban"), Some(&1));
        assert_eq!(counts.len(), 2);
    }

    // ----------------------------------------------------- build_segments

    #[test]
    fn segments_zero_findings_single_text() {
        let (segs, truncated) = build_segments("Nur normaler Text.", &[]);
        assert!(!truncated);
        assert_eq!(
            segs,
            vec![Segment::Text {
                content: "Nur normaler Text.".into()
            }]
        );
    }

    #[test]
    fn segments_empty_text_is_empty() {
        let (segs, truncated) = build_segments("", &[]);
        assert!(!truncated);
        assert!(segs.is_empty());
    }

    #[test]
    fn segments_split_at_finding_boundaries() {
        // "Sehr geehrter Herr Müller, hallo"
        //  Finding "Herr Müller" — Byte-Offsets (ü = 2 Bytes).
        let text = "Sehr geehrter Herr Müller, hallo";
        let start = text.find("Herr").unwrap();
        let end = start + "Herr Müller".len();
        let f = finding("person", "Herr Müller", "«P_a4b»", start, end);
        let (segs, truncated) = build_segments(text, &[f]);
        assert!(!truncated);
        assert_eq!(
            segs,
            vec![
                Segment::Text {
                    content: "Sehr geehrter ".into()
                },
                Segment::Finding {
                    original: "Herr Müller".into(),
                    replacement: "«P_a4b»".into(),
                    entity_type: "person".into(),
                    confidence: 0.9,
                },
                Segment::Text {
                    content: ", hallo".into()
                },
            ]
        );
    }

    #[test]
    fn segments_handle_umlauts_and_emoji_around_boundaries() {
        // Grenzen liegen direkt an Mehr-Byte-Zeichen — byte-basiertes Slicing
        // muss exakt an den Finding-Offsets schneiden, ohne Zeichen zu zerhacken.
        let text = "Öäü 🚀 Max 🎉 Ende";
        let start = text.find("Max").unwrap();
        let end = start + "Max".len();
        let f = finding("person", "Max", "«P_x»", start, end);
        let (segs, _truncated) = build_segments(text, &[f]);
        // Rekonstruktion aus den Segmenten muss den Originaltext ergeben
        // (Finding-Segment steuert `original` bei).
        let rebuilt: String = segs
            .iter()
            .map(|s| match s {
                Segment::Text { content } => content.clone(),
                Segment::Finding { original, .. } => original.clone(),
            })
            .collect();
        assert_eq!(rebuilt, text);
    }

    #[test]
    fn segments_truncate_at_char_cap() {
        // Text länger als der Cap → letztes Segment ist "… [gekürzt]",
        // truncated = true, und der belegte Anteil überschreitet den Cap nicht.
        let text = "x".repeat(SEGMENT_CHAR_CAP + 500);
        let (segs, truncated) = build_segments(&text, &[]);
        assert!(truncated);
        let last = segs.last().unwrap();
        assert_eq!(
            last,
            &Segment::Text {
                content: "… [gekürzt]".into()
            }
        );
        // Angezeigter Originaltext (ohne den Marker) exakt auf den Cap gekappt.
        let shown: usize = segs
            .iter()
            .take(segs.len() - 1)
            .map(|s| match s {
                Segment::Text { content } => content.chars().count(),
                Segment::Finding { original, .. } => original.chars().count(),
            })
            .sum();
        assert_eq!(shown, SEGMENT_CHAR_CAP);
    }

    #[test]
    fn segments_cap_counts_chars_not_bytes() {
        // Mehr-Byte-Zeichen: 8 001 Umlaute = 16 002 Bytes. Der Cap zählt
        // Zeichen, also wird bei 8 000 Zeichen gekappt, nicht früher.
        let text = "ä".repeat(SEGMENT_CHAR_CAP + 1);
        let (segs, truncated) = build_segments(&text, &[]);
        assert!(truncated);
        let shown: usize = segs
            .iter()
            .take(segs.len() - 1)
            .map(|s| match s {
                Segment::Text { content } => content.chars().count(),
                _ => 0,
            })
            .sum();
        assert_eq!(shown, SEGMENT_CHAR_CAP);
    }

    #[test]
    fn segments_exact_cap_not_truncated() {
        let text = "y".repeat(SEGMENT_CHAR_CAP);
        let (segs, truncated) = build_segments(&text, &[]);
        assert!(!truncated);
        assert!(segs.iter().all(|s| !matches!(
            s,
            Segment::Text { content } if content == "… [gekürzt]"
        )));
    }

    #[test]
    fn segments_skip_overlapping_defensively() {
        // Zweites Finding zeigt rückwärts (start < pos) → wird übersprungen,
        // statt zu panicken.
        let text = "abcdefgh";
        let f1 = finding("person", "bcd", "«P_a»", 1, 4);
        let f2 = finding("person", "cde", "«P_b»", 2, 5); // überlappt f1
        let (segs, _truncated) = build_segments(text, &[f1, f2]);
        // f1 wird angezeigt, f2 übersprungen; kein Panic.
        assert!(segs.iter().any(|s| matches!(
            s,
            Segment::Finding { replacement, .. } if replacement == "«P_a»"
        )));
        assert!(!segs.iter().any(|s| matches!(
            s,
            Segment::Finding { replacement, .. } if replacement == "«P_b»"
        )));
    }

    // ----------------------------------------------------- Payload-Serialisierung

    #[test]
    fn segment_json_shape_matches_contract() {
        let text_seg = Segment::Text {
            content: "Sehr geehrter ".into(),
        };
        let finding_seg = Segment::Finding {
            original: "Herr Müller".into(),
            replacement: "«P_a4b»".into(),
            entity_type: "person".into(),
            confidence: 0.93,
        };
        let tj = serde_json::to_value(&text_seg).unwrap();
        assert_eq!(tj["kind"], "text");
        assert_eq!(tj["content"], "Sehr geehrter ");

        let fj = serde_json::to_value(&finding_seg).unwrap();
        assert_eq!(fj["kind"], "finding");
        assert_eq!(fj["original"], "Herr Müller");
        assert_eq!(fj["replacement"], "«P_a4b»");
        assert_eq!(fj["entity_type"], "person");
    }

    #[test]
    fn stage_job_json_shape_matches_contract() {
        let job = StageJob {
            job_id: "c3f9".into(),
            mode: "reversible".into(),
            stash_id: Some(42),
            finding_count: 3,
            truncated: false,
            segments: vec![Segment::Text {
                content: "hi".into(),
            }],
            content_kind: "plain".into(),
            annotated_html: None,
            image_data_url: None,
            boxes: Vec::new(),
            ocr_based: false,
        };
        let j = serde_json::to_value(&job).unwrap();
        assert_eq!(j["job_id"], "c3f9");
        assert_eq!(j["mode"], "reversible");
        assert_eq!(j["stash_id"], 42);
        assert_eq!(j["finding_count"], 3);
        assert_eq!(j["truncated"], false);
        assert!(j["segments"].is_array());
        assert_eq!(j["content_kind"], "plain");
        assert!(j["annotated_html"].is_null());
    }

    #[test]
    fn stage_job_null_stash_id_serializes_as_null() {
        let job = StageJob {
            job_id: "u".into(),
            mode: "strict".into(),
            stash_id: None,
            finding_count: 0,
            truncated: false,
            segments: Vec::new(),
            content_kind: "plain".into(),
            annotated_html: None,
            image_data_url: None,
            boxes: Vec::new(),
            ocr_based: false,
        };
        let j = serde_json::to_value(&job).unwrap();
        assert!(j["stash_id"].is_null());
    }

    // ----------------------------------------------------- prepare_content (Stufe 2)

    #[test]
    fn prepare_prefers_html_when_it_has_text() {
        let content = prepare_content(
            Some("Fallback-Text".into()),
            Some("<p>Hallo <b>Max</b></p>".into()),
            None,
        )
        .unwrap();
        match content {
            StageContent::Html {
                parsed,
                text_flavor,
                ..
            } => {
                assert!(
                    parsed.plaintext().contains("Hallo Max"),
                    "got: {:?}",
                    parsed.plaintext()
                );
                // Der originale Text-Flavor wird für den Clipboard-Fallback
                // mitgeführt.
                assert_eq!(text_flavor.as_deref(), Some("Fallback-Text"));
            }
            other => panic!("erwartet Html, got: {other:?}"),
        }
    }

    #[test]
    fn prepare_falls_back_to_text_for_image_only_html() {
        // Nur-Bild-HTML (Review-Befund): der injizierte Platzhalter zählt
        // NICHT als sichtbarer Text — der Plain-Flavor gewinnt, damit dessen
        // Inhalt gescannt wird statt einer „[Bild entfernt]"-Bühne.
        let content = prepare_content(
            Some("Nur Text".into()),
            Some(r#"<img src="https://tracker.example/p.gif">"#.into()),
            None,
        )
        .unwrap();
        assert!(
            matches!(&content, StageContent::Plain(t) if t == "Nur Text"),
            "erwartet Plain-Fallback, got: {content:?}"
        );

        let empty = prepare_content(Some("Nur Text".into()), Some("<p>  </p>".into()), None).unwrap();
        assert!(matches!(&empty, StageContent::Plain(t) if t == "Nur Text"));

        // Nur-Bild-HTML UND leerer Text-Flavor → Fehler-State, keine Bühne
        // voller Platzhalter.
        assert!(prepare_content(None, Some(r#"<img src="https://x.example/p.gif">"#.into()), None)
            .is_err());
    }

    #[test]
    fn prepare_without_html_is_plain() {
        let content = prepare_content(Some("abc".into()), None, None).unwrap();
        assert!(matches!(&content, StageContent::Plain(t) if t == "abc"));
    }

    #[test]
    fn prepare_oversized_html_falls_back_to_text() {
        let big_html = format!("<p>{}</p>", "x".repeat(MAX_CLIPBOARD_BYTES + 1));
        let content = prepare_content(Some("klein".into()), Some(big_html), None).unwrap();
        assert!(matches!(&content, StageContent::Plain(t) if t == "klein"));
    }

    #[test]
    fn prepare_empty_everything_is_error() {
        assert!(prepare_content(None, None, None).is_err());
        assert!(prepare_content(Some(String::new()), Some(String::new()), None).is_err());
    }

    #[test]
    fn prepare_image_only_when_no_text_flavors() {
        // Screenshot-Fall: nur Bild im Clipboard → Bild-Flow.
        let content = prepare_content(None, None, Some(vec![1, 2, 3])).unwrap();
        assert!(matches!(&content, StageContent::Image(b) if b == &vec![1, 2, 3]));

        // Text schlägt Bild (Bild greift nur als letztes).
        let content =
            prepare_content(Some("Text".into()), None, Some(vec![1, 2, 3])).unwrap();
        assert!(matches!(&content, StageContent::Plain(t) if t == "Text"));

        // Übergroßes Bild → TooLarge statt Empty. Der Bild-Cap ist eigener
        // (64 MB) — unkomprimiertes CF_DIB normaler Screenshots liegt weit
        // über dem 10-MB-Text-Cap.
        let normal_screenshot_dib = vec![0u8; MAX_CLIPBOARD_BYTES + 1]; // ~10 MB
        assert!(matches!(
            prepare_content(None, None, Some(normal_screenshot_dib)),
            Ok(StageContent::Image(_))
        ));
        let big = vec![0u8; MAX_IMAGE_INPUT_BYTES + 1];
        assert!(matches!(
            prepare_content(None, None, Some(big)),
            Err(CaptureDecision::TooLarge(_))
        ));

        // Leeres Bild-Array zählt nicht als Inhalt.
        assert!(prepare_content(None, None, Some(Vec::new())).is_err());
    }

    #[test]
    fn prepare_oversized_text_beats_image() {
        // Invariante aus Stufe 1: übergroßer Text-Flavor endet IMMER im
        // TooLarge-State — das Bitmap-Rendering daneben (Excel/Word) darf
        // den kopierten Text nicht still durch ein PNG ersetzen.
        let big_text = "x".repeat(MAX_CLIPBOARD_BYTES + 1);
        assert!(matches!(
            prepare_content(Some(big_text), None, Some(vec![1, 2, 3])),
            Err(CaptureDecision::TooLarge(_))
        ));
    }

    // ----------------------------------------------------- Anzeige-Caps (Stufe 2)

    #[test]
    fn display_fits_checks_both_caps() {
        assert!(html_display_fits("kurz", "<p>kurz</p>"));
        // Zeichen-Cap überschritten → Plain-Preview.
        let long_text = "x".repeat(SEGMENT_CHAR_CAP + 1);
        assert!(!html_display_fits(&long_text, "<p>ok</p>"));
        // Byte-Cap überschritten (z. B. data:-Bilder) trotz kurzem Text →
        // Plain-Preview (Review-Befund: Multi-MB-DOM friert die Bühne ein).
        let heavy_html = format!(
            r#"<p>kurz</p><img src="data:image/png;base64,{}">"#,
            "A".repeat(ANNOTATED_HTML_DISPLAY_CAP_BYTES)
        );
        assert!(!html_display_fits("kurz", &heavy_html));
    }

    // ----------------------------------------------------- Text-Flavor-Fallback

    #[test]
    fn text_flavor_redaction_keeps_source_text_shape() {
        // Der Clipboard-Text-Fallback kommt aus dem ORIGINALEN Plain-Flavor
        // (Listennummern etc. bleiben) — geschwärzt mit derselben case_id,
        // damit die Tokens zu denen der HTML-Fassung passen.
        let case_id = secrets::new_case_id();
        let source = "1. Schreib an max.mustermann@example.de\n2. Danach abschicken";
        let redacted = redact_text_flavor(source, false, &case_id);
        assert!(!redacted.contains("max.mustermann@example.de"));
        assert!(redacted.contains("1. Schreib an"), "got: {redacted}");
        assert!(redacted.contains("2. Danach abschicken"), "got: {redacted}");

        // Token-Konsistenz: gleiche case_id → gleiches Token für dieselbe
        // Fundstelle wie im (HTML-)Detection-Pfad.
        let html_findings =
            detection::detect_with_case("Mail: max.mustermann@example.de", &case_id);
        let token = &html_findings
            .iter()
            .find(|f| f.entity_type == "email")
            .expect("Mail muss erkannt werden")
            .token;
        assert!(redacted.contains(token.as_str()), "got: {redacted}");
    }

    #[test]
    fn text_flavor_redaction_strict_uses_placeholders() {
        let redacted = redact_text_flavor("Mail: max.mustermann@example.de", true, "egal");
        assert!(!redacted.contains("max.mustermann@example.de"));
        assert!(redacted.contains("anonymisierte Platzhalter"), "Strict-Hint fehlt: {redacted}");
    }

    // ----------------------------------------------------- finalize_redacted_html

    #[test]
    fn html_hint_appended_only_with_findings() {
        let plain = finalize_redacted_html("<p>x</p>".into(), "reversible", 0);
        assert_eq!(plain, "<p>x</p>");

        let hinted = finalize_redacted_html("<p>«P_a»</p>".into(), "reversible", 1);
        assert!(hinted.starts_with("<p>«P_a»</p><hr><p>Hinweis:"), "got: {hinted}");
        assert!(!hinted.contains("<script"));

        let strict = finalize_redacted_html("<p>«Person A»</p>".into(), "strict", 2);
        assert!(strict.contains("anonymisierte Platzhalter"), "got: {strict}");
    }
}
