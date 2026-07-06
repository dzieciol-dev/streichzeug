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

/// Laufzeit-Gate der NER-Schicht — gespiegelt aus `Settings.enable_ner`
/// beim App-Start und bei jedem Settings-Save. Vorher wurde das Setting
/// zur Laufzeit NIRGENDS geprüft: lagen Modell-Dateien vor, lief NER auch
/// abgeschaltet mit; fehlten sie, bewirkte der Haken nichts (Beta-Befund
/// 2026-07-06). Der Wert lebt hier statt in einem Settings-Reload pro
/// Detection-Aufruf — kein Datei-I/O im Hot-Path.
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Setzt das Laufzeit-Gate (App-Start, Settings-Save, Erkennung-Tab).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn is_enabled() -> bool {
    ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Hauptfunktion: liefert NER-Findings für `text`. Bei abgeschaltetem
/// Setting, deaktiviertem Feature-Flag oder Modell-Load-Fehler → leerer
/// Vektor. Das Setting wirkt SOFORT (kein Neustart) — siehe [`set_enabled`].
pub fn classify(text: &str) -> Vec<NerFinding> {
    if !is_enabled() {
        return Vec::new();
    }
    inference::classify(text)
}

/// Diagnostik-Hilfsfunktion: ist die Engine geladen und einsatzbereit?
/// Hauptsächlich für das Settings-UI und Logging.
pub fn is_ready() -> bool {
    inference::is_ready()
}

/// Erzwingt einen (erneuten) Lade-Versuch der Engine — nach einem
/// Modell-Download oder beim Aktivieren im Erkennung-Tab. Anders als der
/// Lazy-Load in [`classify`] probiert das auch nach einem früheren
/// Fehlschlag erneut: der Fehlschlag-Cache existiert nur, damit der
/// Hot-Path (jeder Hotkey-Druck) keine Retry-Stürme fährt — ein bewusster
/// Klick im UI darf es wieder versuchen (vorher cachte ein `OnceCell`
/// auch den Fehlschlag für immer, und ein Runtime-Download konnte die
/// Engine bis zum Neustart nie mehr aktivieren).
pub fn ensure_loaded() -> bool {
    inference::ensure_loaded()
}

/// Plattform-spezifischer Dateiname der ONNX-Runtime-Shared-Library.
/// Immer kompiliert (auch ohne `ner`-Feature), damit Existenz- und
/// Manifest-Checks in [`model_files_present`] plattformkonsistent bleiben.
pub(crate) fn native_lib_filename() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "libonnxruntime.dylib"
    }
    #[cfg(target_os = "windows")]
    {
        "onnxruntime.dll"
    }
    #[cfg(target_os = "linux")]
    {
        "libonnxruntime.so"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        "libonnxruntime.so"
    }
}

/// Prüft, ob die Modell-Files lokal liegen (Status für Onboarding/UI).
/// Schaut im User-Daten-Verzeichnis nach Modell, Tokenizer, nativer
/// ORT-Library und Manifest. Reine Existenzprüfung, **keine**
/// Hash-Verifikation — die übernimmt der Engine-Loader.
///
/// Zusätzlich: das Manifest muss die native ORT-Library listen. Alte
/// Manifeste (vor der Lib-Hash-Härtung) enthalten keinen Lib-Eintrag —
/// die behandeln wir hier bewusst als „nicht vollständig vorhanden", damit
/// Onboarding/UI einen Re-Download anbieten statt eine unverifizierte Lib
/// zu laden.
pub fn model_files_present() -> bool {
    let Some(dir) = user_models_dir() else { return false };
    let manifest = dir.join("MANIFEST.sha256");
    dir.join("model.onnx").exists()
        && dir.join("tokenizer.json").exists()
        && dir.join(native_lib_filename()).exists()
        && manifest.exists()
        && manifest_lists_native_lib(&manifest)
}

