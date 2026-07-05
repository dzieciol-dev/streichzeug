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
use serde::Serialize;
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
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
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

        -- Ablage der Schwärz-Bühne. Bewusst **nur geschwärzter Text** — kein
        -- Original, damit die Ablage strict-mode-kompatibel ist und keinem
        -- eigenen Retention-Zwang unterliegt (die Rück-Übersetzbarkeit hängt
        -- allein an `mappings`). Löschung: manuell, `stash_clear`, optional
        -- bei Quit (`stash_clear_on_quit`).
        CREATE TABLE IF NOT EXISTS stash (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            created_at    DATETIME DEFAULT CURRENT_TIMESTAMP,
            mode          TEXT NOT NULL,
            title         TEXT NOT NULL,
            redacted_text TEXT NOT NULL,
            entity_counts TEXT NOT NULL
        );
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
    static RE: Lazy<regex::Regex> = Lazy::new(|| {
        regex::Regex::new(crate::tokens::TOKEN_REGEX_PATTERN).expect("valid token regex")
    });
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
    conn.query_row("SELECT COUNT(*) FROM mappings", [], |row| {
        row.get::<_, i64>(0)
    })
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

// ============================================================= Ablage (Stash)
//
// Die Schwärz-Bühne legt jedes geschwärzte Ergebnis als Ablage-Eintrag ab.
// Bewusst getrennt vom Mapping-Store: die Ablage speichert nur die geschwärzte
// Fassung und darf länger leben als die (retention-gebundene) Mapping-Tabelle.

/// Listen-Metadaten eines Ablage-Eintrags. Wird direkt ans Frontend
/// serialisiert (Command `stash_list`) — der geschwärzte Volltext bleibt
/// draußen und wird nur auf Anforderung über `stash_get_text` nachgeladen.
#[derive(Debug, Clone, Serialize)]
pub struct StashMeta {
    pub id: i64,
    /// ISO-8601 in UTC (`2026-07-04T12:34:56Z`). SQLite speichert
    /// `CURRENT_TIMESTAMP` bereits als UTC — wir formatieren nur um.
    pub created_at: String,
    pub mode: String,
    pub title: String,
    pub entity_counts: HashMap<String, u32>,
    /// Zeichen-Länge (nicht Bytes) des geschwärzten Volltexts — für eine
    /// aussagekräftige Größenanzeige unabhängig von Umlauten/Emoji.
    pub char_len: usize,
}

/// Kürzt einen Text auf einen kompakten Listen-Titel: Whitespace-Läufe zu
/// je einem Space kollabiert, dann die ersten 60 **Zeichen**. Der Schnitt
/// läuft über `chars()` statt Byte-Offsets, damit Umlaute/Emoji nie an einer
/// UTF-8-Grenze zerhackt werden. Der Aufrufer reicht den Titel un-vorbereitet
/// (in der Praxis den geschwärzten Text selbst) — die Normalisierung lebt
/// hier an einer Stelle.
fn stash_title(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(60).collect()
}

/// Legt einen Ablage-Eintrag an und liefert dessen `rowid`. `entity_counts`
/// wird als JSON-Objekt (`{"person":2,"iban":1}`) serialisiert, damit die
/// Chip-Anzeige im Frontend die Typen ohne Zusatztabelle rekonstruiert.
///
/// `#[allow(dead_code)]`: der einzige Nicht-Test-Aufrufer ist der Capture-Flow
/// aus WP-A; bis dessen Merge hat die Funktion in diesem Crate keinen Caller.
#[allow(dead_code)]
pub fn stash_insert(
    mode: &str,
    title: &str,
    redacted_text: &str,
    entity_counts: &HashMap<String, u32>,
) -> i64 {
    let title = stash_title(title);
    let counts_json = serde_json::to_string(entity_counts).unwrap_or_else(|_| "{}".to_string());
    let conn = CONN.lock().expect("CONN mutex poisoned");
    match conn.execute(
        "INSERT INTO stash (mode, title, redacted_text, entity_counts) VALUES (?1, ?2, ?3, ?4)",
        params![mode, title, redacted_text, counts_json],
    ) {
        Ok(_) => conn.last_insert_rowid(),
        Err(e) => {
            log::warn!("storage: stash_insert failed: {e}");
            -1
        }
    }
}

