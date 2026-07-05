//! Lokale OS-Texterkennung für die Bild-Bühne (Stufe 3, WP-I).
//!
//! Ein dünner Adapter über die nativen OCR-Engines — **kein** Modell-Download,
//! **kein** Netz, kein Feature-Flag: beides sind System-APIs.
//!
//! - **macOS:** Apple Vision (`VNRecognizeTextRequest`, accurate,
//!   de-DE + en-US). Die Bindings kommen aus `objc2-vision` 0.3
//!   (objc2-0.6-Generation) — dieses Modul ist bewusst in sich geschlossen
//!   (Input Bild-Bytes, Output reine Rust-Typen), damit die zwei
//!   objc2-Generationen im Tree sich nie berühren.
//! - **Windows:** `Windows.Media.Ocr` (WinRT) über die windows-Crate,
//!   Sprache aus dem User-Profil.
//! - **Andere Plattformen:** `Err` — die Bild-Bühne meldet das ehrlich.
//!
//! # Koordinaten-Vertrag
//!
//! [`OcrWord`]-Boxen sind **normiert (0–1)** mit Ursprung **oben links** —
//! exakt das, was CSS-Overlays im Frontend und die Pixel-Redaction in
//! [`crate::imaging`] brauchen. Vision liefert unten-links-normiert
//! (wird hier gespiegelt), Windows liefert Pixel (wird hier normiert).
//!
//! # Wort- vs. Zeilen-Boxen
//!
//! Der Konzept-Vertrag (`OcrWord { text, bbox }`) ist um ein `line`-Feld
//! erweitert: beide APIs liefern Zeilen nativ, und das Mapping in
//! [`crate::imaging`] braucht die Zeilenzugehörigkeit für die Box-Union
//! mehrwortiger Entities („Box-Union pro Zeile").

/// Ein erkanntes Wort mit normierter Bounding-Box (0–1, Ursprung oben links)
/// und dem Index der Zeile, aus der es stammt.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrWord {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Zeilen-Index in Lesereihenfolge — Basis für Text-Zusammenbau und
    /// zeilenweise Box-Union in [`crate::imaging`].
    pub line: usize,
}

/// Erkennt Text in einem Bild (PNG/JPEG/TIFF-Bytes, wie von Clipboard oder
/// Datei-Drop geliefert). Liefert Wörter in Lesereihenfolge.
pub fn recognize(image_bytes: &[u8]) -> Result<Vec<OcrWord>, String> {
    #[cfg(target_os = "macos")]
    {
        vision::recognize(image_bytes)
    }
    #[cfg(target_os = "windows")]
    {
        winocr::recognize(image_bytes)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = image_bytes;
        Err("Texterkennung ist auf dieser Plattform nicht verfügbar (macOS/Windows)".into())
    }
}

// =================================================================== macOS (Vision)

#[cfg(target_os = "macos")]
mod vision {
    use super::OcrWord;

    // Vision gehört zur objc2-0.6-Generation — Retained/AnyThread MÜSSEN aus
    // `objc2_06` kommen, nicht aus dem 0.5er `objc2` des restlichen Crates.
    use objc2_06::rc::Retained;
    use objc2_06::AnyThread;
    use objc2_foundation06::{NSArray, NSData, NSDictionary, NSRange, NSString};
    use objc2_vision::{
        VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
    };

