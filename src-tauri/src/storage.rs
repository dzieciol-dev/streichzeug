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
//! # Verschlüsselung (SQLCipher)
//!
//! Die DB ist **transparent AES-256-verschlüsselt** (SQLCipher, via
//! `rusqlite`-Feature `bundled-sqlcipher-vendored-openssl`). Der 256-Bit-Key
//! wird aus dem Master-Secret abgeleitet ([`crate::secrets::db_key_hex`]) und
//! direkt nach dem `Connection::open` als `PRAGMA key = "x'<hex>'"` gesetzt.
//! Weil das Master-Secret bereits kryptografisch zufällig ist, nutzen wir den
//! Roh-Key ohne KDF-Runden.
//!
//! **Migration:** Beim ersten Start mit dieser Version wird eine bestehende
//! *unverschlüsselte* `storage.db` erkannt und per `sqlcipher_export()`
//! transparent in eine verschlüsselte Kopie überführt; die Klartext-Datei
//! (inkl. `-wal`/`-shm`) wird anschließend sicher gelöscht. Siehe
//! [`migrate_plaintext_if_needed`].

use once_cell::sync::Lazy;
use rusqlite::{params, params_from_iter, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
        // In-Memory-DB pro Test-Prozess. Unverschlüsselt — die
        // SQLCipher-Migration wird separat über die reinen Helfer-Funktionen
        // mit echten Temp-Dateien getestet (siehe Tests unten).
        Connection::open_in_memory().expect("open in-memory DB")
    } else {
        let path = db_path().expect("resolve data dir");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create app data dir");
        }
        let key_hex = crate::secrets::db_key_hex();

        // Bestehende Klartext-DB transparent nach SQLCipher migrieren.
        if let Err(e) = migrate_plaintext_if_needed(&path, &key_hex) {
            log::error!("storage: plaintext migration failed: {e}");
        }

        let conn = Connection::open(&path).expect("open storage.db");
        set_key(&conn, &key_hex).expect("set SQLCipher key");
        conn
    };
    init_schema(&conn).expect("init schema");
    Mutex::new(conn)
});

/// Setzt den SQLCipher-Schlüssel als Roh-Key (`x'<hex>'`, kein KDF). Muss
/// direkt nach `Connection::open` und **vor** jeder anderen Anweisung laufen.
fn set_key(conn: &Connection, key_hex: &str) -> rusqlite::Result<()> {
    conn.execute_batch(&format!("PRAGMA key = \"x'{key_hex}'\";"))
}

/// Öffnet eine SQLCipher-Verbindung und setzt den Schlüssel. Prüft **nicht**,
/// ob der Key passt — dafür [`is_readable`].
fn open_encrypted(path: &Path, key_hex: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    set_key(&conn, key_hex)?;
    Ok(conn)
}

/// Prüft, ob die Verbindung tatsächlich lesbar ist (korrekter Key bzw.
/// Klartext-DB). Bei falschem Key liefert SQLCipher „file is not a database".
fn is_readable(conn: &Connection) -> bool {
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))
        .is_ok()
}

/// Migriert eine bestehende **unverschlüsselte** `storage.db` transparent nach
/// SQLCipher. Idempotent und sicher:
///
/// - Datei fehlt → nichts zu tun (frische DB wird verschlüsselt angelegt).
/// - Datei bereits mit unserem Key lesbar → nichts zu tun.
/// - Datei als Klartext lesbar → Export in verschlüsselte Kopie, altes File
///   (inkl. `-wal`/`-shm`) sicher löschen, Kopie an Zielstelle rücken.
/// - Weder Klartext noch mit Key lesbar → unangetastet lassen (fremd
///   verschlüsselt oder korrupt), Fehler wird oben geloggt.
fn migrate_plaintext_if_needed(path: &Path, key_hex: &str) -> rusqlite::Result<()> {
    if !path.exists() {
        return Ok(());
    }

    // Bereits mit unserem Key lesbar? → fertig.
    if let Ok(conn) = open_encrypted(path, key_hex) {
        if is_readable(&conn) {
            return Ok(());
        }
    }

    // Als Klartext öffnen und prüfen.
    let plain = Connection::open(path)?;
    if !is_readable(&plain) {
        log::error!(
            "storage.db weder als Klartext noch mit aktuellem Key lesbar — Migration übersprungen"
        );
        return Ok(());
    }

    log::warn!("unverschlüsselte storage.db erkannt — migriere nach SQLCipher");

    // WAL einchecken, damit sqlcipher_export den vollständigen Stand sieht.
    let _ = plain.pragma_update(None, "wal_checkpoint", "TRUNCATE");

    let tmp = sidecar(path, ".enc-tmp");
    let _ = std::fs::remove_file(&tmp);

    // Verschlüsselte Ziel-DB anhängen, exportieren, wieder lösen.
    let tmp_sql = tmp.to_string_lossy().replace('\'', "''");
    plain.execute_batch(&format!(
        "ATTACH DATABASE '{tmp_sql}' AS encrypted KEY \"x'{key_hex}'\";\
         SELECT sqlcipher_export('encrypted');\
         DETACH DATABASE encrypted;"
    ))?;
    drop(plain);

    // Klartext-Artefakte sicher entfernen.
    secure_delete(path);
    let _ = std::fs::remove_file(sidecar(path, "-wal"));
    let _ = std::fs::remove_file(sidecar(path, "-shm"));

    // Verschlüsselte Datei an die Zielstelle rücken.
    std::fs::rename(&tmp, path)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

    log::info!("storage.db erfolgreich nach SQLCipher migriert");
    Ok(())
}