/// Liest das Manifest und prüft, ob es eine Zeile für die native
/// ORT-Library ([`native_lib_filename`]) enthält. Toleriert BOM und
/// Kommentarzeilen (gleiches Format wie [`inference::verify_manifest`]).
/// Bei Lesefehler → `false` (behandeln wie „fehlt").
fn manifest_lists_native_lib(manifest_path: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(manifest_path) else {
        return false;
    };
    let content = content.strip_prefix('\u{FEFF}').unwrap_or(&content);
    let lib = native_lib_filename();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Format: "<hash>  <relative-path>" — Pfad ist das zweite Feld.
        if let Some((_, rel)) = line.split_once(char::is_whitespace) {
            let rel = rel.trim();
            // Vergleich über den Dateinamen, robust gegen "./"-Präfixe.
            if std::path::Path::new(rel)
                .file_name()
                .and_then(|n| n.to_str())
                == Some(lib)
            {
                return true;
            }
        }
    }
    false
}

/// Zielverzeichnis für [`download_models`]: `<LOCALAPPDATA>/de.streichzeug.app/models/`.
/// Macht das Verzeichnis auf Abruf hin sichtbar — der Caller kann's für
/// User-Feedback nutzen („Modell wird nach … geladen").
pub fn user_models_dir() -> Option<std::path::PathBuf> {
    dirs::data_local_dir().map(|d| d.join("de.streichzeug.app").join("models"))
}

/// Lädt Modell-, Tokenizer- und ONNX-Runtime-Files in den User-Daten-Pfad.
///
/// Quelle: HuggingFace + Microsoft GitHub-Releases. Es wird **nichts** im
/// Bundle mitgeliefert — der User triggert den Download explizit über
/// das Onboarding oder den UI-Button. Damit ist die App-Distribution
/// klar nur „Code", die Lizenz-Last für Modell + ORT bleibt beim
/// Original-Distributor.
///
/// Aufruf nur mit aktivem `ner`-Feature sinnvoll — ohne das Feature ist
/// die Funktion eine kostenpflichtige No-Op-Fehler-Antwort, damit
/// Frontend-Code denselben Pfad nutzen kann.
pub async fn download_models() -> Result<std::path::PathBuf, String> {
    #[cfg(feature = "ner")]
    {
        downloader::run().await.map_err(|e| format!("{e:#}"))
    }
    #[cfg(not(feature = "ner"))]
    {
        Err("NER-Feature ist in diesem Build nicht aktiviert (kompiliere mit --features ner).".to_string())
    }
}

// =================================================================== Real Impl (feature = "ner")

#[cfg(feature = "ner")]
mod inference {
    use super::{NerFinding, MIN_CONFIDENCE};
    use ndarray::Array2;
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

    /// Process-weiter Engine-Slot. `Unavailable` merkt sich einen
    /// Fehlschlag, damit der Hot-Path (jeder Hotkey-Druck) keine
    /// Retry-Stürme fährt — anders als das frühere `OnceCell` ist der
    /// Zustand aber über [`ensure_loaded`] zurücksetzbar: nach einem
    /// Runtime-Modell-Download muss die Engine OHNE Neustart ladbar sein.
    enum EngineSlot {
        Untried,
        Unavailable,
        // Box: clippy::large_enum_variant — die Engine (Session+Tokenizer,
        // ~1 KB Struct) soll die leeren Varianten nicht aufblähen.
        Ready(Box<NerEngine>),
    }

    static ENGINE: std::sync::RwLock<EngineSlot> = std::sync::RwLock::new(EngineSlot::Untried);

    /// Führt `f` mit der geladenen Engine aus; lädt beim ersten Aufruf
    /// lazy. `None`, wenn die Engine (weiterhin) nicht verfügbar ist.
    fn with_engine<R>(f: impl FnOnce(&NerEngine) -> R) -> Option<R> {
        {
            let slot = ENGINE.read().expect("ENGINE lock poisoned");
            match &*slot {
                EngineSlot::Ready(engine) => return Some(f(engine)),
                EngineSlot::Unavailable => return None,
                EngineSlot::Untried => {}
            }
        }
        let mut slot = ENGINE.write().expect("ENGINE lock poisoned");
        if matches!(*slot, EngineSlot::Untried) {
            *slot = load_slot();
        }
        match &*slot {
            EngineSlot::Ready(engine) => Some(f(engine)),
            _ => None,
        }
    }

