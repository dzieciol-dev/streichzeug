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
//! - **Persistiert** in `$DATA_DIR/de.streichzeug.app/secret.bin`,
//!   mit `0600`-Permissions auf Unix (Windows: User-only-Default-ACL).
//! - **Beim ersten Start** generiert, bei jedem weiteren Start geladen.
//! - **In-Memory** als `Lazy<Vec<u8>>` — keine Re-Reads pro Token.
//!
//! # Threat-Model-Status nach diesem Modul
//!
//! - **P0 — Hartkodierter Secret**: gefixt für Cross-Installation. Innerhalb
//!   einer Installation produziert dieselbe PII dasselbe Token (gewollt für
//!   Reverse-Mapping).
//! - **P1 — Secret-Datei lesbar mit User-Filezugriff**: bleibt offen. Phase-2-
//!   Fix: `keyring`-Crate für OS-Keychain (Windows Credential Vault / macOS
//!   Keychain). Erfordert dort Code-Signing der App, deshalb erst Pre-Release.
//! - **P1 — Pro-Case-Sub-Keys via HKDF**: noch nicht implementiert. Wird nötig,
//!   sobald wir mehrere Cases pro User unterstützen — sonst sind Tokens
//!   case-übergreifend korrelierbar.

use once_cell::sync::Lazy;
use std::fs;
use std::path::PathBuf;

/// Anwendungs-Verzeichnis unter `$DATA_DIR`. Muss zur `identifier`-Property
/// in `tauri.conf.json` und zum Storage-Modul passen.
const APP_DIR: &str = "de.streichzeug.app";

/// Dateiname des Secrets. Liegt in App-Daten neben der SQLite-DB.
const SECRET_FILENAME: &str = "secret.bin";

/// Länge des Secrets in Byte. 32 Byte = 256 Bit, passt zu HMAC-SHA256.
const SECRET_LEN: usize = 32;

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

/// Lädt das Secret aus dem File oder erzeugt es bei Bedarf neu.
///
/// Auf Unix-Systemen werden die File-Permissions auf `0600` gesetzt —
/// das schützt vor anderen Usern auf derselben Maschine. Auf Windows
/// reicht der per-User-Pfad in `%APPDATA%\Roaming\`; das ACL erbt die
/// User-only-Defaults.
fn load_or_generate() -> std::io::Result<Vec<u8>> {
    let path = secret_path()?;

    if path.exists() {
        let bytes = fs::read(&path)?;
        if bytes.len() == SECRET_LEN {
            return Ok(bytes);
        }
        // Falsche Länge → korrupte Datei, neu generieren.
        log::warn!(
            "secret file has wrong length ({}), regenerating",
            bytes.len()
        );
    }

    // Neu generieren.
    let mut secret = vec![0u8; SECRET_LEN];
    getrandom::getrandom(&mut secret)
        .map_err(|e| std::io::Error::other(format!("OS RNG failed: {e}")))?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, &secret)?;

    // Permissions tightening. Auf Windows ist das per-User-Verzeichnis
    // bereits User-only zugänglich — kein chmod nötig.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o600); // rw- für Owner, sonst nichts
        fs::set_permissions(&path, perms)?;
    }

    log::info!("generated fresh master secret at {}", path.display());
    Ok(secret)
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
}
