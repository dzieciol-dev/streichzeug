//! Layer-3 NER — Named-Entity-Recognition über ein lokales ONNX-Modell.
//!
//! Diese Schicht ergänzt die deterministischen Regex- (L1) und
//! Gazetteer- (L2) Layer um statistische Erkennung für die Fälle, in
//! denen Pattern und Listen versagen:
//!
//! - **Personen ohne Kontext** wie `Jürgen Sonder` in einer Signatur ohne
//!   vorangehende Anrede
//! - **Organisationen ohne Suffix** wie `Sparkasse Dortmund`, `DKB`,
//!   `Klarna`, `Commerzbank` (keine GmbH/AG/eG)
//! - **Adressen ohne Straßen-Suffix** wie `Freistuhl 2`
//!
//! # Modell
//!
//! Empfohlen: `Davlan/distilbert-base-multilingual-cased-ner-hrl`.
//! Multilinguales DistilBERT mit NER-Head, ~70 MB INT8. Labels:
//! `O`, `B-PER`, `I-PER`, `B-ORG`, `I-ORG`, `B-LOC`, `I-LOC`.
//!
//! Andere kompatible Modelle (gleiche Label-Reihenfolge) funktionieren
//! durch reines Austauschen der Dateien in `$EXE_DIR/models/`.
//!
//! # Lifecycle
//!
//! Single-instance pro Prozess, lazy-init beim ersten Aufruf. Wenn das
//! Laden fehlschlägt (Datei fehlt, ORT-DLL fehlt, Modell-Schema-Mismatch),
//! liefert [`classify`] immer einen leeren Slice → die App funktioniert
//! ohne NER weiter (L1/L2 reichen für strukturierte Daten).
//!
//! # Feature-Flag
//!
//! Aktiviert mit `cargo build --features ner`. Ohne Feature ist
//! [`classify`] eine kostenlose No-Op, der ONNX-Runtime wird gar nicht
//! erst gelinkt. So bleibt der Default-Build schlank und ohne externe
//! Native-Dependency.

use serde::Serialize;

/// Output-Struktur eines NER-Findings. Wird in der `detection`-Pipeline
/// zu einem `Finding` mit Entity-Type Person/Location/Organization
/// konvertiert.
#[derive(Debug, Clone, Serialize)]
pub struct NerFinding {
    /// `"person"`, `"location"` oder `"organization"` (Mapping aus dem
    /// Modell-Label-Set, B-/I-Prefix entfernt).
    pub entity_type: String,
    /// Char-Offset im Eingabetext (UTF-8-Byte-Offset, kompatibel mit
    /// `&str`-Slicing).
    pub start: usize,
    pub end: usize,
    /// Der erkannte Span-Text (für Debugging und UI-Anzeige).
    pub text: String,
    /// Aggregierte Confidence des Spans, ∈ [0, 1]. Niedrige Werte
    /// (z. B. < 0.7) filtert die `detection`-Pipeline aus.
    pub confidence: f32,
}

/// Schwellwert für Confidence — Findings darunter werden verworfen.
/// Empirisch gewählt; bei zu vielen False Positives kann man's erhöhen.
#[cfg_attr(not(feature = "ner"), allow(dead_code))]
pub const MIN_CONFIDENCE: f32 = 0.70;

/// Hauptfunktion: liefert NER-Findings für `text`. Bei deaktiviertem
/// Feature-Flag oder Modell-Load-Fehler → leerer Vektor.
pub fn classify(text: &str) -> Vec<NerFinding> {
    inference::classify(text)
}

/// Diagnostik-Hilfsfunktion: ist die Engine geladen und einsatzbereit?
/// Hauptsächlich für das Settings-UI und Logging.
pub fn is_ready() -> bool {
    inference::is_ready()
}

// =================================================================== Real Impl (feature = "ner")

#[cfg(feature = "ner")]
mod inference {
    use super::{NerFinding, MIN_CONFIDENCE};
    use ndarray::Array2;
    use once_cell::sync::OnceCell;
    use ort::session::Session;
    use ort::value::Value;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tokenizers::Tokenizer;

    /// Label-IDs des Xenova/Davlan-Modells. Reihenfolge **muss exakt**
    /// zum `id2label` aus der `config.json` des Modells passen, sonst
    /// werden Token-Klassen falsch interpretiert. Siehe
    /// `models/README.md` für die verifizierte Quelle.
    ///
    /// Bei einem Modellwechsel hier UND in `aggregate_spans` (Mapping
    /// auf EntityType) anpassen.
    const LABELS: &[&str] = &[
        "O",        // 0
        "B-DATE",   // 1
        "I-DATE",   // 2
        "B-PER",    // 3
        "I-PER",    // 4
        "B-ORG",    // 5
        "I-ORG",    // 6
        "B-LOC",    // 7
        "I-LOC",    // 8
    ];

