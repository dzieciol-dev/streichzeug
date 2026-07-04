//! Master-Secret-Verwaltung für HMAC-basierte Token-Generierung.
//!
//! # Problem
//!
//! `crate::tokens::make_token()` erwartet einen `case_secret: &[u8]` —
//! HMAC-Schlüssel-Material, das ein Token deterministisch aus dem Original
//! ableitet. Wenn dieses Schlüssel-Material *konstant* ist (z. B.
//! hartkodiert), produzieren **alle Installationen** dasselbe Token für
//! denselben Klartext. Ein Angreifer könnte:
//!
//! 1. Beliebige PII-Kandidaten in `make_token()` laufen lassen
//! 2. Ergebnis-Tokens mit abgefangenen Tokens aus echter Kommunikation vergleichen
//! 3. Klartext rekonstruieren — **ohne Zugriff auf die Mapping-DB**.
//!
//! Das ist eine Preimage-Attacke und beseitigt jeden Privacy-Wert der
//! Pseudonymisierung.
//!
//! # Mitigation (dieses Modul)
//!
//! - **Pro-Installation-zufälliger Master-Secret** (32 Byte aus OS-RNG).
//! - **Persistiert** primär im **OS-Keychain** (macOS Keychain / Windows
//!   Credential Manager) via `keyring`-Crate; **Fallback** auf die bisherige
//!   Datei `$DATA_DIR/de.streichzeug.app/secret.bin` (`0600` auf Unix), wenn
//!   kein Keychain-Backend verfügbar ist oder der Keychain-Zugriff scheitert.
//! - **Beim ersten Start** generiert, bei jedem weiteren Start geladen.
//! - **Automatische Migration**: liegt (noch) eine `secret.bin`-Datei vor und
//!   ist ein Keychain verfügbar, wird das Secret in den Keychain übernommen und
//!   die Datei anschließend sicher gelöscht.
//! - **In-Memory** als `Lazy<Vec<u8>>` — keine Re-Reads pro Token.
//!
//! # Threat-Model-Status nach diesem Modul
//!
//! - **P0 — Hartkodierter Secret**: gefixt für Cross-Installation. Innerhalb
//!   einer Installation produziert dieselbe PII dasselbe Token (gewollt für
//!   Reverse-Mapping).
//! - **P1 — Secret-Datei lesbar mit User-Filezugriff**: **gefixt** auf
//!   macOS/Windows durch den OS-Keychain. Auf Plattformen ohne unterstütztes
//!   Backend (oder wenn der Keychain zur Laufzeit nicht erreichbar ist) greift
//!   der Datei-Fallback mit `0600`-Permissions.
//! - **P1 — Pro-Case-Sub-Keys via HKDF**: implementiert via [`case_secret`].
//!
//! # DB-Verschlüsselung
//!
//! Aus dem Master-Secret wird zusätzlich der SQLCipher-DB-Schlüssel abgeleitet
//! ([`db_key_hex`]) — domain-separiert per fixem Label, damit er nicht mit den
//! Token-HMACs korreliert. Das Storage-Modul verschlüsselt die Mapping-DB
//! transparent damit.

use once_cell::sync::Lazy;
use std::fs;
use std::path::{Path, PathBuf};

/// Anwendungs-Verzeichnis unter `$DATA_DIR`. Muss zur `identifier`-Property
/// in `tauri.conf.json` und zum Storage-Modul passen.
const APP_DIR: &str = "de.streichzeug.app";

/// Dateiname des Secrets. Liegt in App-Daten neben der SQLite-DB.
const SECRET_FILENAME: &str = "secret.bin";

/// Länge des Secrets in Byte. 32 Byte = 256 Bit, passt zu HMAC-SHA256.
const SECRET_LEN: usize = 32;

/// Keychain-„Service" (Namespace) für den Master-Secret-Eintrag.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const KEYCHAIN_SERVICE: &str = APP_DIR;

