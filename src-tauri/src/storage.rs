//! Persistenter Mapping-Store für Reverse-Lookup.
//!
//! # Architektur
//!
//! - **SQLite-Datei** unter `$DATA_DIR/de.streichzeug.app/storage.db`.
//!   `dirs::data_dir()` löst die Plattform-spezifischen Pfade auf — Windows
//!   nach `%APPDATA%\Roaming\de.streichzeug.app\`, macOS nach
//!   `~/Library/Application Support/de.streichzeug.app/`. Sowohl
//!   Tauri-UI als auch NMH-Child kommen auf denselben Pfad → ein gemeinsamer
//!   Store, keine doppelte In-Memory-Kopie.
//!
//! - **WAL-Mode** für Multi-Prozess-Sicherheit. Mehrere parallele Reader,
//!   ein Writer at a time, kein File-Locking-Stall.
//!
//! - **Connection per Prozess**, in `Lazy<Mutex<Connection>>`. Die Mutex
//!   serialisiert Schreiber innerhalb eines Prozesses; WAL erledigt den
//!   Inter-Prozess-Part.
//!
//! # Schema
//!
//! ```sql
//! CREATE TABLE mappings (
//!     case_id    TEXT NOT NULL,
//!     token      TEXT NOT NULL,
//!     original   TEXT NOT NULL,
//!     first_seen DATETIME DEFAULT CURRENT_TIMESTAMP,
//!     PRIMARY KEY (case_id, token)
//! );
//! ```
//!
//! # Phase 2 — Verschlüsselung
//!
//! Aktuell **plain SQLite**. Mapping-Daten enthalten Klartext-PII, die der
//! User ohnehin freiwillig kopiert hat (kein Credential-Material) — Risiko
//! niedriger als bei Secrets. Für Defense-in-Depth wäre der nächste Schritt
//! SQLCipher mit Master-Key aus dem OS-Keychain. Aktivierbar durch
//! Feature-Flag in `Cargo.toml`:
//!
//! ```toml
//! rusqlite = { features = ["bundled-sqlcipher-vendored-openssl"] }
//! ```
//!
//! Plus `connection.pragma_update(None, "key", &master_key)?;` vor dem
//! ersten Query.

use once_cell::sync::Lazy;
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::Mutex;

/// Anwendungs-spezifischer Unterordner unter `$DATA_DIR`. Muss zur
/// `identifier`-Property in `tauri.conf.json` passen.
const APP_DIR: &str = "de.streichzeug.app";
const DB_FILENAME: &str = "storage.db";

/// Globaler Connection-Handle. Wird beim ersten Zugriff geöffnet und
/// initialisiert. Bei Test-Builds zeigt der Pfad auf eine `:memory:`-DB,
/// damit Tests sich nicht gegenseitig die Mappings stören.
static CONN: Lazy<Mutex<Connection>> = Lazy::new(|| {
    let conn = if cfg!(test) {
        Connection::open_in_memory().expect("open in-memory DB")
    } else {
        let path = db_path().expect("resolve data dir");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create app data dir");
        }
        Connection::open(&path).expect("open storage.db")
    };
    init_schema(&conn).expect("init schema");
    Mutex::new(conn)
});

/// Auflösung des DB-Pfads: `$DATA_DIR/de.streichzeug.app/storage.db`.
fn db_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(APP_DIR).join(DB_FILENAME))
}

/// WAL-Mode + Schema-Erzeugung. Idempotent — kann beliebig oft laufen.
fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    // WAL gibt uns Multi-Reader/Single-Writer ohne File-Locking-Stall.
    // Pragmas mit `pragma_update(None, ...)` — None heißt „kein
    // bestimmtes Schema" (anders als ATTACHed DBs).
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS mappings (
            case_id    TEXT NOT NULL,
            token      TEXT NOT NULL,
            original   TEXT NOT NULL,
            first_seen DATETIME DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (case_id, token)
        );
        CREATE INDEX IF NOT EXISTS idx_mappings_token ON mappings(token);
        "#,
    )?;
    Ok(())
}

// =================================================================== Public API

/// Speichert eine Token→Original-Zuordnung für einen Case.
///
/// Idempotent: existiert das Paar bereits, wird der Eintrag gelassen (kein
/// Update des `first_seen`-Timestamps). Wir nutzen `INSERT OR IGNORE` —
/// schneller als ein vorheriges SELECT, und für unsere Determinismus-Garantie
/// (gleicher Klartext → gleiches Token) liefert der spätere Lookup ohnehin
/// denselben Wert.
pub fn record(case_id: &str, token: &str, original: &str) {
    let conn = CONN.lock().expect("CONN mutex poisoned");
    let _ = conn.execute(
        "INSERT OR IGNORE INTO mappings (case_id, token, original) VALUES (?1, ?2, ?3)",
        params![case_id, token, original],
    );
}

/// Ersetzt im Text alle bekannten Tokens des Cases durch ihre Originale.
/// Unbekannte Tokens bleiben unverändert.
///
/// Implementierung: ein einziger SELECT holt alle Mappings des Cases,
/// dann String-Replace im Speicher. Für typische Case-Größen (10–100
/// Mappings) schneller als pro-Token-Lookups.
pub fn restore(case_id: &str, text: &str) -> String {
    let conn = CONN.lock().expect("CONN mutex poisoned");
    let mut stmt = match conn.prepare("SELECT token, original FROM mappings WHERE case_id = ?1") {
        Ok(s) => s,
        Err(_) => return text.to_string(),
    };
    let rows = stmt.query_map(params![case_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    });
    let pairs: Vec<(String, String)> = match rows {
        Ok(iter) => iter.flatten().collect(),
        Err(_) => return text.to_string(),
    };

    let mut result = text.to_string();
    for (token, original) in pairs {
        if result.contains(&token) {
            result = result.replace(&token, &original);
        }
    }
    result
}