/// Alle Ablage-Einträge, neueste zuerst. Der Volltext wird bewusst nicht
/// mitgeladen — nur seine Zeichen-Länge (`length()` zählt bei TEXT Zeichen,
/// nicht Bytes), damit die Liste auch bei großen Einträgen schlank bleibt.
pub fn stash_list() -> Vec<StashMeta> {
    let conn = CONN.lock().expect("CONN mutex poisoned");
    let mut stmt = match conn.prepare(
        "SELECT id, strftime('%Y-%m-%dT%H:%M:%SZ', created_at), mode, title, entity_counts, length(redacted_text) \
         FROM stash ORDER BY id DESC",
    ) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("storage: stash_list prepare failed: {e}");
            return Vec::new();
        }
    };
    let rows = stmt.query_map([], |row| {
        let counts_json: String = row.get(4)?;
        let entity_counts: HashMap<String, u32> =
            serde_json::from_str(&counts_json).unwrap_or_default();
        Ok(StashMeta {
            id: row.get(0)?,
            created_at: row.get(1)?,
            mode: row.get(2)?,
            title: row.get(3)?,
            entity_counts,
            char_len: row.get::<_, i64>(5)? as usize,
        })
    });
    match rows {
        Ok(iter) => iter.flatten().collect(),
        Err(e) => {
            log::warn!("storage: stash_list query failed: {e}");
            Vec::new()
        }
    }
}

/// Liefert den geschwärzten Volltext eines Eintrags. `Err`, wenn die ID
/// nicht (mehr) existiert.
pub fn stash_get_text(id: i64) -> Result<String, String> {
    let conn = CONN.lock().expect("CONN mutex poisoned");
    conn.query_row(
        "SELECT redacted_text FROM stash WHERE id = ?1",
        params![id],
        |row| row.get::<_, String>(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => format!("Ablage-Eintrag {id} nicht gefunden"),
        other => other.to_string(),
    })
}

/// Schreibt den geschwärzten Volltext eines Eintrags ins System-Clipboard —
/// „Nochmal kopieren" aus der Ablage.
pub fn stash_copy(id: i64) -> Result<(), String> {
    let text = stash_get_text(id)?;
    crate::clipboard::write_clipboard_text(&text)
}