/// Baut einen Geschwister-Dateinamen (`storage.db` + Suffix). Für Temp- und
/// WAL/SHM-Sidecar-Dateien.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Überschreibt eine Datei mit Nullen und löscht sie (Best-Effort). Für die
/// alte Klartext-DB nach der Migration — kein Klartext-PII soll zurückbleiben.
fn secure_delete(path: &Path) {
    use std::io::Write;
    if let Ok(meta) = std::fs::metadata(path) {
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(path) {
            let zeros = vec![0u8; 8192];
            let mut remaining = meta.len();
            while remaining > 0 {
                let n = remaining.min(zeros.len() as u64) as usize;
                if f.write_all(&zeros[..n]).is_err() {
                    break;
                }
                remaining -= n as u64;
            }
            let _ = f.flush();
            let _ = f.sync_all();
        }
    }
    let _ = std::fs::remove_file(path);
}

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
/// # Performance
///
/// Statt eines Full-Table-Scans mit `.contains`/`.replace` pro Mapping (linear
/// in der Tabellengröße — bei 24h-Retention leicht mehrere tausend Zeilen)
/// werden zuerst die **tatsächlich im Text vorkommenden Tokens** per Regex
/// extrahiert und nur diese über den `idx_mappings_token`-Index nachgeschlagen
/// (`WHERE token IN (...)`). Die Arbeit skaliert damit mit der Token-Anzahl im
/// Text, nicht mit der DB-Größe. Enthält der Text keine Tokens, entfällt der
/// DB-Zugriff komplett.
///
/// Semantik unverändert: bekannte Tokens werden durch ihr Original ersetzt,
/// unbekannte bleiben stehen. Bei case-übergreifend kollidierendem Token
/// (gleiches Token, verschiedene Originale) gewinnt — wie zuvor — genau eine
/// Zuordnung.
pub fn restore_all_cases(text: &str) -> String {
    // 1. Tokens aus dem Text extrahieren (dedupliziert, in Auftrittsreihenfolge).
    let tokens = extract_tokens(text);
    if tokens.is_empty() {
        return text.to_string();
    }

    // 2. Gezielter Lookup nur der vorhandenen Tokens.
    let conn = CONN.lock().expect("CONN mutex poisoned");
    let placeholders = vec!["?"; tokens.len()].join(",");
    let sql = format!("SELECT token, original FROM mappings WHERE token IN ({placeholders})");
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return text.to_string(),
    };
    let rows = stmt.query_map(params_from_iter(tokens.iter().copied()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    });
    let mut map: HashMap<String, String> = HashMap::new();
    match rows {
        Ok(iter) => {
            for (token, original) in iter.flatten() {
                // Erste Zuordnung gewinnt bei case-übergreifender Kollision.
                map.entry(token).or_insert(original);
            }
        }
        Err(_) => return text.to_string(),
    }

    // 3. Ersetzen — in Auftrittsreihenfolge, jedes Token genau einmal
    //    (replace ersetzt alle Vorkommen).
    let mut result = text.to_string();
    for token in tokens {
        if let Some(original) = map.get(token) {
            result = result.replace(token, original);
        }
    }
    result
}