    /// Expliziter (Re-)Ladeversuch — auch nach früherem Fehlschlag.
    pub(super) fn ensure_loaded() -> bool {
        {
            let slot = ENGINE.read().expect("ENGINE lock poisoned");
            if matches!(&*slot, EngineSlot::Ready(_)) {
                return true;
            }
        }
        let mut slot = ENGINE.write().expect("ENGINE lock poisoned");
        if matches!(&*slot, EngineSlot::Ready(_)) {
            return true;
        }
        *slot = load_slot();
        matches!(&*slot, EngineSlot::Ready(_))
    }

    /// Ein Lade-Versuch → Slot-Zustand. Der `catch_unwind` fängt Panics
    /// INNERHALB des Locks — der Lock wird dadurch nie vergiftet.
    fn load_slot() -> EngineSlot {
        // catch_unwind: ORT-FFI panickt bei manchen Init-Fehlern
        // (DLL-Version-Mismatch, Modell-Schema, fehlende MSVC-Runtime, …).
        // Ohne catch_unwind würde dieser Panic den ganzen Prozess killen —
        // App startete nicht mal mehr zum Tray-Icon. Mit catch_unwind
        // degradieren wir L3 auf no-op und die App läuft mit L1+L2 weiter.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(try_load));
        match result {
            Ok(Ok(engine)) => {
                log::info!("ner: engine loaded successfully");
                EngineSlot::Ready(Box::new(engine))
            }
            Ok(Err(e)) => {
                log::warn!("ner: engine load failed: {e:#} — L3 will be no-op");
                EngineSlot::Unavailable
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
                EngineSlot::Unavailable
            }
        }
    }

    fn try_load() -> anyhow::Result<NerEngine> {
        let models_dir = models_dir()?;
        let model_path = models_dir.join("model.onnx");
        let tokenizer_path = models_dir.join("tokenizer.json");
        let manifest_path = models_dir.join("MANIFEST.sha256");

        anyhow::ensure!(
            model_path.exists(),
            "NER model file not found: {} — bitte erst in der App das NER-Modell herunterladen",
            model_path.display()
        );
        anyhow::ensure!(
            tokenizer_path.exists(),
            "NER tokenizer file not found: {}",
            tokenizer_path.display()
        );

        // ORT-Shared-Library zur Laufzeit lokalisieren. `load-dynamic` lädt
        // die Lib aus dem Pfad in `ORT_DYLIB_PATH`. Wir setzen das vor
        // `Session::builder()`, damit ort die heruntergeladene Lib aus
        // dem User-Daten-Pfad findet — nicht ins App-Bundle gepackt.
        let dylib_path = dylib_path(&models_dir);
        anyhow::ensure!(
            dylib_path.exists(),
            "ORT shared library not found: {} — bitte erst in der App das NER-Modell herunterladen",
            dylib_path.display()
        );
        std::env::set_var("ORT_DYLIB_PATH", &dylib_path);
        log::info!("ner: ORT_DYLIB_PATH = {}", dylib_path.display());

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

    /// Plattform-spezifischer Filename der ONNX-Runtime-Shared-Library.
    fn dylib_path(models_dir: &std::path::Path) -> PathBuf {
        models_dir.join(super::native_lib_filename())
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
        let native_lib = super::native_lib_filename();
        let mut saw_native_lib = false;
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
            if std::path::Path::new(rel).file_name().and_then(|n| n.to_str()) == Some(native_lib) {
                saw_native_lib = true;
            }
            entries += 1;
        }
        anyhow::ensure!(
            entries > 0,
            "manifest ist leer — mindestens model.onnx + tokenizer.json eintragen"
        );
        // Migration/Härtung: Die native ORT-Library wird per ORT_DYLIB_PATH
        // geladen und ist damit Teil der Supply-Chain. Ohne Hash im Manifest
        // würden wir sie nur auf Existenz prüfen — genau das soll nicht mehr
        // passieren. Alte Manifeste (vor dieser Härtung) listen die Lib nicht:
        // wir brechen NICHT hart (der Aufrufer degradiert L3 ohnehin graceful
        // auf No-Op), signalisieren aber klar, dass ein Re-Download nötig ist.
        anyhow::ensure!(
            saw_native_lib,
            "Manifest enthält keinen Hash für die ONNX-Runtime-Library ({native_lib}) — \
             vermutlich ein altes Manifest vor der Lib-Hash-Härtung. Bitte das NER-Modell \
             neu laden (Re-Download nötig)."
        );
        log::info!("ner: manifest verified ({} files)", entries);
        Ok(())
    }

