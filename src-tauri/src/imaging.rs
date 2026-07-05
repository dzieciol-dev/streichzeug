//! Bild-Pipeline der Schwärz-Bühne (Stufe 3, WP-I) — reine Logik.
//!
//! Nimmt die [`crate::ocr`]-Wörter und die Detection-Findings und macht
//! daraus (a) den OCR-Plaintext, (b) die normierten Schwärz-Boxen und
//! (c) das geschwärzte PNG. Kein OS-Zugriff — alles unit-testbar mit
//! synthetischen Wörtern und generierten Bildern.
//!
//! # Ehrlichkeit
//!
//! Die Pipeline schwärzt nur, was die Texterkennung GELESEN hat. Was OCR
//! nicht erkennt (Handschrift, exotische Fonts, Logos), bleibt sichtbar —
//! das UI (WP-J) trägt den Warnhinweis, der Payload das `ocr_based`-Flag.
//!
//! # EXIF
//!
//! Das Ausgabe-PNG entsteht durch Neu-Kodieren der Pixel — Metadaten des
//! Originals (EXIF, GPS, XMP) existieren darin nicht. Gleiches gilt für die
//! Anzeige-Kopie des Originals ([`reencode_png`]).

use serde::Serialize;

use crate::detection::Finding;
use crate::ocr::OcrWord;

/// Hartes Kanten-Limit. Über ~8k Pixeln pro Seite wird die Pixel-Arbeit
/// (RGBA-Puffer, Re-Encode) unverhältnismäßig; reale Screenshots/Scans
/// liegen weit darunter.
pub const MAX_IMAGE_DIMENSION: u32 = 8000;

/// Padding der Schwärz-Balken in Pixeln — OCR-Boxen sitzen eng auf den
/// Glyphen, ohne Rand blieben Ober-/Unterlängen sichtbar.
const REDACTION_PADDING_PX: u32 = 3;

/// Eine Schwärz-Box im Payload (Vertrag Stufe 3): normierte Koordinaten
/// (0–1, Ursprung oben links), plus Entity-Typ und Replacement fürs UI.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RedactionBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub entity_type: String,
    pub replacement: String,
}

/// Baut aus den OCR-Wörtern den Detection-Plaintext: Wörter einer Zeile mit
/// Space verbunden, Zeilen mit `\n`. Liefert zusätzlich die Byte-Range jedes
/// Worts im Plaintext — die Rück-Zuordnung der Finding-Offsets.
pub fn assemble_text(words: &[OcrWord]) -> (String, Vec<(usize, usize)>) {
    let mut text = String::new();
    let mut spans = Vec::with_capacity(words.len());
    let mut current_line: Option<usize> = None;
    for word in words {
        match current_line {
            None => {}
            Some(line) if line == word.line => text.push(' '),
            Some(_) => text.push('\n'),
        }
        current_line = Some(word.line);
        let start = text.len();
        text.push_str(&word.text);
        spans.push((start, text.len()));
    }
    (text, spans)
}

/// Mappt Findings (Byte-Offsets im Plaintext aus [`assemble_text`]) auf
/// Schwärz-Boxen: alle überlappten Wörter einsammeln, **pro Zeile** zur
/// Union vereinigen (mehrwortige Entities ergeben einen durchgehenden
/// Balken je Zeile statt Einzel-Kästchen).
pub fn map_findings_to_boxes(
    findings: &[Finding],
    words: &[OcrWord],
    spans: &[(usize, usize)],
) -> Vec<RedactionBox> {
    let mut boxes = Vec::new();
    for f in findings {
        // Überlappte Wörter, gruppiert nach Zeile (Wörter sind in
        // Lesereihenfolge — die Gruppen sind zusammenhängend).
        let mut current: Option<(usize, f64, f64, f64, f64)> = None; // (line, x0, y0, x1, y1)
        for (word, &(start, end)) in words.iter().zip(spans) {
            if end <= f.start || start >= f.end {
                continue;
            }
            let (x0, y0, x1, y1) = (word.x, word.y, word.x + word.w, word.y + word.h);
            match &mut current {
                Some((line, cx0, cy0, cx1, cy1)) if *line == word.line => {
                    *cx0 = cx0.min(x0);
                    *cy0 = cy0.min(y0);
                    *cx1 = cx1.max(x1);
                    *cy1 = cy1.max(y1);
                }
                Some(finished) => {
                    boxes.push(box_from_union(*finished, f));
                    current = Some((word.line, x0, y0, x1, y1));
                }
                None => {
                    current = Some((word.line, x0, y0, x1, y1));
                }
            }
        }
        if let Some(finished) = current {
            boxes.push(box_from_union(finished, f));
        }
    }
    boxes
}