/// Löscht einen einzelnen Eintrag. Idempotent: eine bereits fehlende ID ist
/// kein Fehler.
pub fn stash_delete(id: i64) -> Result<(), String> {
    let conn = CONN.lock().expect("CONN mutex poisoned");
    conn.execute("DELETE FROM stash WHERE id = ?1", params![id])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Leert die gesamte Ablage und liefert die Anzahl gelöschter Einträge.
/// Vom „Alle löschen"-Button und optional beim App-Quit gerufen.
pub fn stash_clear() -> usize {
    let conn = CONN.lock().expect("CONN mutex poisoned");
    match conn.execute("DELETE FROM stash", []) {
        Ok(n) => {
            if n > 0 {
                log::info!("storage: cleared {n} stash entries");
            }
            n
        }
        Err(e) => {
            log::warn!("storage: stash_clear failed: {e}");
            0
        }
    }
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
        assert_eq!(
            restored,
            "Hallo Max Mustermann, erreichbar unter jan@example.de."
        );
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

    // ------------------------------------------------------- Ablage (Stash, WP-B)
    //
    // Alle Stash-Tests teilen sich die eine In-Memory-`stash`-Tabelle des
    // Test-Prozesses (CONN). Da es keinen Partitions-Schlüssel wie `case_id`
    // gibt, serialisiert dieser Lock die Stash-Tests gegeneinander; jeder
    // Test startet mit `stash_clear()` von sauberem Grund.
    static STASH_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn stash_test_guard() -> std::sync::MutexGuard<'static, ()> {
        let g = STASH_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        stash_clear();
        g
    }

    fn counts(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn stash_insert_list_get_delete_roundtrip() {
        let _guard = stash_test_guard();

        let id = stash_insert(
            "reversible",
            "Sehr geehrter «P_a4b»",
            "Sehr geehrter «P_a4b», Ihre IBAN «I_x9y».",
            &counts(&[("person", 1), ("iban", 1)]),
        );
        assert!(id > 0);

        let list = stash_list();
        assert_eq!(list.len(), 1);
        let meta = &list[0];
        assert_eq!(meta.id, id);
        assert_eq!(meta.mode, "reversible");
        assert_eq!(meta.title, "Sehr geehrter «P_a4b»");
        assert_eq!(
            meta.char_len,
            "Sehr geehrter «P_a4b», Ihre IBAN «I_x9y».".chars().count()
        );
        // created_at ist ISO-8601-UTC (…Z).
        assert!(meta.created_at.ends_with('Z') && meta.created_at.contains('T'));

        let text = stash_get_text(id).unwrap();
        assert_eq!(text, "Sehr geehrter «P_a4b», Ihre IBAN «I_x9y».");

        stash_delete(id).unwrap();
        assert!(stash_list().is_empty());
        assert!(
            stash_get_text(id).is_err(),
            "gelöschter Eintrag darf nicht mehr lesbar sein"
        );
    }

    #[test]
    fn stash_list_newest_first() {
        let _guard = stash_test_guard();
        let first = stash_insert("strict", "erster", "erster", &HashMap::new());
        let second = stash_insert("strict", "zweiter", "zweiter", &HashMap::new());
        let list = stash_list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, second, "neuester Eintrag zuerst");
        assert_eq!(list[1].id, first);
    }

    #[test]
    fn stash_title_truncation_and_whitespace() {
        // Whitespace-Läufe (Spaces, Tabs, Newlines) kollabieren zu je einem Space.
        assert_eq!(stash_title("a  b\t\n c   d"), "a b c d");
        // Rand-Whitespace fällt weg.
        assert_eq!(
            stash_title("   führend und  folgend   "),
            "führend und folgend"
        );

        // Genau 60 Zeichen bei Umlauten (2-Byte in UTF-8): es wird an
        // Char-Grenzen geschnitten, nicht an Byte-Offset 60.
        let umlauts = "ä".repeat(70);
        let title = stash_title(&umlauts);
        assert_eq!(title.chars().count(), 60);
        assert_eq!(title, "ä".repeat(60));

        // Emoji (4-Byte) an der 60er-Grenze wird ganz gehalten oder ganz
        // weggelassen — nie halbiert.
        let kept = format!("{}🎉", "x".repeat(59)); // 59 + 1 = 60 Zeichen
        assert_eq!(stash_title(&kept).chars().count(), 60);
        assert!(stash_title(&kept).ends_with('🎉'));

        let dropped = format!("{}🎉", "x".repeat(60)); // Emoji wäre Zeichen 61
        let t = stash_title(&dropped);
        assert_eq!(t.chars().count(), 60);
        assert!(!t.contains('🎉'), "Zeichen jenseits 60 fällt weg");
    }

    #[test]
    fn stash_title_derived_on_insert() {
        let _guard = stash_test_guard();
        // Der Titel wird vom Aufrufer un-vorbereitet gereicht; stash_insert
        // normalisiert (Whitespace, 60 Zeichen) selbst.
        let raw = format!("Viele   Wörter\tund\nZeilen {}", "z".repeat(80));
        let id = stash_insert("reversible", &raw, "voller text", &HashMap::new());
        let meta = stash_list().into_iter().find(|m| m.id == id).unwrap();
        assert_eq!(meta.title.chars().count(), 60);
        assert!(meta.title.starts_with("Viele Wörter und Zeilen "));
    }

    #[test]
    fn stash_entity_counts_json_roundtrip() {
        let _guard = stash_test_guard();
        let original = counts(&[("person", 2), ("iban", 1), ("email", 3)]);
        let id = stash_insert("reversible", "titel", "text", &original);
        let meta = stash_list().into_iter().find(|m| m.id == id).unwrap();
        assert_eq!(
            meta.entity_counts, original,
            "entity_counts überlebt JSON-Roundtrip"
        );
    }

    #[test]
    fn stash_clear_removes_all() {
        let _guard = stash_test_guard();
        stash_insert("strict", "a", "a", &HashMap::new());
        stash_insert("strict", "b", "b", &HashMap::new());
        let removed = stash_clear();
        assert_eq!(removed, 2);
        assert!(stash_list().is_empty());
    }
}
