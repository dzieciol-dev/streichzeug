//! Logging-Infrastruktur für die App.
//!
//! Schreibt **gleichzeitig** auf stdout (für `cargo run`-Entwicklung) und
//! in eine rotierende Log-Datei unter `$DATA_DIR/<APP_DIR>/logs/app.log`.
//! Die Datei ist der Pfad, den ein Beta-Tester bei einem Bug-Report
//! mitschicken kann.
//!
//! # Design-Entscheidungen
//!
//! - **Kein `flexi_logger` oder `tracing-appender`**: für ein einzelnes
//!   Log-File mit Größen-basierter Rotation reicht ein 100-Zeilen-
//!   `log::Log`-Sink. Vermeidet eine neue Crate-Dep und ist auditierbar.
//! - **Größen-basierte Rotation** statt täglich: ein Beta-Tester drückt
//!   evtl. nur 10 Mal Strg+B pro Tag, ein Daily-Rotate-Logfile wäre fast
//!   leer. 1 MB Roll-Over hält ~10.000 Zeilen pro Datei und rotiert auf
//!   `app.log.1`, `app.log.2`. Maximal 3 Generationen behalten.
//! - **Synchrones Schreiben**: kein async, kein Buffer-Channel — wir
//!   haben höchstens ~50 Log-Calls pro Strg+B-Druck, das verträgt die
//!   Disk auch synchron problemlos.
//! - **Best-Effort**: schlägt das Schreiben fehl (Datei gelockt, kein
//!   Platz), loggen wir trotzdem auf stdout und schlucken den Fehler.
//!   Ein Logging-Modul darf die App nicht zum Crash bringen.
//!
//! # Log-Level-Default
//!
//! INFO in Release, DEBUG in Debug-Builds. Override via `RUST_LOG`-
//! Env-Variable (Standard-`env_logger`-Konvention).

use log::{LevelFilter, Log, Metadata, Record};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

const APP_DIR: &str = "de.streichzeug.app";
const LOG_FILENAME: &str = "app.log";
/// Roll-Over bei 1 MB. Bei DEBUG-Logs ein paar tausend Zeilen — genug für
/// einen Bug-Report-Snapshot, klein genug zum Mail-Anhang.
const MAX_LOG_BYTES: u64 = 1_000_000;
/// Anzahl rotierter Generationen (app.log.1, app.log.2, …).
const MAX_GENERATIONS: usize = 3;

struct AppLogger {
    /// Die offene Log-Datei. `None` falls Initialisierung scheiterte —
    /// dann logging-fallback auf stdout.
    file: Mutex<Option<std::fs::File>>,
    path: Option<PathBuf>,
    level: LevelFilter,
}

impl Log for AppLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} [{:5}] {}: {}\n",
            chrono_like_timestamp(),
            record.level(),
            record.target(),
            record.args(),
        );
        // Stdout (immer, damit `cargo run`-Entwicklung sichtbar bleibt)
        let _ = std::io::stdout().write_all(line.as_bytes());
        // Datei (best-effort)
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.write_all(line.as_bytes());
                // Größen-Check und ggf. Rotation
                if let Ok(pos) = file.seek(SeekFrom::Current(0)) {
                    if pos > MAX_LOG_BYTES {
                        if let Some(path) = &self.path {
                            rotate(path);
                            // Datei neu öffnen
                            if let Ok(new_file) = open_log_file(path) {
                                *guard = Some(new_file);
                            }
                        }
                    }
                }
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock() {
            if let Some(file) = guard.as_mut() {
                let _ = file.flush();
            }
        }
        let _ = std::io::stdout().flush();
    }
}

/// Registriert einen Panic-Hook, der den Panic in unser Log-File schreibt.
/// Sonst geht ein Release-Panic bei `panic = "unwind"` nur in stderr, und
/// das ist im MSI-Build typischerweise nirgendwo sichtbar.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("<non-string panic>");
        log::error!("PANIC at {location}: {payload}");
    }));
}

/// Initialisiert den Logger. Sollte einmal beim App-Start gerufen werden.
/// Idempotent dank `set_logger`-Idiom — ein zweiter Aufruf wird ignoriert.
pub fn init() {
    let level = parse_log_level();
    let path = log_path();
    let file = path.as_ref().and_then(|p| open_log_file(p).ok());

    let logger = Box::new(AppLogger {
        file: Mutex::new(file),
        path,
        level,
    });

    // `set_boxed_logger` schlägt fehl, wenn schon ein Logger registriert
    // ist. Das ist OK — wir wollen idempotent sein.
    let _ = log::set_boxed_logger(logger);
    log::set_max_level(level);
    install_panic_hook();

    log::info!(
        "logger initialized (level={}, file={:?})",
        level,
        log_path()
    );
}