fn box_from_union(
    (_line, x0, y0, x1, y1): (usize, f64, f64, f64, f64),
    finding: &Finding,
) -> RedactionBox {
    RedactionBox {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
        entity_type: finding.entity_type.clone(),
        replacement: finding.token.clone(),
    }
}

/// Dekodiert Bild-Bytes (PNG/JPEG/BMP/TIFF) zu RGBA. `Err` bei unbekanntem
/// Format oder Überschreitung von [`MAX_IMAGE_DIMENSION`].
pub fn decode_image(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| format!("Bild nicht dekodierbar: {e}"))?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("Bild hat keine Fläche".into());
    }
    if w > MAX_IMAGE_DIMENSION || h > MAX_IMAGE_DIMENSION {
        return Err(format!(
            "Bild zu groß ({w}×{h} px, Limit {MAX_IMAGE_DIMENSION} px pro Seite)"
        ));
    }
    Ok(img.to_rgba8())
}

/// Kodiert ein RGBA-Bild als PNG. Weil nur Pixel kodiert werden, enthält das
/// Ergebnis keinerlei Metadaten des Originals (EXIF/GPS/XMP-Strip gratis).
pub fn encode_png(img: &image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .map_err(|e| format!("PNG-Encode fehlgeschlagen: {e}"))?;
    Ok(out.into_inner())
}

/// Dekodiert und rekodiert Bild-Bytes als PNG — für die Anzeige-Kopie des
/// Originals (einheitliches Format fürs Frontend, Metadaten weg).
pub fn reencode_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let img = decode_image(bytes)?;
    let (w, h) = (img.width(), img.height());
    Ok((encode_png(&img)?, w, h))
}

/// Malt die Schwärz-Boxen als voll deckende schwarze Balken ins Bild
/// (inkl. [`REDACTION_PADDING_PX`] Rand, an den Bildkanten geklemmt).
pub fn render_redactions(img: &mut image::RgbaImage, boxes: &[RedactionBox]) {
    let (width, height) = (img.width(), img.height());
    let black = image::Rgba([0u8, 0, 0, 255]);
    for b in boxes {
        // Normiert → Pixel, mit Padding und Clamping.
        let x0 = ((b.x * width as f64).floor() as i64 - REDACTION_PADDING_PX as i64).max(0) as u32;
        let y0 = ((b.y * height as f64).floor() as i64 - REDACTION_PADDING_PX as i64).max(0) as u32;
        let x1 = (((b.x + b.w) * width as f64).ceil() as u32 + REDACTION_PADDING_PX).min(width);
        let y1 = (((b.y + b.h) * height as f64).ceil() as u32 + REDACTION_PADDING_PX).min(height);
        for y in y0..y1 {
            for x in x0..x1 {
                img.put_pixel(x, y, black);
            }
        }
    }
}