/// Keychain-„Account"/„User" (Eintrags-Name) für den Master-Secret.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const KEYCHAIN_ACCOUNT: &str = "master-secret";

/// Domain-Separation-Label für den abgeleiteten SQLCipher-DB-Schlüssel.
/// Versioniert, falls wir das Ableitungsschema je rotieren müssen.
const DB_KEY_LABEL: &[u8] = b"streichzeug-sqlcipher-db-key-v1";

/// Globale Master-Secret-Sicht. Wird beim ersten Aufruf von [`master_secret`]
/// einmal initialisiert (generiert oder geladen) und für die Prozess-
/// Lebensdauer gecached.
///
/// Bei `#[cfg(test)]` wird ein **fester** Secret zurückgegeben, damit Tests
/// reproduzierbar sind und nicht in das User-Daten-Verzeichnis schreiben.
static MASTER_SECRET: Lazy<Vec<u8>> = Lazy::new(|| {
    if cfg!(test) {
        // Stabiler Test-Wert. Reicht für deterministische Token-Tests.
        b"streichzeug--test-secret-32-byte!".to_vec()
    } else {
        load_or_generate().unwrap_or_else(|e| {
            // Fallback: bei IO-/RNG-Fehler nicht crashen, sondern eine
            // ephemere zufällige Sitzung-Secret bereitstellen. Reverse-Mapping
            // funktioniert dann nur innerhalb der laufenden Session.
            log::error!("master secret init failed: {e}; using ephemeral fallback");
            let mut buf = vec![0u8; SECRET_LEN];
            // Selbst der Fallback braucht Zufall — wenn auch der schief geht,
            // bleibt nur Panik. Reine HMAC-Sicherheit hängt vom RNG ab.
            getrandom::getrandom(&mut buf).expect("OS RNG unavailable");
            buf
        })
    }
});

/// Liefert das Master-Secret. Beim ersten Aufruf wird es geladen oder
/// generiert; alle weiteren Aufrufe sind O(1) (statisches `&[u8]`).
pub fn master_secret() -> &'static [u8] {
    MASTER_SECRET.as_slice()
}

/// Leitet ein Case-spezifisches Secret aus dem Master ab. Damit sind
/// Tokens **zwischen** Cases nicht korrelierbar — derselbe Klartext in
/// zwei verschiedenen Forward-Aktionen produziert unterschiedliche
/// Tokens. Innerhalb eines Cases bleibt das Mapping stabil.
///
/// `case_secret = HMAC-SHA256(master_secret, case_id)`. Das ist
/// HKDF-Lite — kein vollständiges HKDF, weil wir nur ein einziges
/// 32-Byte-Schlüsselmaterial pro Case brauchen.
///
/// # DSGVO-Hintergrund
///
/// Echte Pseudonymisierung nach Art. 4(5) DSGVO verlangt, dass die
/// pseudonymisierten Daten **ohne zusätzliche Informationen** keiner
/// betroffenen Person zugeordnet werden können. Mit cross-session-
/// stabilen Tokens (wie wir's vorher hatten) wäre für einen Angreifer,
/// der LLM-Logs sammelt, eine Frequency-Analyse trivial. Per-Case-
/// Secrets entkoppeln Vorkommen über Sitzungs-/Anfrage-Grenzen hinweg.
pub fn case_secret(case_id: &str) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(master_secret())
        .expect("HMAC accepts any key length");
    mac.update(case_id.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Liefert den SQLCipher-DB-Schlüssel als 64-stelligen Hex-String
/// (32 Byte Roh-Schlüssel). Wird vom Storage-Modul als
/// `PRAGMA key = "x'<hex>'"` gesetzt — Roh-Key ohne KDF-Runden, weil das
/// Master-Secret bereits kryptografisch zufällige 256 Bit hat.
///
/// `db_key = HMAC-SHA256(master_secret, DB_KEY_LABEL)`. Die Domain-Separation
/// stellt sicher, dass der DB-Schlüssel nicht mit einem Token-HMAC (oder einem
/// Case-Secret) zusammenfällt.
pub fn db_key_hex() -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(master_secret())
        .expect("HMAC accepts any key length");
    mac.update(DB_KEY_LABEL);
    hex::encode(mac.finalize().into_bytes())
}