/// Wie [`restore`], aber über **alle** Cases hinweg. Wird vom
/// Reverse-Pfad genutzt, weil Forward-Operationen jetzt eigene
/// frische case_ids erzeugen — würde Reverse mit einem festen
/// case_id filtern, fänden wir die Mappings des Forward-Cases nicht.
///
/// Performance: ein einziger Full-Table-Scan. Bei typischen Beta-
/// Volumina (paar tausend Mappings) im Millisekundenbereich. Phase-2:
/// falls die DB groß wird, könnten wir die Restore-Suche auf Cases der
/// letzten N Tage einschränken.
pub fn restore_all_cases(text: &str) -> String {
    let conn = CONN.lock().expect("CONN mutex poisoned");
    let mut stmt = match conn.prepare("SELECT token, original FROM mappings") {
        Ok(s) => s,
        Err(_) => return text.to_string(),
    };
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    });
    let pairs: Vec<(String, String)> = match rows {
        Ok(iter) => iter.flatten().collect(),
        Err(_) => return text.to_string(),
    };

    let mut result = text.to_string();
    for (token, original) in pairs {
        if result.contains(&token) {
            result = result.replace(&token, &original);
        }
    }
    result
}

/// Löscht alle Mappings älter als `minutes` Minuten.
///
/// **DSGVO-Hintergrund:** solange die Mapping-Tabelle existiert, sind
/// die zugehörigen Tokens beim LLM-Anbieter noch personenbezogene Daten
/// (reversibel). Mit einer endlichen Retention werden die Tokens nach
/// Ablauf zu anonymen Daten (Art. 4(5) DSGVO). Default-Retention liegt
/// bei 60 Minuten, konfigurierbar in den Settings.
///
/// SQL: `first_seen < datetime('now', '-N minutes')`. Nutzt den
/// SQLite-eigenen Zeitvergleich, damit wir keinen Rust-seitigen
/// Now-Offset durchreichen müssen.
pub fn purge_older_than(minutes: u32) -> usize {
    let conn = CONN.lock().expect("CONN mutex poisoned");
    let modifier = format!("-{minutes} minutes");
    match conn.execute(
        "DELETE FROM mappings WHERE first_seen < datetime('now', ?1)",
        params![modifier],
    ) {
        Ok(n) => {
            if n > 0 {
                log::info!("storage: purged {n} mappings older than {minutes} min");
            }
            n
        }
        Err(e) => {
            log::warn!("storage: purge_older_than failed: {e}");
            0
        }
    }
}

/// Löscht **alle** Mappings sofort. Wird vom UI-Button „Jetzt alle
/// Mappings löschen" und beim App-Start im Session-only-Modus
/// (`retention_minutes = 0`) aufgerufen.
pub fn purge_all() -> usize {
    let conn = CONN.lock().expect("CONN mutex poisoned");
    match conn.execute("DELETE FROM mappings", []) {
        Ok(n) => {
            log::info!("storage: purged ALL mappings ({n})");
            n
        }
        Err(e) => {
            log::warn!("storage: purge_all failed: {e}");
            0
        }
    }
}

/// Anzahl der aktuell gespeicherten Mappings — für die UI-Anzeige
/// im Datenspeicherungs-Bereich.
pub fn mapping_count() -> usize {
    let conn = CONN.lock().expect("CONN mutex poisoned");
    conn.query_row("SELECT COUNT(*) FROM mappings", [], |row| row.get::<_, i64>(0))
        .map(|n| n as usize)
        .unwrap_or(0)
}

/// Löscht alle Mappings eines Case — z. B. wenn der User „Case schließen"
/// klickt. Aktuell nur über Tests aufgerufen; UI-Anbindung folgt.
#[allow(dead_code)]
pub fn clear(case_id: &str) {
    let conn = CONN.lock().expect("CONN mutex poisoned");
    let _ = conn.execute("DELETE FROM mappings WHERE case_id = ?1", params![case_id]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_restore_roundtrip() {
        let case = "test-roundtrip";
        clear(case);
        record(case, "«P_a4b»", "Max Mustermann");
        record(case, "«E_zk9»", "jan@example.de");

        let pseud = "Hallo «P_a4b», erreichbar unter «E_zk9».";
        let restored = restore(case, pseud);
        assert_eq!(restored, "Hallo Max Mustermann, erreichbar unter jan@example.de.");
    }

    #[test]
    fn restore_leaves_unknown_tokens() {
        let case = "test-unknown";
        clear(case);
        record(case, "«P_aaa»", "Jan");
        let restored = restore(case, "Hallo «P_aaa» und «P_bbb»!");
        assert_eq!(restored, "Hallo Jan und «P_bbb»!");
    }

    #[test]
    fn cases_are_isolated() {
        clear("case-a");
        clear("case-b");
        record("case-a", "«P_aaa»", "Alice");
        record("case-b", "«P_aaa»", "Bob");
        assert_eq!(restore("case-a", "«P_aaa»"), "Alice");
        assert_eq!(restore("case-b", "«P_aaa»"), "Bob");
    }

    #[test]
    fn record_is_idempotent() {
        let case = "test-idempotent";
        clear(case);
        record(case, "«P_x»", "Alice");
        record(case, "«P_x»", "Alice"); // selber Eintrag
        // Sollte nicht crashen, und der Restore funktioniert weiterhin.
        assert_eq!(restore(case, "«P_x»"), "Alice");
    }
}