    /// Vision-OCR über `VNRecognizeTextRequest`. Läuft synchron auf dem
    /// aufrufenden Thread (Vision ist dafür ausgelegt; der Capture-Flow ruft
    /// aus einem Kontext, in dem ein paar hundert Millisekunden okay sind —
    /// die Bühne öffnet sich danach).
    pub fn recognize(image_bytes: &[u8]) -> Result<Vec<OcrWord>, String> {
        let data = NSData::with_bytes(image_bytes);
        let options = NSDictionary::new();
        let handler = VNImageRequestHandler::initWithData_options(
            VNImageRequestHandler::alloc(),
            &data,
            &options,
        );

        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setUsesLanguageCorrection(true);
        let languages = NSArray::from_retained_slice(&[
            NSString::from_str("de-DE"),
            NSString::from_str("en-US"),
        ]);
        request.setRecognitionLanguages(&languages);

        // Upcast auf VNRequest für performRequests (Klassenhierarchie:
        // VNRecognizeTextRequest → VNImageBasedRequest → VNRequest).
        let base: Retained<VNRequest> =
            Retained::into_super(Retained::into_super(request.clone()));
        let requests = NSArray::from_retained_slice(&[base]);
        handler
            .performRequests_error(&requests)
            .map_err(|e| format!("Vision performRequests: {e}"))?;

        let Some(observations) = request.results() else {
            return Ok(Vec::new());
        };

        let mut words = Vec::new();
        for (line_index, observation) in observations.iter().enumerate() {
            let candidates = observation.topCandidates(1);
            let Some(candidate) = candidates.firstObject() else {
                continue;
            };
            let line_text = candidate.string().to_string();

            // Wörter mit ihren UTF-16-Ranges (NSRange rechnet in UTF-16-
            // Code-Units, Rust-Strings in UTF-8-Bytes — hier wird sauber
            // umgerechnet statt Offsets zu raten).
            for (utf16_start, word) in split_words_utf16(&line_text) {
                let utf16_len = word.encode_utf16().count();
                let range = NSRange::new(utf16_start, utf16_len);
                // SAFETY: range liegt garantiert innerhalb des Strings, aus
                // dem er berechnet wurde; boundingBoxForRange ist ein reiner
                // Rechen-Call auf dem Kandidaten.
                let rect_observation =
                    match unsafe { candidate.boundingBoxForRange_error(range) } {
                        Ok(o) => o,
                        Err(e) => {
                            log::debug!("ocr: boundingBoxForRange fehlgeschlagen ({e}) — Wort übersprungen");
                            continue;
                        }
                    };
                // SAFETY: reiner Property-Read.
                let rect = unsafe { rect_observation.boundingBox() };
                let (x, w) = (rect.origin.x, rect.size.width);
                let h = rect.size.height;
                // Vision: normiert, Ursprung UNTEN links → oben links spiegeln.
                let y = 1.0 - rect.origin.y - h;
                words.push(OcrWord {
                    text: word.to_string(),
                    x,
                    y,
                    w,
                    h,
                    line: line_index,
                });
            }
        }
        Ok(words)
    }

    /// Zerlegt eine Zeile in Whitespace-getrennte Wörter und liefert je Wort
    /// den Start-Offset in UTF-16-Code-Units (für NSRange).
    fn split_words_utf16(line: &str) -> Vec<(usize, &str)> {
        let mut out = Vec::new();
        let mut utf16_pos = 0usize;
        let mut byte_pos = 0usize;
        for part in line.split_inclusive(char::is_whitespace) {
            // split_inclusive hält Wort+Trenner zusammen; Wort = getrimmt.
            let word = part.trim_end_matches(char::is_whitespace);
            if !word.is_empty() {
                out.push((utf16_pos, &line[byte_pos..byte_pos + word.len()]));
            }
            utf16_pos += part.encode_utf16().count();
            byte_pos += part.len();
        }
        out
    }
}

// =================================================================== Windows (Windows.Media.Ocr)

#[cfg(target_os = "windows")]
mod winocr {
    use super::OcrWord;

    use windows::Graphics::Imaging::BitmapDecoder;
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