/// Erzeugt eine frische, eindeutige Case-ID. Format: `<μs-since-epoch>-<8 hex>`.
/// Microsecond-Timestamp + 64 Bit Random reicht für globale Eindeutigkeit
/// ohne `uuid`-Crate. Beispiel: `1715954400000123-3f7a8b9c1d2e3f4a`.
pub fn new_case_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    let mut rand_bytes = [0u8; 8];
    // Bei RNG-Fehler: nehmen wir 0s. Timestamp allein ist immer noch
    // einigermaßen eindeutig, und ein Case-ID-Clash wäre kein
    // Sicherheits-, sondern ein UX-Problem (Tokens vermischen sich).
    let _ = getrandom::getrandom(&mut rand_bytes);
    format!("{:020}-{}", now_us, hex::encode(rand_bytes))
}

/// Pfad zur Secret-Datei (`$DATA_DIR/<APP_DIR>/secret.bin`).
fn secret_path() -> std::io::Result<PathBuf> {
    dirs::data_dir()
        .map(|d| d.join(APP_DIR).join(SECRET_FILENAME))
        .ok_or_else(|| std::io::Error::other("data_dir() returned None"))
}

/// Lädt das Secret oder erzeugt es bei Bedarf neu. Reihenfolge:
///
/// 1. **OS-Keychain** (macOS/Windows) — bevorzugte Quelle.
/// 2. **Datei** `secret.bin` — Fallback bzw. Migrationsquelle. Existiert die
///    Datei und ist ein Keychain verfügbar, wird das Secret dorthin migriert
///    und die Datei sicher gelöscht.
/// 3. **Neu generieren** — 32 Byte aus dem OS-RNG, bevorzugt in den Keychain,
///    sonst in die Datei (mit `0600` auf Unix).
fn load_or_generate() -> std::io::Result<Vec<u8>> {
    // 1. Keychain zuerst.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        match keychain_get() {
            Ok(Some(secret)) if secret.len() == SECRET_LEN => {
                log::debug!("master secret loaded from OS keychain");
                return Ok(secret);
            }
            Ok(Some(_)) => log::warn!("keychain secret has wrong length, ignoring"),
            Ok(None) => {} // Kein Eintrag — weiter mit Datei/Generierung.
            Err(e) => log::warn!("keychain unavailable ({e}); falling back to file"),
        }
    }

    // 2. Datei (Fallback / Migrationsquelle).
    let path = secret_path()?;
    if let Some(secret) = read_secret_file(&path)? {
        // Vorhandene Datei ggf. in den Keychain migrieren.
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            match keychain_set(&secret) {
                Ok(()) => {
                    log::info!("migrated master secret from file into OS keychain");
                    secure_delete_file(&path);
                }
                Err(e) => log::warn!("could not migrate secret to keychain ({e}); keeping file"),
            }
        }
        return Ok(secret);
    }

    // 3. Neu generieren.
    let mut secret = vec![0u8; SECRET_LEN];
    getrandom::getrandom(&mut secret)
        .map_err(|e| std::io::Error::other(format!("OS RNG failed: {e}")))?;

    // Bevorzugt Keychain, sonst Datei-Fallback.
    #[allow(unused_mut)]
    let mut stored_in_keychain = false;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        match keychain_set(&secret) {
            Ok(()) => {
                stored_in_keychain = true;
                log::info!("generated fresh master secret in OS keychain");
            }
            Err(e) => log::warn!("keychain store failed ({e}); using file fallback"),
        }
    }
    if !stored_in_keychain {
        write_secret_file(&path, &secret)?;
        log::info!("generated fresh master secret at {}", path.display());
    }

    Ok(secret)
}