    /// Sucht das `models/`-Verzeichnis. Standard-Pfad (Public-Release) ist
    /// das User-Daten-Verzeichnis — dorthin lädt [`super::download_models`]
    /// die Files. Dev-Pfade in `src-tauri/models/` werden als Fallback
    /// für lokale Entwicklung weiter unterstützt.
    ///
    /// Reihenfolge:
    /// 1. `<LOCALAPPDATA>/de.streichzeug.app/models/` — der Pfad, in den
    ///    die App selbst per `download_models` lädt
    /// 2. `<EXE_DIR>/models/` — Dev-Build, wenn jemand
    ///    `scripts/download-ner-model.sh` manuell ausgeführt hat
    /// 3. `<EXE_DIR>/../../models/` — Dev-Build via `cargo build` aus
    ///    `src-tauri/target/`
    ///
    /// Erster Treffer mit existierender `model.onnx` gewinnt.
    fn models_dir() -> anyhow::Result<PathBuf> {
        let exe = std::env::current_exe()?;
        let exe_dir = exe
            .parent()
            .ok_or_else(|| anyhow::anyhow!("no exe parent dir"))?;

        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(local) = dirs::data_local_dir() {
            candidates.push(local.join("de.streichzeug.app").join("models"));
        }
        candidates.push(exe_dir.join("models"));
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
        with_engine(|_| ()).is_some()
    }