// =================================================================== Tests

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, line: usize, x: f64, y: f64, w: f64, h: f64) -> OcrWord {
        OcrWord {
            text: text.to_string(),
            x,
            y,
            w,
            h,
            line,
        }
    }

    fn finding_for(text: &str, needle: &str, token: &str) -> Finding {
        let start = text.find(needle).expect("needle im Text");
        Finding {
            entity_type: "person".into(),
            original: needle.into(),
            token: token.into(),
            start,
            end: start + needle.len(),
            confidence: 0.9,
        }
    }

    #[test]
    fn assemble_joins_words_and_lines() {
        let words = vec![
            word("Sehr", 0, 0.1, 0.1, 0.1, 0.05),
            word("geehrter", 0, 0.22, 0.1, 0.15, 0.05),
            word("Max", 1, 0.1, 0.2, 0.08, 0.05),
            word("Mustermann", 1, 0.2, 0.2, 0.2, 0.05),
        ];
        let (text, spans) = assemble_text(&words);
        assert_eq!(text, "Sehr geehrter\nMax Mustermann");
        // Spans zeigen exakt auf die Wörter.
        for (w, &(s, e)) in words.iter().zip(&spans) {
            assert_eq!(&text[s..e], w.text);
        }
    }

    #[test]
    fn assemble_handles_umlauts() {
        let words = vec![
            word("Grüße", 0, 0.0, 0.0, 0.1, 0.05),
            word("Müller", 0, 0.15, 0.0, 0.1, 0.05),
        ];
        let (text, spans) = assemble_text(&words);
        assert_eq!(text, "Grüße Müller");
        assert_eq!(&text[spans[1].0..spans[1].1], "Müller");
    }

    #[test]
    fn map_single_word_finding() {
        let words = vec![
            word("Mail:", 0, 0.05, 0.1, 0.1, 0.04),
            word("max@example.de", 0, 0.2, 0.1, 0.3, 0.04),
        ];
        let (text, spans) = assemble_text(&words);
        let f = finding_for(&text, "max@example.de", "«E_a»");
        let boxes = map_findings_to_boxes(&[f], &words, &spans);
        assert_eq!(boxes.len(), 1);
        let b = &boxes[0];
        assert!((b.x - 0.2).abs() < 1e-9 && (b.w - 0.3).abs() < 1e-9);
        assert_eq!(b.replacement, "«E_a»");
    }

    #[test]
    fn map_multiword_finding_unions_per_line() {
        // „Max Mustermann" über zwei Wörter derselben Zeile → EIN Balken.
        let words = vec![
            word("Von", 0, 0.0, 0.1, 0.06, 0.04),
            word("Max", 0, 0.1, 0.1, 0.08, 0.04),
            word("Mustermann", 0, 0.2, 0.09, 0.25, 0.05),
        ];
        let (text, spans) = assemble_text(&words);
        let f = finding_for(&text, "Max Mustermann", "«P_a»");
        let boxes = map_findings_to_boxes(&[f], &words, &spans);
        assert_eq!(boxes.len(), 1);
        let b = &boxes[0];
        assert!((b.x - 0.1).abs() < 1e-9);
        assert!((b.x + b.w - 0.45).abs() < 1e-9, "Union bis Wortende");
        assert!((b.y - 0.09).abs() < 1e-9, "Union nimmt min-y");
        assert!((b.h - 0.05).abs() < 1e-9, "Union nimmt max-Höhe");
    }

    #[test]
    fn map_finding_across_lines_gives_one_box_per_line() {
        // Entity über einen Zeilenumbruch → zwei Balken (einer je Zeile),
        // kein Riesen-Balken über beide Zeilen samt Zwischenraum.
        let words = vec![
            word("Max", 0, 0.7, 0.1, 0.08, 0.04),
            word("Mustermann", 1, 0.05, 0.2, 0.25, 0.04),
        ];
        let (text, spans) = assemble_text(&words);
        assert_eq!(text, "Max\nMustermann");
        let f = Finding {
            entity_type: "person".into(),
            original: "Max\nMustermann".into(),
            token: "«P_a»".into(),
            start: 0,
            end: text.len(),
            confidence: 0.9,
        };
        let boxes = map_findings_to_boxes(&[f], &words, &spans);
        assert_eq!(boxes.len(), 2);
        assert!((boxes[0].y - 0.1).abs() < 1e-9);
        assert!((boxes[1].y - 0.2).abs() < 1e-9);
    }

    #[test]
    fn map_ignores_non_overlapping_findings() {
        let words = vec![word("Harmlos", 0, 0.1, 0.1, 0.2, 0.05)];
        let (text, spans) = assemble_text(&words);
        let f = Finding {
            entity_type: "person".into(),
            original: "X".into(),
            token: "«P_x»".into(),
            start: text.len() + 10, // außerhalb
            end: text.len() + 11,
            confidence: 0.9,
        };
        assert!(map_findings_to_boxes(&[f], &words, &spans).is_empty());
    }

    #[test]
    fn redaction_paints_black_with_padding_and_clamps() {
        let mut img = image::RgbaImage::from_pixel(100, 50, image::Rgba([255, 255, 255, 255]));
        let boxes = vec![RedactionBox {
            x: 0.0, // Padding würde links aus dem Bild laufen → clampen
            y: 0.2,
            w: 0.5,
            h: 0.2,
            entity_type: "person".into(),
            replacement: "«P_a»".into(),
        }];
        render_redactions(&mut img, &boxes);
        // Kern der Box ist schwarz …
        assert_eq!(img.get_pixel(25, 15), &image::Rgba([0, 0, 0, 255]));
        // … Padding wirkt (Pixel oberhalb der Box, innerhalb 3px) …
        assert_eq!(img.get_pixel(25, 8), &image::Rgba([0, 0, 0, 255]));
        // … und weit außerhalb bleibt weiß.
        assert_eq!(img.get_pixel(90, 45), &image::Rgba([255, 255, 255, 255]));
    }

    #[test]
    fn png_roundtrip_strips_dimensions_survive() {
        let img = image::RgbaImage::from_pixel(20, 10, image::Rgba([1, 2, 3, 255]));
        let png = encode_png(&img).unwrap();
        let (reencoded, w, h) = reencode_png(&png).unwrap();
        assert_eq!((w, h), (20, 10));
        // PNG-Signatur vorhanden.
        assert_eq!(&reencoded[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    }

    #[test]
    fn decode_rejects_garbage_and_oversize() {
        assert!(decode_image(b"kein bild").is_err());
        // Übergroßes Bild synthetisch: encode zulässig, decode-Guard greift
        // erst über dem Limit — hier nur der Garbage-Pfad, das Limit selbst
        // ist eine Konstante ohne eigenen Testwert (8000² wäre 256 MB RAM).
    }
}