    struct NerEngine {
        // Session::run braucht &mut self, deshalb Mutex. Inference ist
        // im Hot-Path (~50 ms), aber wir haben effektiv nur eine Anfrage
        // pro Strg+B-Druck — Contention ist daher kein Thema.
        session: Mutex<Session>,
        tokenizer: Tokenizer,
    }

    /// Process-weiter Single-Instance-Cache. `OnceCell<Option<…>>` weil
    /// auch das „Laden ist fehlgeschlagen"-Ergebnis nur einmal versucht
    /// werden soll — sonst hätten wir on every call retries.
    static ENGINE: OnceCell<Option<NerEngine>> = OnceCell::new();

    fn engine() -> Option<&'static NerEngine> {
        ENGINE
            .get_or_init(|| {
                // catch_unwind: ORT-FFI panickt bei manchen Init-Fehlern
                // (DLL-Version-Mismatch, Modell-Schema, fehlende MSVC-
                // Runtime, …). Ohne catch_unwind würde dieser Panic
                // den ganzen Prozess killen — App starte nicht mal mehr
                // zum Tray-Icon. Mit catch_unwind degradieren wir L3
                // auf no-op und die App läuft mit L1+L2 weiter.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(try_load));
                match result {
                    Ok(Ok(e)) => {
                        log::info!("ner: engine loaded successfully");
                        Some(e)
                    }
                    Ok(Err(e)) => {
                        log::warn!("ner: engine load failed: {e:#} — L3 will be no-op");
                        None
                    }
                    Err(panic_payload) => {
                        let msg = panic_payload
                            .downcast_ref::<&'static str>()
                            .copied()
                            .or_else(|| {
                                panic_payload.downcast_ref::<String>().map(|s| s.as_str())
                            })
                            .unwrap_or("<non-string panic>");
                        log::error!(
                            "ner: engine load PANICKED: {msg} — L3 disabled, App läuft weiter"
                        );
                        None
                    }
                }
            })
            .as_ref()
    }

    fn try_load() -> anyhow::Result<NerEngine> {
        let models_dir = models_dir()?;
        let model_path = models_dir.join("model.onnx");
        let tokenizer_path = models_dir.join("tokenizer.json");
        let manifest_path = models_dir.join("MANIFEST.sha256");

        anyhow::ensure!(
            model_path.exists(),
            "NER model file not found: {} — run scripts/download-ner-model.{{ps1,sh}}",
            model_path.display()
        );
        anyhow::ensure!(
            tokenizer_path.exists(),
            "NER tokenizer file not found: {}",
            tokenizer_path.display()
        );

        verify_manifest(&manifest_path, &models_dir)?;

        let session = Session::builder()?
            .commit_from_file(&model_path)
            .map_err(|e| anyhow::anyhow!("onnx session: {e}"))?;
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("tokenizer load: {e}"))?;

        Ok(NerEngine {
            session: Mutex::new(session),
            tokenizer,
        })
    }

    /// Liest `MANIFEST.sha256` und vergleicht die erwarteten Hashes mit den
    /// tatsächlichen SHA-256-Hashes der Dateien.
    ///
    /// Format pro Zeile (kompatibel zu `sha256sum`):
    /// ```text
    /// <hex-hash>  <relative_path>
    /// ```
    /// Zeilen, die mit `#` beginnen, sowie Leerzeilen werden ignoriert.
    ///
    /// **Strikt:** wenn das Manifest fehlt, schlägt das Laden fehl. Wir
    /// wollen kein „silently disabled" — wenn jemand die Files manuell
    /// austauscht ohne das Manifest mitzupflegen, soll das Auffallen.
    /// Workaround für Dev-Setups: `sha256sum models/*.{onnx,json} >
    /// models/MANIFEST.sha256` einmal nach dem Download.
    fn verify_manifest(
        manifest_path: &std::path::Path,
        models_dir: &std::path::Path,
    ) -> anyhow::Result<()> {
        use sha2::{Digest, Sha256};

        anyhow::ensure!(
            manifest_path.exists(),
            "NER manifest fehlt: {} — Setup-Script erzeugt diese Datei mit den \
             erwarteten Hashes. Ohne Manifest verweigern wir den Modell-Load \
             (Supply-Chain-Schutz).",
            manifest_path.display()
        );

        let content = std::fs::read_to_string(manifest_path)?;
        // PowerShell's `Set-Content -Encoding utf8` schreibt UTF-8 mit BOM.
        // Das BOM würde sonst die erste Zeile vor „#" stellen und unseren
        // Kommentar-Check brechen.
        let content = content.strip_prefix('\u{FEFF}').unwrap_or(&content);
        let mut entries = 0;
        for (lineno, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(2, char::is_whitespace);
            let expected = parts
                .next()
                .ok_or_else(|| anyhow::anyhow!("manifest:{}: missing hash", lineno + 1))?;
            let rel = parts
                .next()
                .ok_or_else(|| anyhow::anyhow!("manifest:{}: missing path", lineno + 1))?
                .trim();

            let file_path = models_dir.join(rel);
            let bytes = std::fs::read(&file_path)
                .map_err(|e| anyhow::anyhow!("read {}: {e}", file_path.display()))?;
            let actual = hex::encode(Sha256::digest(&bytes));

            anyhow::ensure!(
                actual.eq_ignore_ascii_case(expected),
                "manifest mismatch für {}: erwartet {}, gefunden {}",
                file_path.display(),
                expected,
                actual
            );
            entries += 1;
        }
        anyhow::ensure!(
            entries > 0,
            "manifest ist leer — mindestens model.onnx + tokenizer.json eintragen"
        );
        log::info!("ner: manifest verified ({} files)", entries);
        Ok(())
    }

    /// Sucht das `models/`-Verzeichnis. Mehrere Locations werden in
    /// dieser Reihenfolge probiert:
    ///
    /// 1. `<EXE_DIR>/models/` — Win-Production-MSI bündelt direkt neben EXE
    /// 2. `<EXE_DIR>/../Resources/models/` — macOS-App-Bundle
    ///    (`Streichzeug.app/Contents/MacOS/streichzeug` →
    ///    `Streichzeug.app/Contents/Resources/models/`)
    /// 3. `<LOCALAPPDATA>/de.streichzeug.app/models/` — Beta-Setup
    ///    in userbeschreibbarem Verzeichnis (auf Win), bzw.
    ///    `~/Library/Application Support/…` auf macOS
    /// 4. `<EXE_DIR>/../../models/` — Dev-Build via `cargo build`
    ///
    /// Erster Treffer mit existierendem `model.onnx` gewinnt. Fehlt's
    /// überall, liefern wir alle Pfade im Fehler — der User sieht direkt,
    /// wo er das Modell hinlegen kann.
    fn models_dir() -> anyhow::Result<PathBuf> {
        let exe = std::env::current_exe()?;
        let exe_dir = exe
            .parent()
            .ok_or_else(|| anyhow::anyhow!("no exe parent dir"))?;

        let mut candidates: Vec<PathBuf> = vec![
            exe_dir.join("models"),
            exe_dir.join("..").join("Resources").join("models"),
        ];
        if let Some(local) = dirs::data_local_dir() {
            candidates.push(local.join("de.streichzeug.app").join("models"));
        }
        candidates.push(exe_dir.join("..").join("..").join("models"));

        for path in &candidates {
            if path.join("model.onnx").exists() {
                log::info!("ner: models directory resolved to {}", path.display());
                return Ok(path.clone());
            }
        }
        let formatted = candidates
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!(
            "kein models/ Verzeichnis mit model.onnx gefunden. Probierte Pfade:\n{formatted}"
        )
    }

    pub(super) fn is_ready() -> bool {
        engine().is_some()
    }

    pub(super) fn classify(text: &str) -> Vec<NerFinding> {
        let Some(eng) = engine() else {
            return vec![];
        };
        match run_inference(eng, text) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("ner: inference failed: {e:#}");
                vec![]
            }
        }
    }

    fn run_inference(eng: &NerEngine, text: &str) -> anyhow::Result<Vec<NerFinding>> {
        // 1) Tokenisieren mit Word-Offsets, damit wir Spans später
        //    zurück auf Char-Positionen mappen können.
        let encoding = eng
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("encode: {e}"))?;

        let ids = encoding.get_ids();
        let mask = encoding.get_attention_mask();
        let offsets = encoding.get_offsets();
        let seq_len = ids.len();
        if seq_len == 0 {
            return Ok(vec![]);
        }

        let input_ids = Array2::from_shape_vec(
            (1, seq_len),
            ids.iter().map(|&x| x as i64).collect(),
        )?;
        let attention_mask = Array2::from_shape_vec(
            (1, seq_len),
            mask.iter().map(|&x| x as i64).collect(),
        )?;

        // 2) Forward-Pass. Input-Namen sind bei BERT-Familien-Modellen
        //    konventionell „input_ids" und „attention_mask".
        //
        // `ort::inputs![…]` liefert ein `Vec<(Cow<str>, SessionInputValue)>`
        // (kein Result), `from_array` ein `Result`. Nur `Session::run` ist
        // fehlerbehaftet. Session läuft hinter Mutex, weil run() &mut nimmt.
        let mut session = eng
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("session mutex poisoned"))?;
        let outputs = session.run(ort::inputs![
            "input_ids" => Value::from_array(input_ids)?,
            "attention_mask" => Value::from_array(attention_mask)?,
        ])?;

        // 3) Logits extrahieren — [1, seq_len, num_labels].
        //
        // `try_extract_tensor` (in ort 2.0-rc.10) gibt `(Shape, &[T])`
        // zurück — Shape ist ein `&[i64]` mit den Dimensionen.
        let (shape, logits) = outputs[0].try_extract_tensor::<f32>()?;
        let num_labels = LABELS.len();
        anyhow::ensure!(
            logits.len() == seq_len * num_labels,
            "logits shape mismatch: got {} elements (shape {:?}), expected seq_len*num_labels = {}*{}",
            logits.len(),
            shape,
            seq_len,
            num_labels
        );

        // 4) Pro Token: Softmax + Argmax → Label + Confidence.
        let mut token_labels: Vec<(usize, f32)> = Vec::with_capacity(seq_len);
        for t in 0..seq_len {
            let row = &logits[t * num_labels..(t + 1) * num_labels];
            let (idx, conf) = softmax_argmax(row);
            token_labels.push((idx, conf));
        }

        // 5) B-/I- Spans aggregieren → Findings.
        Ok(aggregate_spans(text, offsets, &token_labels))
    }

    fn softmax_argmax(row: &[f32]) -> (usize, f32) {
        let max = row.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let mut sum = 0.0f32;
        let exps: Vec<f32> = row
            .iter()
            .map(|&v| {
                let e = (v - max).exp();
                sum += e;
                e
            })
            .collect();
        let mut best_idx = 0usize;
        let mut best_val = f32::NEG_INFINITY;
        for (i, &e) in exps.iter().enumerate() {
            if e > best_val {
                best_val = e;
                best_idx = i;
            }
        }
        (best_idx, best_val / sum)
    }

    fn aggregate_spans(
        text: &str,
        offsets: &[(usize, usize)],
        token_labels: &[(usize, f32)],
    ) -> Vec<NerFinding> {
        let mut out = Vec::new();
        let mut current: Option<(String, usize, usize, Vec<f32>)> = None;

        for (i, &(label_idx, conf)) in token_labels.iter().enumerate() {
            let label = LABELS.get(label_idx).copied().unwrap_or("O");
            let (offset_start, offset_end) = offsets[i];
            // Special-Tokens haben (0,0) als Offset — überspringen.
            if offset_start == 0 && offset_end == 0 && i != 0 {
                continue;
            }

            if label == "O" {
                if let Some(span) = current.take() {
                    push_span(&mut out, text, span);
                }
                continue;
            }

            let (prefix, kind) = label.split_at(1);
            // kind wird "-PER", "-ORG", "-LOC", "-DATE" — Bindestrich abschneiden.
            let kind = kind.trim_start_matches('-');
            let entity = match kind {
                "PER" => "person",
                "ORG" => "organization",
                "LOC" => "location",
                "DATE" => "date",
                _ => continue,
            };

            if prefix == "B" || current.is_none() {
                if let Some(span) = current.take() {
                    push_span(&mut out, text, span);
                }
                current = Some((entity.to_string(), offset_start, offset_end, vec![conf]));
            } else {
                // I-Prefix — Fortsetzung. Aber nur, wenn die Entity zum
                // bisher offenen Span passt; sonst neuen Span eröffnen.
                if let Some(span) = current.as_mut() {
                    if span.0 == entity {
                        span.2 = offset_end;
                        span.3.push(conf);
                    } else {
                        let old = current.take().unwrap();
                        push_span(&mut out, text, old);
                        current =
                            Some((entity.to_string(), offset_start, offset_end, vec![conf]));
                    }
                }
            }
        }
        if let Some(span) = current.take() {
            push_span(&mut out, text, span);
        }
        out
    }

    fn push_span(
        out: &mut Vec<NerFinding>,
        text: &str,
        (entity_type, start, end, confs): (String, usize, usize, Vec<f32>),
    ) {
        if start >= end || end > text.len() {
            return;
        }
        // Confidence = geometrisches Mittel aller Token-Confidences. Damit
        // wirken kurze, sehr sichere Spans nicht künstlich nieder durch
        // einen schwachen Sub-Token-Match.
        let conf = if confs.is_empty() {
            0.0
        } else {
            let prod: f32 = confs.iter().product();
            prod.powf(1.0 / confs.len() as f32)
        };
        if conf < MIN_CONFIDENCE {
            return;
        }
        let Some(span_text) = text.get(start..end) else {
            return;
        };
        out.push(NerFinding {
            entity_type,
            start,
            end,
            text: span_text.to_string(),
            confidence: conf,
        });
    }
}