    pub(super) fn classify(text: &str) -> Vec<NerFinding> {
        with_engine(|engine| match run_inference_chunked(engine, text) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("ner: inference failed: {e:#}");
                vec![]
            }
        })
        .unwrap_or_default()
    }

    /// Positions-Limit des DistilBERT-Modells (Positional Embeddings).
    /// Längere Eingaben MÜSSEN gefenstert werden — vorher scheiterte die
    /// Inferenz an >512 Tokens KOMPLETT (ORT-Broadcast-Fehler „512 by 725")
    /// und NER lieferte für normale lange Mails still gar nichts
    /// (Beta-Befund 2026-07-06).
    const MAX_MODEL_TOKENS: usize = 512;
    /// Nutz-Tokens pro Fenster — Puffer für [CLS]/[SEP]-Sondertokens, die
    /// `encode(text, true)` in [`run_inference`] zusätzlich anfügt.
    const CHUNK_TOKENS: usize = 480;
    /// Überlappung zwischen Fenstern, damit Entities an Schnittkanten
    /// nicht halbiert werden.
    const CHUNK_OVERLAP_TOKENS: usize = 48;

    /// Fenstert lange Texte auf Modell-taugliche Stücke und führt die
    /// Findings (Byte-Offsets zurückverschoben) zusammen. Kurze Texte gehen
    /// direkt durch. Identische Doppel-Findings aus der Überlappung werden
    /// hier dedupliziert; teilweise überlappende entschärft downstream
    /// `detection::dedupe_and_sort` (längerer Span gewinnt).
    fn run_inference_chunked(eng: &NerEngine, text: &str) -> anyhow::Result<Vec<NerFinding>> {
        // Encoding OHNE Sondertokens — nur für die Schnittpunkte (die
        // Offsets sind Byte-Offsets an Token-Grenzen, also Char-sicher).
        let encoding = eng
            .tokenizer
            .encode(text, false)
            .map_err(|e| anyhow::anyhow!("encode (chunking): {e}"))?;
        let offsets = encoding.get_offsets();
        if offsets.len() + 2 <= MAX_MODEL_TOKENS {
            return run_inference(eng, text);
        }
        log::info!(
            "ner: Text hat {} Tokens — fenstere in {}er-Stücke (Überlappung {})",
            offsets.len(),
            CHUNK_TOKENS,
            CHUNK_OVERLAP_TOKENS
        );

        let mut findings: Vec<NerFinding> = Vec::new();
        let mut start_tok = 0usize;
        loop {
            let end_tok = (start_tok + CHUNK_TOKENS).min(offsets.len());
            let byte_start = offsets[start_tok].0;
            let byte_end = if end_tok == offsets.len() {
                text.len()
            } else {
                offsets[end_tok].0
            };
            let chunk = &text[byte_start..byte_end];
            for mut f in run_inference(eng, chunk)? {
                f.start += byte_start;
                f.end += byte_start;
                let duplicate = findings.iter().any(|g| {
                    g.start == f.start && g.end == f.end && g.entity_type == f.entity_type
                });
                if !duplicate {
                    findings.push(f);
                }
            }
            if end_tok == offsets.len() {
                break;
            }
            start_tok = end_tok.saturating_sub(CHUNK_OVERLAP_TOKENS);
        }
        Ok(findings)
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

    // ------------------------------------------------------------ Tests (Manifest-Härtung)
    #[cfg(test)]
    mod manifest_tests {
        use super::verify_manifest;
        use sha2::{Digest, Sha256};
        use std::path::{Path, PathBuf};

        /// Legt ein isoliertes temporäres models/-Verzeichnis an.
        fn temp_models_dir() -> PathBuf {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("streichzeug_manifest_test_{nanos}_{n}"));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> String {
            std::fs::write(dir.join(name), bytes).unwrap();
            hex::encode(Sha256::digest(bytes))
        }

        /// Schreibt model.onnx + tokenizer.json + native Lib mit korrekten
        /// Hashes und gibt deren Hex-Hashes zurück (model, tokenizer, lib).
        fn write_payload(dir: &Path) -> (String, String, String) {
            let lib = super::super::native_lib_filename();
            let model = write_file(dir, "model.onnx", b"fake-onnx-bytes");
            let tok = write_file(dir, "tokenizer.json", b"{\"fake\":\"tokenizer\"}");
            let libhash = write_file(dir, lib, b"fake-native-lib-bytes");
            (model, tok, libhash)
        }

        /// Vollständiges, korrektes Manifest inkl. Lib-Hash → verifiziert.
        #[test]
        fn verify_ok_with_lib_hash() {
            let dir = temp_models_dir();
            let (model, tok, libhash) = write_payload(&dir);
            let lib = super::super::native_lib_filename();
            let manifest = dir.join("MANIFEST.sha256");
            std::fs::write(
                &manifest,
                format!("# comment\n{model}  model.onnx\n{tok}  tokenizer.json\n{libhash}  {lib}\n"),
            )
            .unwrap();

            let res = verify_manifest(&manifest, &dir);
            assert!(res.is_ok(), "erwartete Ok, bekam: {res:?}");
        }

        /// Altes Manifest ohne Lib-Eintrag → Fehler, Hinweis auf Re-Download.
        /// Kein Hard-Break: der Aufrufer degradiert L3 auf No-Op — hier prüfen
        /// wir nur, dass klar als „Re-Download nötig" signalisiert wird.
        #[test]
        fn verify_fails_when_lib_hash_missing() {
            let dir = temp_models_dir();
            let (model, tok, _lib) = write_payload(&dir);
            let manifest = dir.join("MANIFEST.sha256");
            // Bewusst nur model + tokenizer, wie vor der Härtung.
            std::fs::write(
                &manifest,
                format!("{model}  model.onnx\n{tok}  tokenizer.json\n"),
            )
            .unwrap();

            let err = verify_manifest(&manifest, &dir).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("Re-Download"),
                "Fehlermeldung sollte Re-Download nennen, war: {msg}"
            );
        }

        /// Manipulierte native Lib (Hash im Manifest passt nicht) → Mismatch.
        #[test]
        fn verify_fails_on_lib_hash_mismatch() {
            let dir = temp_models_dir();
            let (model, tok, _libhash) = write_payload(&dir);
            let lib = super::super::native_lib_filename();
            let manifest = dir.join("MANIFEST.sha256");
            // Falscher (aber wohlgeformter) Hash für die Lib.
            let wrong = "0".repeat(64);
            std::fs::write(
                &manifest,
                format!("{model}  model.onnx\n{tok}  tokenizer.json\n{wrong}  {lib}\n"),
            )
            .unwrap();

            let err = verify_manifest(&manifest, &dir).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("mismatch"),
                "Fehlermeldung sollte Mismatch nennen, war: {msg}"
            );
        }
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

    pub(super) fn ensure_loaded() -> bool {
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
    /// - Cold-start (mit Modell-Load): 300–500 ms
    /// - Warm classify(): 40–100 ms (p50), < 200 ms (p99)
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

// =================================================================== Downloader

/// Holt Modell-, Tokenizer- und ONNX-Runtime-Files zur Laufzeit aus
/// HuggingFace und Microsoft-GitHub. Ziel ist das User-Daten-Verzeichnis,
/// **nicht** das App-Bundle — dadurch sind wir nicht der Re-Distributor
/// der Modell-Files (Davlan/AFL-3.0 + MS-ORT/MIT).
///
/// Aktiv nur mit `--features ner`. Ohne Feature ist die Funktion gar
/// nicht kompiliert; der Aufruf landet im No-Op-Fehler in `download_models`.
#[cfg(feature = "ner")]
mod downloader {
    use anyhow::{Context, Result};
    use futures_util::StreamExt;
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;
    use tokio::io::AsyncWriteExt;

    const MODEL_URL: &str = "https://huggingface.co/Xenova/distilbert-base-multilingual-cased-ner-hrl/resolve/main/onnx/model_quantized.onnx";
    const TOKENIZER_URL: &str = "https://huggingface.co/Xenova/distilbert-base-multilingual-cased-ner-hrl/resolve/main/tokenizer.json";
    fn ort_asset_url() -> &'static str {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        return "https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-osx-arm64-1.22.0.tgz";
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        return "https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-osx-x86_64-1.22.0.tgz";
        #[cfg(target_os = "windows")]
        return "https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-win-x64-1.22.0.zip";
        #[cfg(target_os = "linux")]
        return "https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-linux-x64-1.22.0.tgz";
    }

    /// Filename der ORT-Shared-Library für die aktuelle Plattform.
    fn ort_lib_name() -> &'static str {
        super::native_lib_filename()
    }

    pub(super) async fn run() -> Result<PathBuf> {
        let dir = super::user_models_dir()
            .ok_or_else(|| anyhow::anyhow!("kein User-Daten-Verzeichnis verfügbar"))?;
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("create_dir_all {}", dir.display()))?;

        log::info!("ner download: starting into {}", dir.display());

        let client = reqwest::Client::builder()
            .user_agent(concat!("streichzeug/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("reqwest client init")?;

        // 1. Modell-Datei (~145 MB)
        log::info!("ner download: model.onnx ({})", MODEL_URL);
        download_to_file(&client, MODEL_URL, &dir.join("model.onnx")).await?;

        // 2. Tokenizer
        log::info!("ner download: tokenizer.json ({})", TOKENIZER_URL);
        download_to_file(&client, TOKENIZER_URL, &dir.join("tokenizer.json")).await?;

        // 3. ORT-Shared-Library — entpacke aus .tgz/.zip
        let ort_url = ort_asset_url();
        log::info!("ner download: ORT runtime ({})", ort_url);
        let archive_path = dir.join("ort_archive.bin");
        download_to_file(&client, ort_url, &archive_path).await?;
        extract_ort_lib(&archive_path, &dir.join(ort_lib_name()))?;
        let _ = tokio::fs::remove_file(&archive_path).await;

        // 4. MANIFEST.sha256 erzeugen
        write_manifest(&dir)?;

        log::info!("ner download: complete → {}", dir.display());
        Ok(dir)
    }

    /// Streamt eine HTTP-Response in eine Datei. `tokio::fs::File` +
    /// `reqwest::Response::bytes_stream` halten den Speicher klein
    /// (Chunk-für-Chunk statt voller Body im RAM).
    async fn download_to_file(
        client: &reqwest::Client,
        url: &str,
        path: &std::path::Path,
    ) -> Result<()> {
        let resp = client
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("HTTP-Status für {url}"))?;
        let mut file = tokio::fs::File::create(path)
            .await
            .with_context(|| format!("create {}", path.display()))?;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("stream {url}"))?;
            file.write_all(&chunk)
                .await
                .with_context(|| format!("write {}", path.display()))?;
        }
        file.flush().await?;
        Ok(())
    }

    /// Entpackt die ORT-Shared-Library aus dem heruntergeladenen Archive.
    /// macOS/Linux: .tgz mit tar+gzip. Windows: .zip mit dem `zip`-Crate.
    fn extract_ort_lib(archive: &std::path::Path, target: &std::path::Path) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let file = std::fs::File::open(archive)
                .with_context(|| format!("open archive {}", archive.display()))?;
            let gz = flate2::read::GzDecoder::new(file);
            let mut tar = tar::Archive::new(gz);
            for entry in tar.entries()? {
                let mut entry = entry?;
                // NUR reguläre Dateien: Im ORT-Archiv ist der unversionierte
                // Name (libonnxruntime.dylib) ein SYMLINK auf die versionierte
                // Datei (libonnxruntime.1.22.0.dylib). Das frühere
                // entry.unpack() des Symlink-Eintrags erzeugte einen TOTEN
                // Link — der Manifest-Hash scheiterte dann mit ENOENT
                // (Beta-Befund 2026-07-06). Deshalb: den versionierten
                // Datei-Eintrag inhaltlich unter dem Zielnamen ablegen.
                if entry.header().entry_type() != tar::EntryType::Regular {
                    continue;
                }
                let path = entry.path()?.into_owned();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !is_ort_lib_file(name) {
                    continue;
                }
                // Etwaigen Alt-Zustand (toter Symlink aus früheren Downloads)
                // wegräumen — File::create würde sonst DURCH den Link schreiben.
                let _ = std::fs::remove_file(target);
                let mut out = std::fs::File::create(target)
                    .with_context(|| format!("create target {}", target.display()))?;
                std::io::copy(&mut entry, &mut out)
                    .with_context(|| format!("extract {name} → {}", target.display()))?;
                log::info!("ner download: extracted {} (aus {name})", target.display());
                return Ok(());
            }
            anyhow::bail!("ORT-Shared-Library nicht in Archive gefunden");
        }
        #[cfg(target_os = "windows")]
        {
            use std::io::copy;

            let file = std::fs::File::open(archive)
                .with_context(|| format!("open archive {}", archive.display()))?;
            let mut zip = zip::ZipArchive::new(file)
                .with_context(|| format!("zip parse {}", archive.display()))?;
            let needle = ort_lib_name();
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i).context("zip entry")?;
                // Zip-Einträge verwenden Forward-Slashes — nach dem letzten
                // Slash splitten, statt Path::file_name (das Backslashes
                // erwartet, wenn der Slash nicht als Separator durchgereicht
                // wird).
                let entry_name = entry.name().to_owned();
                let base = entry_name.rsplit('/').next().unwrap_or(&entry_name);
                if base == needle {
                    let mut out = std::fs::File::create(target).with_context(|| {
                        format!("create target {}", target.display())
                    })?;
                    copy(&mut entry, &mut out).with_context(|| {
                        format!("extract {entry_name} → {}", target.display())
                    })?;
                    log::info!("ner download: extracted {}", target.display());
                    return Ok(());
                }
            }
            anyhow::bail!(
                "ORT-Shared-Library {needle} nicht in Archive {} gefunden",
                archive.display()
            );
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            anyhow::bail!("Plattform nicht unterstützt für ORT-Auto-Extract")
        }
    }

    /// Matcht die ORT-Hauptbibliothek — unversioniert („libonnxruntime.dylib")
    /// wie versioniert („libonnxruntime.1.22.0.dylib", „libonnxruntime.so.1.22.0").
    /// Der Punkt nach dem Stamm grenzt gegen Nachbar-Libs wie
    /// „libonnxruntime_providers_shared.so" ab.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn is_ort_lib_file(name: &str) -> bool {
        if name == ort_lib_name() {
            return true;
        }
        name.strip_prefix("libonnxruntime")
            .map(|rest| rest.starts_with('.') && (rest.ends_with(".dylib") || rest.contains(".so")))
            .unwrap_or(false)
    }

    fn write_manifest(dir: &std::path::Path) -> Result<()> {
        use std::io::Write;
        let mut content = String::new();
        content.push_str("# Auto-generated by Streichzeug ner::downloader.\n");
        content.push_str("# Format: <sha256-hex>  <relative-path>\n");
        // Neben Modell + Tokenizer auch die native ORT-Library hashen: sie
        // wird per ORT_DYLIB_PATH geladen und ist damit Teil der Supply-Chain.
        // `verify_manifest` verlangt diesen Eintrag vor dem Laden.
        for name in ["model.onnx", "tokenizer.json", ort_lib_name()] {
            let path = dir.join(name);
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read {} for manifest", path.display()))?;
            let hash = hex::encode(Sha256::digest(&bytes));
            content.push_str(&format!("{hash}  {name}\n"));
        }
        let mut file = std::fs::File::create(dir.join("MANIFEST.sha256"))?;
        file.write_all(content.as_bytes())?;
        Ok(())
    }
}