fn parse_log_level() -> LevelFilter {
    if let Ok(raw) = std::env::var("RUST_LOG") {
        match raw.to_lowercase().as_str() {
            "trace" => return LevelFilter::Trace,
            "debug" => return LevelFilter::Debug,
            "info" => return LevelFilter::Info,
            "warn" => return LevelFilter::Warn,
            "error" => return LevelFilter::Error,
            "off" => return LevelFilter::Off,
            _ => {}
        }
    }
    if cfg!(debug_assertions) {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    }
}

fn log_path() -> Option<PathBuf> {
    log_dir().map(|d| d.join(LOG_FILENAME))
}

/// Pfad zum Logs-Verzeichnis (ohne Dateinamen). Wird von der UI/Tray
/// genutzt, um den Ordner im Explorer/Finder zu öffnen.
pub fn log_dir_path() -> Option<PathBuf> {
    log_dir()
}

fn log_dir() -> Option<PathBuf> {
    let base = dirs::data_local_dir().or_else(dirs::data_dir)?;
    Some(base.join(APP_DIR).join("logs"))
}

/// Liest die letzten `n` Zeilen aus dem aktiven Log-File. Für den
/// „Log in Zwischenablage kopieren"-Workflow im UI: reduziert den
/// Bug-Report auf 2 Klicks (Button → in Mail einfügen).
pub fn read_tail(n_lines: usize) -> Option<String> {
    let path = log_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let tail_start = lines.len().saturating_sub(n_lines);
    Some(lines[tail_start..].join("\n"))
}

fn open_log_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

/// Rotiert `app.log` → `app.log.1`, `app.log.1` → `app.log.2`, …. Die
/// älteste Generation (`app.log.MAX_GENERATIONS`) wird gelöscht.
fn rotate(path: &std::path::Path) {
    // Älteste löschen.
    let oldest = numbered_path(path, MAX_GENERATIONS);
    let _ = std::fs::remove_file(&oldest);
    // Restliche durchschieben — von hinten nach vorne, sonst überschreibt
    // man sich selbst.
    for gen in (1..MAX_GENERATIONS).rev() {
        let src = numbered_path(path, gen);
        let dst = numbered_path(path, gen + 1);
        if src.exists() {
            let _ = std::fs::rename(&src, &dst);
        }
    }
    // app.log → app.log.1
    let _ = std::fs::rename(path, numbered_path(path, 1));
}

fn numbered_path(base: &std::path::Path, n: usize) -> PathBuf {
    let mut p = base.as_os_str().to_owned();
    p.push(format!(".{n}"));
    PathBuf::from(p)
}

/// Schlanker ISO-8601-Timestamp ohne `chrono`-Dependency. Format:
/// `2026-05-17 12:34:56` (Sekunden-Auflösung reicht für Log-Reports).
fn chrono_like_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Konvertieren ohne chrono — einfache Berechnung.
    let secs = now % 60;
    let mins = (now / 60) % 60;
    let hours = (now / 3600) % 24;
    let days_since_epoch = now / 86400;
    let (year, month, day) = days_to_ymd(days_since_epoch as i64);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, day, hours, mins, secs
    )
}

/// Tage-seit-1970-Epoch zu (Jahr, Monat, Tag). Greg-Kalender, Schaltjahre
/// regulär behandelt. Reicht für Log-Timestamps; keine Sub-Sekunden, keine
/// Zeitzone — Tester-Logs werden eh nur grob nach Tag betrachtet.
fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    // Vom 1970-01-01 weg rechnen. Algorithmus aus „Astronomical Algorithms"
    // von Jean Meeus, vereinfacht für die Gregorianische Spanne.
    let mut year = 1970;
    let mut remaining = days;
    loop {
        let leap = is_leap(year);
        let year_days: i64 = if leap { 366 } else { 365 };
        if remaining < year_days {
            break;
        }
        remaining -= year_days;
        year += 1;
    }
    let leap = is_leap(year);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u32;
    let mut day_of_year = remaining as u32;
    for &md in &month_days {
        if day_of_year < md {
            break;
        }
        day_of_year -= md;
        month += 1;
    }
    let day = day_of_year + 1;
    (year, month, day)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}