// =================================================================== No-Op Impl (feature off)

#[cfg(not(feature = "ner"))]
mod inference {
    use super::NerFinding;

    pub(super) fn classify(_text: &str) -> Vec<NerFinding> {
        Vec::new()
    }

    pub(super) fn is_ready() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_op_without_feature_returns_empty() {
        // Egal ob Feature an oder aus: bei fehlendem Modell ist das
        // Ergebnis ein leerer Vec. Test prüft die Vertrags-Garantie.
        let result = classify("Jürgen Sonder arbeitet bei der Sparkasse Dortmund.");
        // Ohne Modell-File → leer; mit Modell-File → ggf. nicht-leer.
        // Wir können nicht beides assertieren, also testen wir nur, dass
        // der Aufruf nicht paniciert.
        let _ = result;
    }

    #[test]
    fn is_ready_does_not_panic() {
        let _ = is_ready();
    }

    /// Latenz-Mikro-Benchmark mit einer realistisch langen DE-Email.
    ///
    /// **Manuell ausgeführt** via:
    /// ```text
    /// cargo test --features ner --release -- --ignored \
    ///     bench_ner_latency_npl_email --nocapture
    /// ```
    ///
    /// Erwartung auf typischer Office-CPU (Ryzen 7 / Apple M-Serie):
    ///   - Cold-start (mit Modell-Load): 300–500 ms
    ///   - Warm classify():               40–100 ms (p50)
    ///                                   < 200 ms (p99)
    ///
    /// Wird mit `#[ignore]` versehen, weil:
    ///   1) ohne Modell-Files No-Op (uninteressant)
    ///   2) mit Modell läuft's mehrere Sekunden für 100 Iterationen,
    ///      sollte nicht jeden CI-Test blockieren
    #[test]
    #[ignore = "benchmark — manuell starten, braucht NER-Modell"]
    fn bench_ner_latency_npl_email() {
        let sample = include_str!("../tests/fixtures/npl_email_sample.txt");

        if !is_ready() {
            eprintln!(
                "⚠ NER-Engine nicht ready — Bench skipped. \
                 Modell in src-tauri/models/ ablegen und mit --features ner bauen."
            );
            return;
        }

        // Warm-up — JIT/Cache von ORT, erste Inferenz ist immer langsamer.
        for _ in 0..3 {
            let _ = classify(sample);
        }

        let iters = 50;
        let mut durations: Vec<std::time::Duration> = Vec::with_capacity(iters);
        for _ in 0..iters {
            let start = std::time::Instant::now();
            let findings = classify(sample);
            durations.push(start.elapsed());
            // Mindestens irgendwas erkennen, sonst stimmt die Pipeline nicht.
            assert!(
                !findings.is_empty(),
                "expected at least one NER finding on NPL sample"
            );
        }
        durations.sort();
        let p50 = durations[iters / 2];
        let p99 = durations[(iters as f32 * 0.99) as usize];
        let mean: u128 = durations.iter().map(|d| d.as_micros()).sum::<u128>() / iters as u128;

        eprintln!("NER Bench (NPL-Email, {} Iter., {} Zeichen):", iters, sample.len());
        eprintln!("  Mean:  {:.1} ms", mean as f64 / 1000.0);
        eprintln!("  p50:   {:.1} ms", p50.as_micros() as f64 / 1000.0);
        eprintln!("  p99:   {:.1} ms", p99.as_micros() as f64 / 1000.0);

        // Sanity-Schranke: ein einzelner Inference-Call darf nicht
        // > 1 s dauern — sonst ist irgendwas grundlegend kaputt.
        assert!(
            p99.as_millis() < 1000,
            "NER inference p99 zu hoch: {:?}",
            p99
        );
    }
}