#[cfg(all(test, feature = "ner"))]
mod diagnose_tests {
    /// Diagnose gegen die ECHTE Engine (braucht heruntergeladene Modell-
    /// Dateien im User-Datenverzeichnis) — bewusst `#[ignore]`:
    /// `cargo test --features ner -- --ignored diagnose --nocapture`
    #[test]
    #[ignore]
    fn diagnose_salutation_names() {
        super::set_enabled(true);
        let ready = super::ensure_loaded();
        println!("engine ready: {ready}");
        for text in [
            "Lieber Herr Dr. Demary,",
            "lieber Herr Dr. Obst,",
            "Volker Demary und Andreas Obst treffen sich in Dortmund.",
            // Länge/Struktur wie eine echte Mail — reproduziert den
            // Beta-Befund, dass NER im langen Text nichts liefert.
            "Betreff: NPL-Grundlagenstudie: Literatur und BKS-Plattform\n\nLieber Herr Dr. Demary,\nlieber Herr Dr. Obst,\n\nvielen Dank!",
            // Gleicher Text OHNE Umlaute/Sonderzeichen zur Abgrenzung.
            "Betreff: Studie\n\nLieber Herr Dr. Demary,\nlieber Herr Dr. Obst,\n\nvielen Dank!",
        ] {
            let findings = super::classify(text);
            println!("{text:?} -> {findings:?}");
        }
    }