/// Extrahiert alle Tokens im Format `«T_xxxxxx»` aus dem Text, dedupliziert und
/// in Reihenfolge des ersten Auftretens. Nutzt das zentrale Token-Regex aus
/// [`crate::tokens`], damit Format-Änderungen an einer Stelle bleiben.
fn extract_tokens(text: &str) -> Vec<&str> {
    static RE: Lazy<regex::Regex> =
        Lazy::new(|| regex::Regex::new(crate::tokens::TOKEN_REGEX_PATTERN).expect("valid token regex"));
    let mut seen = std::collections::HashSet::new();
    RE.find_iter(text)
        .map(|m| m.as_str())
        .filter(|t| seen.insert(*t))
        .collect()
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

    // -------------------------------------------------- Reverse-Lookup (WP3-2)

    #[test]
    fn extract_tokens_dedupes_in_order() {
        let text = "«P_aaa111» x «E_bbb222» y «P_aaa111» z «X_bad» «P_ccc333»";
        // «X_bad» matcht das Regex nicht (Typ X + nur 3 Zeichen).
        let tokens = extract_tokens(text);
        assert_eq!(tokens, vec!["«P_aaa111»", "«E_bbb222»", "«P_ccc333»"]);
    }

    #[test]
    fn restore_all_cases_targeted_lookup() {
        clear("wp3-ca");
        clear("wp3-cb");
        record("wp3-ca", "«P_aaa111»", "Alice");
        record("wp3-cb", "«E_bbb222»", "bob@example.de");

        // Bekannte Tokens aus verschiedenen Cases + ein unbekanntes (korrektes
        // Format, aber nicht in der DB) bleiben unverändert.
        let text = "Hi «P_aaa111», mail «E_bbb222», unbekannt «P_unkn00».";
        let out = restore_all_cases(text);
        assert_eq!(out, "Hi Alice, mail bob@example.de, unbekannt «P_unkn00».");
    }

    #[test]
    fn restore_all_cases_no_tokens_is_noop() {
        // Kein Token im Text → keine Änderung (und kein DB-Zugriff nötig).
        let text = "Reiner Text ohne Pseudonyme.";
        assert_eq!(restore_all_cases(text), text);
    }

    // ---------------------------------------------- SQLCipher-Migration (WP3-1)

    #[test]
    fn migration_plaintext_to_encrypted() {
        // Eindeutiges Temp-Verzeichnis pro Testlauf.
        let dir = std::env::temp_dir().join(format!(
            "sz-wp3-mig-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("storage.db");
        let _ = std::fs::remove_file(&path);

        let key = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

        // 1. Klartext-DB (ohne Key) mit Daten anlegen.
        {
            let conn = Connection::open(&path).unwrap();
            init_schema(&conn).unwrap();
            conn.execute(
                "INSERT INTO mappings (case_id, token, original) VALUES (?1,?2,?3)",
                params!["c1", "«P_abc123»", "Max Mustermann"],
            )
            .unwrap();
        }
        // Sanity: als Klartext lesbar.
        {
            let conn = Connection::open(&path).unwrap();
            assert!(is_readable(&conn), "Klartext-DB muss lesbar sein");
        }

        // 2. Migrieren.
        migrate_plaintext_if_needed(&path, key).unwrap();

        // 3a. Ohne Key nicht mehr lesbar (jetzt verschlüsselt).
        {
            let conn = Connection::open(&path).unwrap();
            assert!(
                !is_readable(&conn),
                "verschlüsselte DB darf ohne Key nicht lesbar sein"
            );
        }
        // 3b. Mit Key lesbar und Daten intakt.
        {
            let conn = open_encrypted(&path, key).unwrap();
            assert!(is_readable(&conn));
            let orig: String = conn
                .query_row(
                    "SELECT original FROM mappings WHERE token = ?1",
                    params!["«P_abc123»"],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(orig, "Max Mustermann");
        }
        // 3c. Sidecar-Dateien der Klartext-DB sind weg.
        assert!(!sidecar(&path, "-wal").exists());
        assert!(!sidecar(&path, "-shm").exists());
        assert!(!sidecar(&path, ".enc-tmp").exists());

        // 4. Idempotenz: erneuter Aufruf lässt die verschlüsselte DB unangetastet.
        migrate_plaintext_if_needed(&path, key).unwrap();
        {
            let conn = open_encrypted(&path, key).unwrap();
            let n: i64 = conn
                .query_row("SELECT count(*) FROM mappings", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1, "Migration darf Daten nicht duplizieren/verlieren");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migration_missing_file_is_noop() {
        let path = std::env::temp_dir().join(format!("sz-wp3-none-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // Darf nicht crashen und legt nichts an.
        migrate_plaintext_if_needed(&path, "aa").unwrap();
        assert!(!path.exists());
    }
}
