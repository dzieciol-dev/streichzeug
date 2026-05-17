//! Token-Generierung im Format `«T_<base32hash>»`.
//!
//! # Format
//!
//! - `T` = Entity-Typ-Kürzel: `P` Person, `L` Location, `O` Organisation,
//!   `E` Email, `T` Telefon, `B` IBAN/Bank, `K` Kreditkarte, `S` Steuer-ID, `D` Datum.
//! - `<base32hash>` = die ersten 6 Zeichen aus `base32(HMAC-SHA256(case_secret, original))`.
//!   30 Bit Entropie ≈ 1G Werte → kollisionssicher bis ~30.000 Entitäten pro Case.
//!
//! # Eigenschaften
//!
//! - **Deterministisch innerhalb eines Cases** — gleiche Eingabe + gleicher Secret
//!   liefert immer dasselbe Token. Grundlage für konsistentes Mapping über mehrere
//!   Copy-Operationen hinweg.
//! - **Translationsresistent** — Französische Anführungszeichen `«»` werden von
//!   LLMs als atomare Symbole behandelt; sie kollidieren weder mit Markdown noch
//!   mit Template-Engines noch mit HTML.
//! - **Unabhängig zwischen Cases** — andere `case_secret` → komplett andere
//!   Token-Werte → keine Cross-Case-Korrelationen über Pseudonyme.

use base32::Alphabet;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Hash-Länge in Zeichen (base32). 6 Zeichen ≈ 30 Bit ≈ 1G möglicher Werte.
const HASH_CHARS: usize = 6;

/// Erzeugt das Token `«<entity_type>_<hash>»` für den gegebenen Original-String.
///
/// Implementierung: HMAC-SHA256 über (`case_secret`, `normalized_original`),
/// dann das Roh-Digest als base32 (lowercase, ohne Padding) kodieren und auf
/// `HASH_CHARS` kürzen.
///
/// # Normalisierung
///
/// Outlook und andere Mail-Clients fügen gerne unsichtbare Sonderzeichen
/// in den kopierten Text ein — typisch Non-Breaking-Space (`\u{00A0}`) in
/// Anschriften, Zero-Width-Joiner (`\u{200B}`) in formatierten Tabellen.
/// Ohne Normalisierung würden die zu unterschiedlichen Hashes führen, und
/// derselbe Name würde im Output zwei unterschiedliche Tokens bekommen.
/// Wir normalisieren daher vor dem HMAC:
///
/// - Trailing/leading Whitespace strippen
/// - `\u{00A0}` (NBSP) → reguläres Leerzeichen
/// - `\u{200B}` (Zero-Width-Space) und `\u{FEFF}` (BOM) entfernen
/// - Mehrfach-Whitespace kollabieren
///
/// # Sicherheit
///
/// HMAC statt nur SHA-256 schützt gegen Rainbow-Tables: ohne den `case_secret`
/// lässt sich aus dem Token nichts Sinnvolles über die Original-Eingabe ableiten.
pub fn make_token(entity_type: char, original: &str, case_secret: &[u8]) -> String {
    let normalized = normalize_for_hashing(original);
    let mut mac = HmacSha256::new_from_slice(case_secret).expect("HMAC accepts any key length");
    mac.update(normalized.as_bytes());
    let digest = mac.finalize().into_bytes();

    let encoded = base32::encode(Alphabet::Rfc4648Lower { padding: false }, &digest);
    let hash = &encoded[..HASH_CHARS];

    format!("«{entity_type}_{hash}»")
}

/// Normalisiert einen String für stabile Hash-/Label-Berechnung. Siehe
/// Docstring von [`make_token`] für die Beweggründe. Wird auch im
/// Strict-Mode genutzt, um Wiederholungen desselben Klartexts demselben
/// Platzhalter zuzuordnen.
pub fn normalize_for_hashing(s: &str) -> String {
    s.trim()
        .replace('\u{00A0}', " ") // NBSP → space
        .replace('\u{200B}', "") // Zero-Width-Space entfernen
        .replace('\u{FEFF}', "") // BOM entfernen
        .split_whitespace() // Mehrfach-Whitespace kollabieren
        .collect::<Vec<_>>()
        .join(" ")
}

/// Regex-Pattern, das alle Tokens im Format `«T_xxxxxx»` matcht.
///
/// Aktuell nur in Tests genutzt — vorgesehen für die Phase-2-Variante des
/// Reverse-Mappings, die Tokens auch in Texten ohne bekanntes Mapping findet
/// (z. B. um „verwaiste" Token-Hashes in Logs zu detektieren).
#[allow(dead_code)]
pub const TOKEN_REGEX_PATTERN: &str = r"«([PLOETBKSDU])_([a-z0-9]{6})»";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_format_is_correct() {
        let t = make_token('P', "Max Mustermann", b"secret");
        assert!(t.starts_with("«P_"));
        assert!(t.ends_with("»"));
        // «P_ + 6 chars + » = 10 chars (« und » sind je 2 Bytes / 1 Char)
        assert_eq!(t.chars().count(), 10);
    }

    #[test]
    fn same_input_same_token() {
        let a = make_token('E', "foo@bar.de", b"case-secret-1");
        let b = make_token('E', "foo@bar.de", b"case-secret-1");
        assert_eq!(a, b);
    }

    #[test]
    fn different_secret_different_token() {
        let a = make_token('E', "foo@bar.de", b"secret-A");
        let b = make_token('E', "foo@bar.de", b"secret-B");
        assert_ne!(a, b);
    }

    #[test]
    fn different_input_different_token() {
        let a = make_token('P', "Müller", b"secret");
        let b = make_token('P', "Schmidt", b"secret");
        assert_ne!(a, b);
    }

    #[test]
    fn token_regex_matches() {
        let pattern = regex::Regex::new(TOKEN_REGEX_PATTERN).unwrap();
        let t = make_token('B', "DE89370400440532013000", b"secret");
        assert!(pattern.is_match(&t));
    }

    #[test]
    fn nbsp_and_regular_space_produce_same_token() {
        // Outlook-Realität: Adresszeilen enthalten oft `\u{00A0}` (NBSP)
        // statt regulärem Space. Ohne Normalisierung würden „Tanja Daugill"
        // und „Tanja\u{00A0}Daugill" zwei unterschiedliche Tokens kriegen.
        let a = make_token('P', "Tanja Daugill", b"secret");
        let b = make_token('P', "Tanja\u{00A0}Daugill", b"secret");
        assert_eq!(a, b);
    }

    #[test]
    fn trailing_whitespace_does_not_change_token() {
        let a = make_token('P', "Tanja Daugill", b"secret");
        let b = make_token('P', "  Tanja Daugill\n", b"secret");
        let c = make_token('P', "Tanja  Daugill", b"secret"); // double space
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn url_short_code_in_regex() {
        let pattern = regex::Regex::new(TOKEN_REGEX_PATTERN).unwrap();
        let t = make_token('U', "https://example.com", b"secret");
        assert!(pattern.is_match(&t), "URL token didn't match regex: {t}");
    }
}