    /// Lange Mail (>512 Tokens) — vorher scheiterte die Inferenz komplett
    /// (ORT-Broadcast „512 by 725") und NER lieferte still nichts.
    #[test]
    #[ignore]
    fn diagnose_long_text_chunking() {
        super::set_enabled(true);
        assert!(super::ensure_loaded());
        // Füller erzeugt >512 Tokens; die Namen stehen am Anfang UND am
        // Ende — beide müssen durchs Fenster-Chunking gefunden werden.
        let filler = "Die Plattform aggregiert Literatur zu notleidenden Krediten und stellt Volltexte bereit. ".repeat(40);
        let text = format!(
            "Lieber Herr Dr. Demary,\n\n{filler}\nMit freundlichen Grüßen\nVolker Demary und Andreas Obst"
        );
        let findings = super::classify(&text);
        println!("findings: {findings:?}");
        assert!(
            findings.iter().any(|f| f.text.contains("Demary") && f.start < 30),
            "Name am ANFANG des langen Texts fehlt"
        );
        assert!(
            findings.iter().any(|f| f.text.contains("Obst") && f.start > 1000),
            "Name am ENDE des langen Texts fehlt"
        );
    }

    /// Voller detect()-Durchlauf über einen realistischen Mail-Anfang mit
    /// Umlauten VOR den Namen — prüft, ob NER-Findings die Pipeline
    /// (Offset-Mapping, Dedupe) überleben.
    #[test]
    #[ignore]
    fn diagnose_full_detect_email() {
        super::set_enabled(true);
        assert!(super::ensure_loaded());
        let text = "Betreff: NPL-Grundlagenstudie: Literatur und BKS-Plattform\n\nLieber Herr Dr. Demary,\nlieber Herr Dr. Obst,\n\nvielen Dank für die Rückmeldung — schöne Grüße aus München.";
        let findings = crate::detection::detect(text);
        for f in &findings {
            println!("{}..{} {} {:?} conf={}", f.start, f.end, f.entity_type, f.original, f.confidence);
        }
        assert!(
            findings.iter().any(|f| f.original == "Demary"),
            "Demary fehlt im Gesamtergebnis"
        );
        assert!(
            findings.iter().any(|f| f.original.contains("Obst")),
            "Obst fehlt im Gesamtergebnis"
        );
    }
}