    /// WinRT-OCR über `Windows.Media.Ocr`. Die `.get()`-Aufrufe blocken auf
    /// den IAsyncOperations — der Capture-Flow verträgt das (s. Vision-Doku).
    pub fn recognize(image_bytes: &[u8]) -> Result<Vec<OcrWord>, String> {
        let engine = OcrEngine::TryCreateFromUserProfileLanguages()
            .map_err(|e| format!("OcrEngine: {e}"))?;

        // Bild-Bytes → InMemoryRandomAccessStream → BitmapDecoder → SoftwareBitmap.
        let stream = InMemoryRandomAccessStream::new().map_err(|e| e.to_string())?;
        let writer = DataWriter::CreateDataWriter(&stream).map_err(|e| e.to_string())?;
        writer.WriteBytes(image_bytes).map_err(|e| e.to_string())?;
        writer
            .StoreAsync()
            .map_err(|e| e.to_string())?
            .get()
            .map_err(|e| e.to_string())?;
        writer
            .FlushAsync()
            .map_err(|e| e.to_string())?
            .get()
            .map_err(|e| e.to_string())?;
        // Writer vom Stream lösen, sonst hält er ihn exklusiv.
        writer.DetachStream().map_err(|e| e.to_string())?;
        stream.Seek(0).map_err(|e| e.to_string())?;

        let decoder = BitmapDecoder::CreateAsync(&stream)
            .map_err(|e| e.to_string())?
            .get()
            .map_err(|e| format!("BitmapDecoder (Format nicht unterstützt?): {e}"))?;
        let bitmap = decoder
            .GetSoftwareBitmapAsync()
            .map_err(|e| e.to_string())?
            .get()
            .map_err(|e| e.to_string())?;

        let width = bitmap.PixelWidth().map_err(|e| e.to_string())? as f64;
        let height = bitmap.PixelHeight().map_err(|e| e.to_string())? as f64;
        if width <= 0.0 || height <= 0.0 {
            return Err("Bild hat keine Fläche".into());
        }

        let result = engine
            .RecognizeAsync(&bitmap)
            .map_err(|e| e.to_string())?
            .get()
            .map_err(|e| format!("RecognizeAsync: {e}"))?;

        let mut words = Vec::new();
        for (line_index, line) in result
            .Lines()
            .map_err(|e| e.to_string())?
            .into_iter()
            .enumerate()
        {
            for word in line.Words().map_err(|e| e.to_string())? {
                let rect = word.BoundingRect().map_err(|e| e.to_string())?;
                words.push(OcrWord {
                    text: word.Text().map_err(|e| e.to_string())?.to_string(),
                    // Pixel → normiert (Ursprung ist bereits oben links).
                    x: rect.X as f64 / width,
                    y: rect.Y as f64 / height,
                    w: rect.Width as f64 / width,
                    h: rect.Height as f64 / height,
                    line: line_index,
                });
            }
        }
        Ok(words)
    }
}

// =================================================================== Tests

#[cfg(test)]
mod tests {
    /// End-to-End gegen die echte Vision-Engine mit einem eingecheckten
    /// Brief-Fixture (gedruckte Helvetica — das muss jede OCR lesen).
    ///
    /// `#[ignore]`: läuft nicht im normalen CI-Durchlauf (OCR-Verfügbarkeit
    /// auf Runnern ist nicht garantiert und das Ergebnis nicht bit-stabil).
    /// Lokal ausführen mit: `cargo test -- --ignored ocr`
    #[test]
    #[ignore]
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn recognizes_printed_letter_fixture() {
        let bytes = include_bytes!("../tests/fixtures/ocr-brief.png");
        let words = super::recognize(bytes).expect("OCR muss auf dem Fixture laufen");
        assert!(!words.is_empty(), "keine Wörter erkannt");

        // Boxen sind normiert (0–1) und haben Fläche.
        for w in &words {
            assert!(w.x >= 0.0 && w.y >= 0.0 && w.x + w.w <= 1.0 + 1e-6 && w.y + w.h <= 1.0 + 1e-6,
                "Box außerhalb des Bilds: {w:?}");
            assert!(w.w > 0.0 && w.h > 0.0, "Box ohne Fläche: {w:?}");
        }

        // Zeilen-Indizes sind monoton (Lesereihenfolge).
        assert!(words.windows(2).all(|p| p[0].line <= p[1].line));

        // Der Kern-Inhalt wird gelesen (gedruckter Text, große Schrift).
        let text = crate::imaging::assemble_text(&words).0;
        assert!(
            text.contains("max.mustermann@example.de"),
            "Mail-Adresse nicht erkannt — Text war: {text:?}"
        );
        assert!(text.to_lowercase().contains("telefon"), "got: {text:?}");
    }
}