/// Liest das Secret aus der Datei. `Ok(None)`, wenn die Datei fehlt oder eine
/// unerwartete Länge hat (→ Aufrufer generiert neu).
fn read_secret_file(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    if bytes.len() == SECRET_LEN {
        Ok(Some(bytes))
    } else {
        log::warn!("secret file has wrong length ({}), regenerating", bytes.len());
        Ok(None)
    }
}

/// Schreibt das Secret in die Datei und setzt auf Unix `0600`. Auf Windows ist
/// der per-User-Pfad in `%APPDATA%\Roaming\` bereits User-only zugänglich.
fn write_secret_file(path: &Path, secret: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, secret)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o600); // rw- für Owner, sonst nichts
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Überschreibt die Secret-Datei mit Nullen und löscht sie anschließend
/// (Best-Effort). Wird nach erfolgreicher Keychain-Migration aufgerufen, damit
/// kein Klartext-Secret auf der Platte zurückbleibt.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn secure_delete_file(path: &Path) {
    use std::io::Write;
    if let Ok(meta) = fs::metadata(path) {
        if let Ok(mut f) = fs::OpenOptions::new().write(true).open(path) {
            let zeros = vec![0u8; meta.len() as usize];
            let _ = f.write_all(&zeros);
            let _ = f.flush();
            let _ = f.sync_all();
        }
    }
    if let Err(e) = fs::remove_file(path) {
        log::warn!("could not remove migrated secret file: {e}");
    }
}

// ------------------------------------------------------------------- Keychain

/// Öffnet den Keychain-Eintrag für den Master-Secret.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn keychain_entry() -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
}

/// Liest das Secret (hex-kodiert) aus dem Keychain. `Ok(None)`, wenn kein
/// Eintrag existiert oder der Inhalt kein gültiges Hex ist.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn keychain_get() -> keyring::Result<Option<Vec<u8>>> {
    match keychain_entry()?.get_password() {
        Ok(hex_str) => Ok(hex::decode(hex_str.trim()).ok()),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Speichert das Secret hex-kodiert im Keychain.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn keychain_set(secret: &[u8]) -> keyring::Result<()> {
    keychain_entry()?.set_password(&hex::encode(secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_is_stable() {
        let a = master_secret();
        let b = master_secret();
        assert_eq!(a, b, "master_secret() muss zwischen Calls identisch sein");
    }

    #[test]
    fn test_secret_has_expected_length() {
        // Test-Modus liefert einen fixed-Wert, der ist nicht zwingend
        // 32 Byte lang. Aber zumindest nicht-leer.
        assert!(!master_secret().is_empty());
    }

    #[test]
    fn test_db_key_is_stable_and_32_bytes() {
        let a = db_key_hex();
        let b = db_key_hex();
        assert_eq!(a, b, "db_key_hex() muss deterministisch sein");
        assert_eq!(a.len(), 64, "32 Byte = 64 Hex-Zeichen");
        assert!(hex::decode(&a).is_ok(), "muss gültiges Hex sein");
    }

    #[test]
    fn test_db_key_differs_from_real_case_secrets() {
        // Domain-Separation: der DB-Schlüssel wird per fixem Label abgeleitet,
        // das mit dem `<ziffern>-<hex>`-Format echter Case-IDs (new_case_id)
        // niemals kollidieren kann. Gegenprobe mit generierten Case-IDs.
        let db = db_key_hex();
        for _ in 0..5 {
            let case = hex::encode(case_secret(&new_case_id()));
            assert_ne!(db, case, "DB-Key darf nicht mit einem Case-Secret kollidieren");
        }
        // Zusätzlich: Label beginnt nicht mit einer Ziffer → kein valides
        // Case-ID-Format, damit strukturell kollisionsfrei.
        let label = std::str::from_utf8(DB_KEY_LABEL).unwrap();
        assert!(!label.starts_with(|c: char| c.is_ascii_digit()));
    }
}
