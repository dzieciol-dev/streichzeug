//! Detection-Pipeline für personenbezogene Daten (PII).
//!
//! # Architektur
//!
//! Die Erkennung läuft in mehreren Layern, die nacheinander auf denselben
//! Eingabe-Text angewendet werden. Jeder Layer schreibt seine Treffer in
//! einen gemeinsamen `Vec<Finding>`. Am Ende dedupliziert
//! [`dedupe_and_sort`] überlappende Treffer (längster gewinnt).
//!
//! | Layer | Inhalt                                                       | Status |
//! |-------|--------------------------------------------------------------|--------|
//! | L1    | Regex + Format-Validierung (Email/IBAN/Telefon/CC/Steuer-ID/Datum) | ✓      |
//! | L2    | Gazetteer (~200 häufige DE-Vor-/Nachnamen)                   | ✓      |
//! | L2b   | Salutations-Kontext (Anrede + Großbuchstaben-Wort)           | ✓      |
//! | L2c   | Compound-Expansion (Vorname + unbekannter Nachname)          | ✓      |
//! | L3    | ONNX-NER (Personen/Orte/Organisationen ohne Heuristik)       | TODO   |
//!
//! # Output
//!
//! Jeder Treffer ([`Finding`]) enthält die Byte-Positionen in der Eingabe,
//! sodass [`apply_tokens`] den Originaltext rückwärts ersetzen kann, ohne
//! dass sich Indices verschieben.
//!
//! # Tokens
//!
//! Tokens haben das Format `«T_<base32hash>»` und werden über
//! [`crate::tokens::make_token`] deterministisch aus (Entity-Type,
//! Original-String, Case-Secret) abgeleitet. Gleicher Klartext im selben
//! Case → gleiches Token; das ist die Grundlage für Reverse-Mapping.

use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::tokens;

// =================================================================== Datentypen

/// Kategorie eines erkannten PII-Treffers.
///
/// Der Serializer benutzt snake_case (z. B. `"credit_card"`), damit die Werte
/// im JSON-Protokoll zur Browser-Extension stabil sind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    // Layer 1 — strukturierte Daten:
    Email,
    Phone,
    Iban,
    CreditCard,
    SteuerId,
    Date,
    Url,
    // Layer 2/3 — unstrukturierte Daten:
    Person,
    Location,
    Organization,
}

impl EntityType {
    /// Ein-Buchstaben-Kürzel für die Token-Erzeugung (`«P_a4b»`, `«E_zk9»`, ...).
    /// Dieselbe Konvention liest das Reverse-Mapping wieder ein.
    pub fn short_code(self) -> char {
        match self {
            EntityType::Person => 'P',
            EntityType::Location => 'L',
            EntityType::Organization => 'O',
            EntityType::Email => 'E',
            EntityType::Phone => 'T',
            EntityType::Iban => 'B',
            EntityType::CreditCard => 'K',
            EntityType::SteuerId => 'S',
            EntityType::Date => 'D',
            EntityType::Url => 'U',
        }
    }

    /// snake_case-Bezeichner für JSON-Serialisierung und Frontend-Vergleich.
    pub fn as_str(self) -> &'static str {
        match self {
            EntityType::Email => "email",
            EntityType::Phone => "phone",
            EntityType::Iban => "iban",
            EntityType::CreditCard => "credit_card",
            EntityType::SteuerId => "steuer_id",
            EntityType::Date => "date",
            EntityType::Person => "person",
            EntityType::Location => "location",
            EntityType::Organization => "organization",
            EntityType::Url => "url",
        }
    }
}

/// Ein erkanntes PII-Vorkommen.
///
/// `start` und `end` sind **Byte-Positionen** in der UTF-8-Eingabe (kein Char-Index!).
/// `original` ist der exakte gefundene Textabschnitt, `token` das Ersetzungs-Token,
/// das beim Pseudonymisieren an seine Stelle tritt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub entity_type: String,
    pub original: String,
    pub token: String,
    pub start: usize,
    pub end: usize,
    pub confidence: f32,
}

// =================================================================== Regex-Patterns
//
// Alle Patterns sind `Lazy<Regex>` — werden beim ersten Zugriff einmal kompiliert
// und für die Lebensdauer des Prozesses gecached. Spart das Wiederkompilieren
// pro Detection-Aufruf.

/// Email-Pattern, vereinfacht (kein voller RFC-5322).
/// Case-insensitiv, TLD 2–24 Zeichen (deckt `.museum`, `.travel` ab).
static RE_EMAIL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,24}\b").unwrap()
});

/// Deutsche Telefonnummern in gängigen Formaten:
/// `+49 89 12345678`, `0049-89-1234`, `089/12345678`, `089 12 34 56`.
///
/// `(?:^|\D)`/`(?:\D|$)` umrahmen die Kandidaten, damit innerhalb einer
/// langen Ziffernfolge (z. B. einer Kontonummer) keine Telefonnummer
/// halluziniert wird. Die finale Plausibilisierung (6–16 Ziffern) erfolgt
/// in [`collect_phones`].
static RE_PHONE: Lazy<Regex> = Lazy::new(|| {
    // `[\s/-]*` statt `?` zwischen den Gruppen — deutsche Schreibweisen wie
    // „04451 / 15 234" haben drei Trennzeichen (Leerzeichen, Slash, Leerzeichen).
    //
    // `[\d\s/-]{3,15}\d` für den Rest erlaubt zusätzlich, dass die Haupt-
    // nummer selbst Leerzeichen-gruppiert ist („15 234" statt „15234").
    Regex::new(
        r"(?x)
        (?:^|\D)
        (
          (?:\+|00)\s?49[\s/-]*\d{2,5}[\s/-]*[\d\s/-]{3,15}\d
          |
          0\d{2,4}[\s/-]*[\d\s/-]{3,15}\d
        )
        (?:\D|$)
        ",
    )
    .unwrap()
});

/// IBAN-Kandidaten: 2 Buchstaben Country-Code + 2 Prüfziffern + 10–30
/// alphanumerische Zeichen, optional mit Leerzeichen gruppiert.
///
/// Reine Form-Erkennung — die mod-97-Prüfsumme validiert das
/// `iban_validate`-Crate in [`collect_ibans`].
static RE_IBAN_CANDIDATE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b[A-Z]{2}\d{2}(?:\s?[A-Z0-9]){10,30}\b").unwrap());

/// Kreditkarten-Kandidaten: 13–19 Ziffern, optional gruppiert mit
/// Spaces oder Bindestrichen. Luhn-Check sortiert ungültige Folgen aus.
static RE_CREDIT_CARD_CANDIDATE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b(?:\d[ -]?){13,19}\b").unwrap());

/// Deutsche Steuer-ID-Kandidaten: 11 zusammenhängende Ziffern.
///
/// Die reine Form (11 Ziffern) ist als Kandidaten-Gate viel zu weit: jede
/// 11-stellige Zahl (Bestell-, Vorgangs-, Kundennummer) fällt hier rein, und
/// grob **1 von 10** solcher Zufallszahlen besteht die ISO-7064-Prüfsumme
/// zufällig. Deshalb reicht die Prüfsumme allein nicht — [`collect_steuer_ids`]
/// verlangt zusätzlich ein **Kontextwort** in der Nähe (Steuer-ID/IdNr/…) und
/// [`steuer_id_check`] prüft die formalen Strukturregeln (führende Ziffer ≠ 0,
/// genau eine Ziffer doppelt/dreifach) vollständig.
static RE_STEUER_ID_CANDIDATE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b\d{11}\b").unwrap());

/// Kontextwörter, die eine 11-stellige Zahl als Steuer-ID plausibilisieren.
/// Alle lowercase — der Abgleich passiert gegen einen kleingeschriebenen
/// Textausschnitt vor dem Kandidaten. Bewusst ohne „Steuernummer" (das ist
/// ein anderes, meist 10-/13-stelliges Format).
const STEUER_ID_CONTEXT_KEYWORDS: &[&str] = &[
    "steuer-id",
    "steuerid",
    "steuer id",
    "steuer-identifikationsnummer",
    "steuerliche identifikationsnummer",
    "steuerliche id",
    "identifikationsnummer",
    "idnr",
    "id-nr",
    "id.-nr",
    "tin",
];

/// Datum im deutschen `DD.MM.YYYY`-Format. Tag 01–31, Monat 01–12, Jahr
/// 19xx/20xx — nur Form-Check, **kein** echter Kalender-Check (30.02.2024
/// würde durchgelassen).
static RE_DATE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b(0?[1-9]|[12]\d|3[01])\.(0?[1-9]|1[0-2])\.((?:19|20)\d{2})\b").unwrap()
});

/// Kandidaten für den Gazetteer-Lookup: deutsches Großbuchstaben-Wort
/// (inkl. Umlaute), mindestens 2 Buchstaben. Erlaubt Bindestrich-Komposita
/// (z. B. „Anna-Maria", „Müller-Lüdenscheidt") — der Teil nach dem
/// Bindestrich beginnt typischerweise mit einem Großbuchstaben, daher
/// erlaubt der innere Bereich auch Großbuchstaben. Das letzte Zeichen
/// muss aber lowercase sein, damit der Regex keinen Satz-Mittel-Strich
/// fressen kann („Müller-").
static RE_NAME_CANDIDATE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b[A-ZÄÖÜ][a-zäöüß](?:[A-ZÄÖÜa-zäöüß\-]*[a-zäöüß])?\b").unwrap()
});

/// Salutations-Kontext: Anrede-Wort + 1–2 Großbuchstaben-Wörter.
///
/// Capture-Group 1 enthält **nur den Namen**, ohne die Anrede selbst.
/// `\p{Lu}\p{Ll}+` (Unicode) deckt fremde Namen wie „Kowalski",
/// „Kowalski", „Tanaka" ab. Bindestrich-Komposita („Müller-Lüdenscheidt",
/// „Anna-Maria") werden mitgefangen — der innere Bereich erlaubt
/// beliebige Buchstaben (auch Großbuchstaben nach Bindestrich), das
/// letzte Zeichen muss aber lowercase sein.
static RE_SALUTATION_NAME: Lazy<Regex> = Lazy::new(|| {
    // Titel ZWISCHEN Anrede und Name („Herr Dr. Demary", „Frau Prof. Dr.
    // Obst") werden übersprungen, ohne Teil der Capture zu sein — sonst
    // captured die Name-Gruppe das „Dr" als vermeintlichen Vornamen und der
    // echte Nachname bleibt ungeschwärzt (Beta-Befund 2026-07-06).
    Regex::new(
        r"\b(?:Herr|Hr\.|Hrn\.|Frau|Fr\.|Dr\.|Prof\.|Mag\.|Mr\.|Mrs\.|Ms\.)\s+(?:(?:Dr\.|Prof\.|Mag\.|Dipl\.-Ing\.)\s+)*(\p{Lu}\p{Ll}(?:[\p{L}\-]*\p{Ll})?(?:\s+\p{Lu}\p{Ll}(?:[\p{L}\-]*\p{Ll})?)?)\b",
    )
    .unwrap()
});

/// Outlook-Adressbuch-Format „Nachname, Vorname" — gängige Schreibweise in
/// Mail-Headern und Verteilerlisten. Beide Capture-Gruppen werden als
/// Person-Findings markiert, sofern der Vorname (Group 2) im Gazetteer steht.
static RE_OUTLOOK_NAME: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b([A-ZÄÖÜ]\p{Ll}+),\s+(\p{Lu}\p{Ll}+)\b").unwrap());

/// Deutsche Umsatzsteuer-Identifikationsnummer: `DE` + 9 Ziffern.
/// Klar abgrenzbar von der 11-stelligen Steuer-ID und von IBANs (die ≥18
/// alphanumerische Zeichen nach `DE` haben).
static RE_VAT_ID: Lazy<Regex> = Lazy::new(|| Regex::new(r"\bDE\d{9}\b").unwrap());

/// BIC nach ISO 9362: 4 Buchstaben Bankcode + Country (hier auf `DE`
/// festgenagelt, sonst zu viele False Positives bei Großbuchstaben-Blöcken
/// wie „RUSTSEC") + 2 alphanumerische Location, optional 3 weitere Zeichen
/// für die Filiale.
static RE_BIC: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b[A-Z]{4}DE[A-Z0-9]{2}(?:[A-Z0-9]{3})?\b").unwrap());

/// Deutsche Postleitzahl: 5 Ziffern, gefolgt von einem Großbuchstaben-Wort
/// (typischer Adressblock „44137 Dortmund"). Die Lookahead-Forderung
/// reduziert False Positives auf zufällige 5-Ziffern-Zahlen (z. B.
/// Vorgangsnummern). Wir capturen nur die PLZ-Ziffern.
static RE_PLZ_WITH_CITY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b(\d{5})\s+\p{Lu}\p{Ll}+\b").unwrap());

/// Straßenangabe mit Hausnummer. Erkennt typische deutsche Straßennamen-
/// Suffixe (-straße, -str., -weg, -gasse, -platz, -allee, -ring, …) plus
/// folgende Hausnummer mit optionalem Zusatz-Buchstaben („14b").
///
/// Erlaubt zusammengesetzte Straßennamen mit Bindestrichen und interior
/// Großbuchstaben (z. B. „Bürgermeister-Heidenreich-Str."), und matcht
/// das Suffix case-insensitiv (sowohl „Str." als auch „str.").
static RE_STREET: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"\b[A-ZÄÖÜ][A-Za-zÄÖÜäöüß\-]+(?i:straße|str\.|weg|gasse|platz|allee|ring|chaussee|damm|ufer|markt)\s+\d+[a-zA-Z]?\b",
    )
    .unwrap()
});

/// URLs/Hostnames. Erkennt sowohl `http(s)://…`-Schemata als auch nackte
/// `www.…`-Hostnames. TLD muss ≥ 2 Buchstaben sein (`*.de`, `*.com`,
/// `*.co.uk` etc.). Pfad/Query/Fragment optional.
///
/// Der Pfad-Teil hat eine zusätzliche Bedingung: er darf nicht auf
/// **Satzzeichen** (`.,;:!?`) enden. Andernfalls würde bei einem URL am
/// Satzende — wie „Mehr auf https://example.de/seite." — der schließende
/// Punkt fälschlich Teil der URL und damit Teil des Tokens. Erlaubt ist
/// weiterhin ein Pfad bestehend nur aus `/` (Root-URL mit Trailing-Slash).
static RE_URL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"\b(?:https?://|www\.)[A-Za-z0-9][A-Za-z0-9.\-]*\.[A-Za-z]{2,}(?:/(?:[^\s<>()]*[^\s<>().,;:!?])?)?",
    )
    .unwrap()
});

/// Organisations-Pattern: 1–4 Großbuchstaben-Worte gefolgt von einer
/// deutschen Rechtsform-Endung. Capture-Group 1 enthält den **kompletten
/// Firmennamen** inkl. Suffix, damit beim Reverse-Mapping die Endung
/// erhalten bleibt.
///
/// Beispiele die matchen:
///   - „Acme GmbH"
///   - „Müller & Söhne KG"
///   - „Deutsche Bahn AG"
///   - „Beispiel e.V."
static RE_ORGANIZATION: Lazy<Regex> = Lazy::new(|| {
    // `\b` am Ende reicht nicht für Suffixe, die auf `.` enden (z. B. „e.V."):
    // dort ist die Word-Boundary nicht gegeben. Stattdessen explizit
    // „gefolgt von Whitespace, Satzzeichen oder Stringende" via `(?:\W|$)`,
    // außerhalb der Capture-Group.
    Regex::new(
        r"\b([A-ZÄÖÜ][\wäöüß\.&\-]*(?:\s+(?:&|\w)[\wäöüß\.&\-]*){0,4}\s+(?:GmbH|gGmbH|AG|SE|KG|OHG|UG|e\.V\.|eG))(?:\W|$)",
    )
    .unwrap()
});

/// Städte-Kandidaten — identisch zur Personen-Regex, der Gazetteer-Lookup
/// entscheidet. Wiederverwendung von RE_NAME_CANDIDATE wäre kürzer, aber
/// expliziter Name macht Debugging einfacher.
///
/// Erlaubt Bindestriche **innerhalb** des Wortes (z. B. „Baden-Baden",
/// „Castrop-Rauxel", „Müllheim-Britzingen") — nicht am Anfang/Ende.
static RE_CITY_CANDIDATE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b[A-ZÄÖÜ][a-zäöüß]{2,}(?:-[A-ZÄÖÜa-zäöüß][a-zäöüß]{1,})*\b").unwrap());

// =================================================================== Öffentliches API

/// Erkennt alle PII-Findings im Eingabe-Text **mit dem Default-Master-
/// Secret** — geeignet für UI-Tests / Detection-Anzeigen ohne
/// Mapping-Speicherung. Für Forward-Operationen mit Reverse-Mapping
/// sollte stattdessen [`detect_with_case`] verwendet werden, das per-
/// Forward unterschiedliche Tokens erzeugt (DSGVO-Pseudonymisierung).
pub fn detect(text: &str) -> Vec<Finding> {
    detect_inner(text, crate::secrets::master_secret())
}

/// Wie [`detect`], aber mit einem case-spezifischen Secret. Damit
/// werden Tokens **per Forward-Vorgang einzigartig** — derselbe Name
/// in zwei Forward-Aktionen kriegt zwei verschiedene Tokens, was
/// Cross-Session-Korrelation für einen Beobachter (z. B. LLM-Logs)
/// verhindert. Innerhalb desselben `case_id` bleibt das Mapping
/// stabil (Wiederholungen im Text werden konsistent ersetzt).
pub fn detect_with_case(text: &str, case_id: &str) -> Vec<Finding> {
    let secret = crate::secrets::case_secret(case_id);
    detect_inner(text, &secret)
}

/// Interne Detection-Pipeline. Die Layer-Reihenfolge ist signifikant:
/// erst strukturierte (eindeutige) Patterns, dann Personen. Am Ende
/// dedupliziert [`dedupe_and_sort`] überlappende Treffer — der längere
/// gewinnt, sodass z. B. „Max Mustermann" das isolierte „Max" verdrängt.
fn detect_inner(text: &str, case_secret: &[u8]) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Layer 1 — strukturierte Daten:
    collect_emails(text, &mut findings, case_secret);
    collect_phones(text, &mut findings, case_secret);
    collect_ibans(text, &mut findings, case_secret);
    collect_credit_cards(text, &mut findings, case_secret);
    collect_steuer_ids(text, &mut findings, case_secret);
    collect_dates(text, &mut findings, case_secret);

    // Layer 1 — weitere strukturierte Daten:
    collect_vat_ids(text, &mut findings, case_secret);
    collect_bics(text, &mut findings, case_secret);
    collect_urls(text, &mut findings, case_secret);

    // Layer 2 — Personen:
    collect_persons(text, &mut findings, case_secret); // L2  Gazetteer-Lookup
    collect_persons_by_salutation(text, &mut findings, case_secret); // L2b Anrede-Kontext
    collect_persons_outlook_format(text, &mut findings, case_secret); // L2b' Outlook „Nachname, Vorname"
    expand_person_findings(text, &mut findings, case_secret); // L2c Compound-Expansion

    // Layer 2 — Orte und Organisationen (Gazetteer + Suffix-Pattern):
    collect_locations(text, &mut findings, case_secret);
    collect_organizations(text, &mut findings, case_secret);
    collect_postal_codes(text, &mut findings, case_secret);
    collect_streets(text, &mut findings, case_secret);

    // Layer 3 — Statistische NER (optional, no-op ohne Feature/Modell).
    collect_ner_findings(text, &mut findings, case_secret);

    dedupe_and_sort(&mut findings);
    findings
}

/// L3 — Layer-3 NER. Ruft das lokale ONNX-Modell auf, mappt die
/// PER/ORG/LOC-Spans auf unsere Entity-Types und pusht sie als Findings.
///
/// Wird durch `dedupe_and_sort` mit L1/L2-Findings entkollidiert — wenn
/// das NER-Modell `"Sparkasse Dortmund"` als ORG findet und `"Dortmund"`
/// schon als Location vom Gazetteer kommt, gewinnt der breitere
/// (längere) Span. Die Dedupe-Logik bevorzugt längere Spans.
fn collect_ner_findings(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    for f in crate::ner::classify(text) {
        let entity_type = match f.entity_type.as_str() {
            "person" => EntityType::Person,
            "organization" => EntityType::Organization,
            "location" => EntityType::Location,
            "date" => EntityType::Date,
            _ => continue,
        };
        push_finding(out, entity_type, f.text, f.start, f.end, f.confidence, secret);
    }
}

/// Ersetzt alle Findings in `text` durch ihre Tokens.
///
/// Die Findings werden **absteigend** nach Start-Position verarbeitet — sonst
/// würden die noch nicht verarbeiteten Indices durch die Längendifferenz
/// (Original vs. Token) verschoben.
///
/// Beispiel: `[finding_at_5..10, finding_at_20..30]` — erst 20..30 ersetzen,
/// dann 5..10. Andersrum würde der zweite Index nach der ersten Ersetzung
/// auf den falschen Substring zeigen.
///
/// `case_id` ist aktuell ungenutzt — kommt mit dem Case-Manager dazu (für
/// Per-Case-Persistenz des Mappings).
pub fn apply_tokens(text: &str, findings: &[Finding], _case_id: &str) -> String {
    let mut sorted: Vec<&Finding> = findings.iter().collect();
    // Absteigend nach start sortieren (Reverse) — siehe Docstring oben.
    sorted.sort_by_key(|f| std::cmp::Reverse(f.start));

    let mut result = text.to_string();
    for f in sorted {
        // Defensiv-Check: schützt vor Findings mit kaputten Indices.
        if f.start < f.end && f.end <= result.len() {
            result.replace_range(f.start..f.end, &f.token);
        }
    }
    result
}

/// LLM-Hinweis, der ans Ende eines pseudonymisierten Texts gehängt wird —
/// erklärt dem Modell, was die Marker bedeuten, und bittet um unveränderte
/// Übernahme. Kurz gehalten (kostet sonst Tokens und Aufmerksamkeit).
pub(crate) const LLM_HINT: &str =
    "\n\n---\nHinweis: Die Marker im Format «X_yyy» im obigen Text sind Pseudonyme für \
     personenbezogene Daten. Bitte exakt so in deiner Antwort übernehmen — nicht verändern, \
     übersetzen oder ergänzen. Sie werden anschließend automatisch zurück übersetzt.";

/// Pseudonymisiert den Text und hängt den LLM-Hinweis an, **wenn** mindestens
/// ein Token gesetzt wurde. Bei leerem `findings` bleibt der Originaltext
/// unverändert (kein nerviger Footer ohne Grund).
pub fn apply_tokens_with_hint(text: &str, findings: &[Finding], case_id: &str) -> String {
    let pseudonymized = apply_tokens(text, findings, case_id);
    if findings.is_empty() {
        pseudonymized
    } else {
        format!("{pseudonymized}{LLM_HINT}")
    }
}

// =================================================================== Strict Mode

/// Strict-Mode-LLM-Hint. Klartext-Platzhalter statt Token-Pattern, plus
/// kein „werden automatisch zurück übersetzt"-Versprechen — im Strict
/// Mode gibt's keine Reverse-Phase.
pub(crate) const LLM_HINT_STRICT: &str =
    "\n\n---\nHinweis: Die Marker im Format «Person A», «Organisation B», «Ort C» etc. \
     im obigen Text sind anonymisierte Platzhalter für personenbezogene Daten. \
     Bitte exakt so in deiner Antwort übernehmen — nicht verändern, übersetzen oder ergänzen.";

/// Strict-Mode-Detection: PII findet wie üblich, aber das `token`-Feld
/// jedes Findings ist ein lesbarer sequenzieller Platzhalter
/// (z. B. „«Person A»") statt eines HMAC-Hashes.
///
/// **Kein Mapping wird gespeichert.** Das Original ist nach dem Apply-
/// Schritt nur noch in der lokalen `Vec<Finding>` — danach geht's an
/// die Garbage Collection. Die Zuordnung Token→Original existiert
/// damit nirgends persistent, was die Tokens beim LLM zu **anonymen
/// Daten** im Sinne von ErwGr. 26 DSGVO macht.
pub fn detect_strict(text: &str) -> Vec<Finding> {
    // Findings sammeln mit einem dummy-Secret — die HMAC-Tokens werden
    // gleich überschrieben, der Wert ist also egal. Wir nehmen
    // master_secret damit detection-interne Tests konsistent bleiben.
    let mut findings = detect_inner(text, crate::secrets::master_secret());
    assign_strict_labels(&mut findings);
    findings
}

/// Wie [`apply_tokens_with_hint`], aber mit dem Strict-Mode-Hint.
pub fn apply_strict_with_hint(text: &str, findings: &[Finding]) -> String {
    let anonymized = apply_tokens(text, findings, "strict");
    if findings.is_empty() {
        anonymized
    } else {
        format!("{anonymized}{LLM_HINT_STRICT}")
    }
}

/// Weist jedem Finding einen sequenziellen Klartext-Platzhalter zu
/// (z. B. „«Person A»", „«Person B»", „«Organisation A»"). Wiederholungen
/// desselben normalisierten Originals bekommen denselben Platzhalter —
/// damit ein LLM weiß, dass dieselbe Entität erneut auftritt.
fn assign_strict_labels(findings: &mut [Finding]) {
    use std::collections::HashMap;

    // Reihenfolge im Text bestimmt die Buchstaben-Vergabe — wer zuerst
    // im Original-Text erscheint, kriegt „A".
    findings.sort_by_key(|f| f.start);

    // Pro entity_type ein eigener Counter — Person und Organisation
    // zählen parallel und unabhängig.
    let mut counters: HashMap<String, usize> = HashMap::new();
    // (entity_type, normalized_original) → bereits vergebener Platzhalter
    let mut assigned: HashMap<(String, String), String> = HashMap::new();

    for f in findings.iter_mut() {
        let normalized = crate::tokens::normalize_for_hashing(&f.original);
        let key = (f.entity_type.clone(), normalized);
        let label = if let Some(existing) = assigned.get(&key) {
            existing.clone()
        } else {
            let counter = counters.entry(f.entity_type.clone()).or_insert(0);
            *counter += 1;
            let new_label = format_strict_label(&f.entity_type, *counter);
            assigned.insert(key, new_label.clone());
            new_label
        };
        f.token = format!("«{label}»");
    }
}

/// Bildet aus Entity-Type und 1-basiertem Counter einen Lesbar-Platzhalter
/// wie „Person A". Bei mehr als 26 Vorkommen pro Type: „Person AA",
/// „Person AB", …, analog zu Excel-Spalten.
fn format_strict_label(entity_type: &str, counter: usize) -> String {
    let prefix = match entity_type {
        "person" => "Person",
        "organization" => "Organisation",
        "location" => "Ort",
        "email" => "E-Mail",
        "phone" => "Telefon",
        "iban" => "Bankverbindung",
        "credit_card" => "Kreditkarte",
        "steuer_id" => "Steuer-ID",
        "date" => "Datum",
        "url" => "URL",
        _ => "Daten",
    };
    format!("{prefix} {}", letter_for_index(counter))
}

/// Wandelt einen 1-basierten Counter in eine Excel-artige Buchstaben-
/// Sequenz: 1→A, 2→B, …, 26→Z, 27→AA, 28→AB, … 52→AZ, 53→BA, …
fn letter_for_index(mut n: usize) -> String {
    let mut s = String::new();
    while n > 0 {
        n -= 1;
        let c = (b'A' + (n % 26) as u8) as char;
        s.insert(0, c);
        n /= 26;
    }
    s
}

// =================================================================== Collectors
//
// Jeder Collector liest dieselbe Eingabe und appended Treffer in `out`.
// Sie sind absichtlich nicht abstrahiert (kein `Recognizer`-Trait) — die
// einzelnen Implementierungen sind klein genug, dass die Indirektion mehr
// Verständnis-Cost als Wartungs-Gewinn brächte.

/// Generischer Collector für **reine Regex-Match-Collectors** — also solche,
/// die jeden Volltreffer der Regex ohne Zusatz-Validierung oder Capture-Group-
/// Logik als Finding übernehmen (Email, Datum, USt-IdNr, BIC, URL, Straße).
///
/// Bewusst eine freie Funktion statt eines `Recognizer`-Traits: die Collectors
/// unterscheiden sich nur in Regex, Entity-Type und Confidence — ein
/// Trait-Objekt brächte hier nur Indirektion ohne Gewinn. Collectors mit
/// Capture-Groups, Prüfsummen oder Gazetteer-Lookup bleiben eigenständig.
fn collect_regex_matches(
    text: &str,
    out: &mut Vec<Finding>,
    re: &Regex,
    entity_type: EntityType,
    confidence: f32,
    secret: &[u8],
) {
    for m in re.find_iter(text) {
        push_finding(out, entity_type, m.as_str(), m.start(), m.end(), confidence, secret);
    }
}

/// Erzeugt aus den Match-Daten ein [`Finding`] und appended es. Reduziert
/// Boilerplate in den Collectors.
fn push_finding(
    out: &mut Vec<Finding>,
    entity_type: EntityType,
    original: impl Into<String>,
    start: usize,
    end: usize,
    confidence: f32,
    secret: &[u8],
) {
    let original = original.into();
    let token = tokens::make_token(entity_type.short_code(), &original, secret);
    out.push(Finding {
        entity_type: entity_type.as_str().into(),
        original,
        token,
        start,
        end,
        confidence,
    });
}

/// Email — Pattern-Match, keine MX-Verifikation.
fn collect_emails(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    collect_regex_matches(text, out, &RE_EMAIL, EntityType::Email, 0.99, secret);
}

/// Telefon — Pattern-Match + Ziffernzahl-Sanity (6–16). Capture-Group 1
/// liefert die reine Nummer ohne führendes/folgendes Trennzeichen.
fn collect_phones(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    for caps in RE_PHONE.captures_iter(text) {
        let Some(m) = caps.get(1) else { continue };
        let original = m.as_str();
        let digit_count = original.chars().filter(|c| c.is_ascii_digit()).count();
        // 6 ≤ Ziffern ≤ 16 — sortiert kurze Buchungsnummern und IBAN-Fragmente aus.
        if !(6..=16).contains(&digit_count) {
            continue;
        }
        // Hex-Literale (`0x…`) und Hashes rutschen sonst als „Telefonnummer"
        // durch — `(?:^|\D)` im Pattern lässt das `x` bzw. einen Hex-Buchstaben
        // als Begrenzer zu.
        if is_part_of_hex_sequence(text, m.start(), m.end()) {
            continue;
        }
        push_finding(out, EntityType::Phone, original, m.start(), m.end(), 0.85, secret);
    }
}

/// Prüft, ob der Kandidat (`text[start..end]`, reine Ziffernfolge ggf. mit
/// Trennzeichen) in Wahrheit Teil eines Hex-Literals oder einer Hex-Sequenz
/// ist:
///
/// 1. **`0x`/`0X`-Präfix** — direkt vor dem Kandidaten, ggf. über weitere
///    Hex-Ziffern hinweg (`0xdead0123456789` → Kandidat `0123456789`).
/// 2. **Nahtloser Übergang in Hex-Buchstaben** (a–f/A–F) unmittelbar vor
///    oder nach dem Kandidaten — typisch für Git-SHAs und andere Hashes
///    (`0891234deadbeef`). Echte Telefonnummern kleben nie ohne Trennzeichen
///    an einem Buchstaben.
fn is_part_of_hex_sequence(text: &str, start: usize, end: usize) -> bool {
    let prefix = &text[..start];

    // Fall 1: rückwärts über Hex-Ziffern bis zu einem etwaigen `0x`-Präfix.
    let before_hex_run = prefix.trim_end_matches(|c: char| c.is_ascii_hexdigit());
    if (before_hex_run.ends_with('x') || before_hex_run.ends_with('X'))
        && before_hex_run[..before_hex_run.len() - 1].ends_with('0')
    {
        return true;
    }

    // Fall 2: direkt angrenzende Hex-Buchstaben.
    let is_hex_letter = |c: char| c.is_ascii_hexdigit() && c.is_ascii_alphabetic();
    prefix.chars().next_back().is_some_and(is_hex_letter)
        || text[end..].chars().next().is_some_and(is_hex_letter)
}

/// IBAN — Form-Match + mod-97-Prüfsumme via `iban_validate`-Crate.
fn collect_ibans(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    for m in RE_IBAN_CANDIDATE.find_iter(text) {
        let candidate = m.as_str();
        // Leerzeichen entfernen, damit der Parser nicht stolpert.
        let cleaned: String = candidate.chars().filter(|c| !c.is_whitespace()).collect();
        if cleaned.parse::<iban::Iban>().is_err() {
            continue;
        }
        push_finding(out, EntityType::Iban, candidate, m.start(), m.end(), 0.99, secret);
    }
}

/// Kreditkarte — Form-Match + Luhn-Prüfsumme.
fn collect_credit_cards(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    for m in RE_CREDIT_CARD_CANDIDATE.find_iter(text) {
        let candidate = m.as_str();
        let digits: String = candidate.chars().filter(|c| c.is_ascii_digit()).collect();
        if !(13..=19).contains(&digits.len()) || !luhn_check(&digits) {
            continue;
        }
        push_finding(out, EntityType::CreditCard, candidate, m.start(), m.end(), 0.95, secret);
    }
}

/// Deutsche Steuer-ID — Form-Match + Kontextwort + formale Strukturprüfung.
///
/// Zwei Gates gegen False Positives auf harmlose 11-stellige Zahlen:
/// 1. **Kontextwort** (Steuer-ID/IdNr/…) muss kurz vor dem Kandidaten stehen —
///    ohne diesen Kontext ist die Prüfsumme allein zu schwach (~1:10 Zufalls-
///    treffer).
/// 2. [`steuer_id_check`] verifiziert die formalen Strukturregeln **und** die
///    ISO-7064-MOD-11,10-Prüfsumme.
fn collect_steuer_ids(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    for m in RE_STEUER_ID_CANDIDATE.find_iter(text) {
        let candidate = m.as_str();
        if !has_steuer_id_context(text, m.start()) {
            continue;
        }
        if !steuer_id_check(candidate) {
            continue;
        }
        push_finding(out, EntityType::SteuerId, candidate, m.start(), m.end(), 0.90, secret);
    }
}

/// Prüft, ob eines der [`STEUER_ID_CONTEXT_KEYWORDS`] in einem Fenster von
/// ~40 Zeichen **vor** dem Kandidaten (Byte-Offset `at`) steht. Das Fenster
/// wird auf einer Zeichen-Grenze abgeschnitten und kleingeschrieben verglichen.
fn has_steuer_id_context(text: &str, at: usize) -> bool {
    const WINDOW_CHARS: usize = 40;
    // Byte-Index der frühesten der letzten WINDOW_CHARS Zeichen vor `at`.
    let prefix = &text[..at];
    let window_start = prefix
        .char_indices()
        .rev()
        .take(WINDOW_CHARS)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    let window = prefix[window_start..].to_lowercase();
    STEUER_ID_CONTEXT_KEYWORDS.iter().any(|kw| window.contains(kw))
}

/// Datum — reines Format-Match, kein Kalender-Plausibilitäts-Check.
fn collect_dates(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    collect_regex_matches(text, out, &RE_DATE, EntityType::Date, 0.80, secret);
}

/// L2 — Personen via Gazetteer.
///
/// Jedes deutsche Großbuchstaben-Wort wird gegen die Namensliste in
/// [`crate::gazetteer`] geprüft. HashSet-Lookup → O(1), aber blind für
/// fremde Namen, die nicht auf der Liste stehen.
fn collect_persons(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    for m in RE_NAME_CANDIDATE.find_iter(text) {
        let candidate = m.as_str();
        if !crate::gazetteer::is_known_name(&candidate.to_lowercase()) {
            continue;
        }
        push_finding(out, EntityType::Person, candidate, m.start(), m.end(), 0.70, secret);
    }
}

/// L1 — Deutsche Umsatzsteuer-Identifikationsnummer (USt-IdNr).
/// Format DE + 9 Ziffern, verwendet als Entity-Type SteuerId (gleiche Kategorie
/// wie die normale Steuer-Identifikationsnummer aus User-Sicht).
fn collect_vat_ids(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    collect_regex_matches(text, out, &RE_VAT_ID, EntityType::SteuerId, 0.99, secret);
}

/// L1 — BIC (SWIFT-Code für Banküberweisungen). Behandelt als Entity-Type
/// Iban, weil's in derselben Bank-Kategorie liegt und der Token-Buchstabe
/// `B` aussagekräftig bleibt.
fn collect_bics(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    collect_regex_matches(text, out, &RE_BIC, EntityType::Iban, 0.95, secret);
}

/// L1 — URLs und nackte Hostnames (`https://…`, `http://…`, `www.…`).
/// Domains können personenbezogene Informationen verraten (Arbeitgeber,
/// regionale Sparkasse, persönliche Homepage) und werden daher als PII
/// behandelt.
fn collect_urls(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    // Email-Adressen enthalten `@`, das matcht unser Pattern nicht —
    // also keine Konflikte mit `collect_emails`.
    collect_regex_matches(text, out, &RE_URL, EntityType::Url, 0.95, secret);
}

/// L2 — Postleitzahl + Stadt-Pattern. Wir tokenisieren nur die PLZ-Ziffern
/// (Group 1); die Stadt wird separat von `collect_locations` behandelt,
/// sofern sie im Gazetteer steht.
fn collect_postal_codes(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    for caps in RE_PLZ_WITH_CITY.captures_iter(text) {
        let Some(plz) = caps.get(1) else { continue };
        push_finding(
            out,
            EntityType::Location,
            plz.as_str(),
            plz.start(),
            plz.end(),
            0.85,
            secret,
        );
    }
}

/// L2 — Straßenangaben mit Hausnummer. Erkennt die Endung -straße/-str./-weg
/// etc.; reine Eigennamen-Straßen ohne Suffix („Freistuhl") werden nicht
/// erfasst (zu hohe False-Positive-Rate).
fn collect_streets(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    collect_regex_matches(text, out, &RE_STREET, EntityType::Location, 0.85, secret);
}

/// L2b' — Outlook-Adressbuch-Konvention „Nachname, Vorname".
///
/// Markiert beide Wörter als Person-Findings, **wenn** der Vorname-Teil im
/// Gazetteer steht. Das verhindert False Positives bei zufälligen
/// „Wort, Wort"-Konstrukten („Berlin, der …", „Hauses, das …").
fn collect_persons_outlook_format(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    for caps in RE_OUTLOOK_NAME.captures_iter(text) {
        let (Some(lastname), Some(firstname)) = (caps.get(1), caps.get(2)) else {
            continue;
        };
        if !crate::gazetteer::is_known_name(&firstname.as_str().to_lowercase()) {
            continue;
        }
        push_finding(
            out,
            EntityType::Person,
            lastname.as_str(),
            lastname.start(),
            lastname.end(),
            0.85,
            secret,
        );
        push_finding(
            out,
            EntityType::Person,
            firstname.as_str(),
            firstname.start(),
            firstname.end(),
            0.85,
            secret,
        );
    }
}

/// L2b — Personen via Salutations-Kontext.
///
/// Fängt Namen ab, die das Gazetteer nicht kennt: alles, was direkt
/// auf „Herr/Frau/Dr./Prof./Mr./Mrs." folgt, gilt als Person — auch wenn
/// der Name fremd ist (Beispiele: „Herr Kowalski", „Dr. Tanaka").
fn collect_persons_by_salutation(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    for caps in RE_SALUTATION_NAME.captures_iter(text) {
        let Some(m) = caps.get(1) else { continue };
        push_finding(out, EntityType::Person, m.as_str(), m.start(), m.end(), 0.75, secret);
    }
}

/// L2 — Orte via Gazetteer-Lookup gegen [`crate::gazetteer::is_known_city`].
fn collect_locations(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    for m in RE_CITY_CANDIDATE.find_iter(text) {
        let candidate = m.as_str();
        if !crate::gazetteer::is_known_city(&candidate.to_lowercase()) {
            continue;
        }
        push_finding(out, EntityType::Location, candidate, m.start(), m.end(), 0.80, secret);
    }
}

/// L2 — Organisationen via Suffix-Pattern.
///
/// Nur die deutsche Rechtsform-Endung wird erkannt (GmbH/AG/KG/e.V. etc.).
/// US-Firmen ohne Suffix („Microsoft", „Google") werden hier nicht gefunden —
/// dafür braucht's Layer 3 NER.
fn collect_organizations(text: &str, out: &mut Vec<Finding>, secret: &[u8]) {
    for caps in RE_ORGANIZATION.captures_iter(text) {
        let Some(m) = caps.get(1) else { continue };
        push_finding(out, EntityType::Organization, m.as_str(), m.start(), m.end(), 0.85, secret);
    }
}

/// L2c — Compound-Expansion.
///
/// Wenn ein erkannter Person-Treffer direkt von einem weiteren
/// Großbuchstaben-Wort gefolgt wird, gehört dieses wahrscheinlich zum Namen
/// (Vorname aus dem Gazetteer + unbekannter Nachname, z. B. „Jan Kowalski").
///
/// Die Methode **mutiert** vorhandene Findings, statt neue anzulegen. Der
/// isolierte Vornamen-Treffer wird später durch [`dedupe_and_sort`] entfernt,
/// weil er vollständig im erweiterten Compound-Treffer liegt.
fn expand_person_findings(text: &str, findings: &mut [Finding], secret: &[u8]) {
    /// „ein Whitespace gefolgt von einem Großbuchstaben-Wort"
    static EXT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s+\p{Lu}\p{Ll}+").unwrap());

    // Indices der Personen-Findings sammeln, damit wir die Liste nicht
    // während des Iterierens mutieren.
    let person_indices: Vec<usize> = findings
        .iter()
        .enumerate()
        .filter(|(_, f)| f.entity_type == "person")
        .map(|(i, _)| i)
        .collect();

    for idx in person_indices {
        let end = findings[idx].end;
        let after = &text[end..];
        let Some(m) = EXT_RE.find(after) else { continue };

        let new_end = end + m.end();
        let start = findings[idx].start;
        // Inklusive verbindendem Whitespace kopieren, damit Reverse-Mapping
        // den exakten Substring (mit Original-Spacing) wieder einsetzen kann.
        let new_original = text[start..new_end].to_string();
        let new_token = tokens::make_token(EntityType::Person.short_code(), &new_original, secret);
        findings[idx].original = new_original;
        findings[idx].token = new_token;
        findings[idx].end = new_end;
    }
}

// =================================================================== Helpers

/// Dedupliziert überlappende Findings.
///
/// Algorithmus:
/// 1. Sortiere nach `(start asc, end desc)` — bei gleichem Start kommt der
///    längere Treffer zuerst.
/// 2. Iteriere durch — wenn der nächste Treffer **innerhalb** des bisher
///    weitest reichenden Bereichs beginnt, drop ihn.
///
/// Damit gewinnt im Konflikt immer der längere Treffer (z. B.
/// „Max Mustermann" schlägt das isolierte „Max").
fn dedupe_and_sort(findings: &mut Vec<Finding>) {
    findings.sort_by_key(|f| (f.start, std::cmp::Reverse(f.end)));
    let mut last_end = 0;
    findings.retain(|f| {
        if f.start < last_end {
            false
        } else {
            last_end = f.end;
            true
        }
    });
}

/// Luhn-Prüfsumme für Kreditkartennummern.
///
/// Jede zweite Ziffer von rechts (0-basiert: Index 1, 3, 5, …) wird verdoppelt;
/// Werte > 9 werden über ihre Quersumme zurückgeführt (`d*2 - 9`). Die Summe
/// aller transformierten Ziffern muss durch 10 teilbar sein.
fn luhn_check(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in digits.chars().rev().enumerate() {
        let Some(d) = c.to_digit(10) else { return false };
        let n = if i % 2 == 1 {
            let doubled = d * 2;
            if doubled > 9 { doubled - 9 } else { doubled }
        } else {
            d
        };
        sum += n;
    }
    sum.is_multiple_of(10)
}

/// Prüfsumme der deutschen Steuer-ID (BMF-Spezifikation).
///
/// Drei Bedingungen:
/// 1. Die **erste Ziffer darf nicht 0 sein** (formale Vorgabe der IdNr).
/// 2. Genau **eine** der ersten 10 Ziffern kommt 2- oder 3-mal vor; alle
///    anderen 0- oder 1-mal.
/// 3. Iteratives ISO-7064-MOD-11,10-Verfahren über die ersten 10 Ziffern
///    liefert die elfte (Prüf-)Ziffer.
fn steuer_id_check(id: &str) -> bool {
    if id.len() != 11 || !id.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }

    // Formale Vorgabe: die IdNr beginnt nie mit 0.
    if id.starts_with('0') {
        return false;
    }

    // Strukturbedingung: genau eine Ziffer kommt 2-/3-mal in den ersten 10 Stellen vor.
    let mut counts = [0u8; 10];
    for c in id[..10].chars() {
        counts[c.to_digit(10).unwrap() as usize] += 1;
    }
    let twos_or_threes = counts.iter().filter(|&&c| c == 2 || c == 3).count();
    if twos_or_threes != 1 {
        return false;
    }

    // ISO 7064 MOD 11,10 — Prüfziffer iterativ berechnen.
    let mut product = 10u32;
    for c in id[..10].chars() {
        let d = c.to_digit(10).unwrap();
        let mut sum_mod = (d + product) % 10;
        if sum_mod == 0 {
            sum_mod = 10;
        }
        product = (sum_mod * 2) % 11;
    }
    let check = (11 - product) % 10;
    let last = id.chars().nth(10).unwrap().to_digit(10).unwrap();
    check == last
}

// =================================================================== Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_email() {
        let f = detect("Bitte an max.mustermann@example.de melden.");
        assert!(f.iter().any(|x| x.entity_type == "email"));
    }

    #[test]
    fn detects_iban() {
        let f = detect("Meine IBAN ist DE89370400440532013000.");
        assert!(f.iter().any(|x| x.entity_type == "iban"));
    }

    #[test]
    fn rejects_invalid_iban() {
        let f = detect("Falsche IBAN: DE00000000000000000000.");
        assert!(!f.iter().any(|x| x.entity_type == "iban"));
    }

    #[test]
    fn detects_credit_card_with_luhn() {
        // Visa-Test-Nummer mit gültigem Luhn
        let f = detect("Karte: 4532015112830366");
        assert!(f.iter().any(|x| x.entity_type == "credit_card"));
    }

    #[test]
    fn rejects_invalid_credit_card() {
        // Gleiche Länge, ungültiger Luhn
        let f = detect("Karte: 4532015112830367");
        assert!(!f.iter().any(|x| x.entity_type == "credit_card"));
    }

    #[test]
    fn detects_german_phone() {
        let f = detect("Rufen Sie an unter 089-12345678 oder +49 30 87654321.");
        assert!(
            f.iter().filter(|x| x.entity_type == "phone").count() >= 1,
            "expected at least 1 phone, got: {:?}",
            f
        );
    }

    #[test]
    fn rejects_hex_literal_as_phone() {
        // Crash-Report-Schnipsel (Issue #22): `0x…` ist keine Telefonnummer.
        let f = detect("Exception at address 0x0000000000000001 in module foo.dll");
        assert!(!f.iter().any(|x| x.entity_type == "phone"), "got: {:?}", f);
    }

    #[test]
    fn rejects_uppercase_hex_literal_as_phone() {
        let f = detect("Register RAX=0X0000000000123456");
        assert!(!f.iter().any(|x| x.entity_type == "phone"), "got: {:?}", f);
    }

    #[test]
    fn rejects_digit_run_inside_hex_literal_as_phone() {
        // Ziffernfolge mitten im Hex-Literal, durch Hex-Buchstaben vom `0x` getrennt.
        let f = detect("Pointer 0xdead0123456789 dereferenziert");
        assert!(!f.iter().any(|x| x.entity_type == "phone"), "got: {:?}", f);
    }

    #[test]
    fn rejects_git_sha_as_phone() {
        // Hash ohne `0x`-Präfix: Ziffernfolge geht nahtlos in Hex-Buchstaben über.
        let f = detect("Gefixt in Commit 0891234567deadbeef003a4b5c6d7e8f90112233.");
        assert!(!f.iter().any(|x| x.entity_type == "phone"), "got: {:?}", f);
    }

    #[test]
    fn detects_phone_next_to_hex_literal() {
        // Der Hex-Guard darf echte Nummern im selben Text nicht mitreißen.
        let f = detect("Fehler 0x0000000000000001 aufgetreten — Support: 089/12345678.");
        let phones: Vec<_> = f.iter().filter(|x| x.entity_type == "phone").collect();
        assert_eq!(phones.len(), 1, "got: {:?}", f);
        assert!(phones[0].original.starts_with("089"));
    }

    #[test]
    fn rejects_hex_literal_as_steuer_id() {
        // 11 Ziffern im Hex-Literal, sogar mit Kontextwort davor: `\b` im
        // Kandidaten-Pattern verhindert den Match nach `x` — festgenagelt,
        // damit eine künftige Pattern-Änderung das nicht still aufweicht.
        let f = detect("IdNr im Speicherdump: 0x12345678901");
        assert!(!f.iter().any(|x| x.entity_type == "steuer_id"), "got: {:?}", f);
    }

    #[test]
    fn salutation_skips_academic_titles() {
        // „Herr Dr. Demary": vorher captured die Regex „Dr" als Namen und
        // ließ den echten Nachnamen stehen (Beta-Befund). Titel zwischen
        // Anrede und Name dürfen nicht die Capture sein.
        let f = detect("Lieber Herr Dr. Demary, willkommen.");
        let persons: Vec<_> = f.iter().filter(|x| x.entity_type == "person").collect();
        assert!(
            persons.iter().any(|p| p.original == "Demary"),
            "Nachname muss erkannt werden, got: {persons:?}"
        );
        assert!(
            !persons.iter().any(|p| p.original == "Dr"),
            "der Titel Dr darf kein Person-Finding sein, got: {persons:?}"
        );

        // Auch gestapelte Titel.
        let f = detect("Sehr geehrter Herr Prof. Dr. Obst,");
        assert!(
            f.iter().any(|x| x.entity_type == "person" && x.original == "Obst"),
            "got: {f:?}"
        );

        // Titel als eigene Anrede funktioniert weiter.
        let f = detect("An: Dr. Volker Demary");
        assert!(
            f.iter()
                .any(|x| x.entity_type == "person" && x.original == "Volker Demary"),
            "got: {f:?}"
        );
    }

    #[test]
    fn detects_date() {
        let f = detect("Geburtsdatum: 15.03.1985");
        assert!(f.iter().any(|x| x.entity_type == "date"));
    }

    #[test]
    fn luhn_known_good() {
        assert!(luhn_check("4532015112830366"));
        assert!(luhn_check("79927398713"));
    }

    #[test]
    fn luhn_known_bad() {
        assert!(!luhn_check("4532015112830367"));
        assert!(!luhn_check("1234567890"));
    }

    #[test]
    fn tokens_are_consistent_per_secret() {
        let text = "Max Mustermann hat IBAN DE89370400440532013000.";
        let f1 = detect(text);
        let f2 = detect(text);
        let iban1 = f1.iter().find(|x| x.entity_type == "iban").unwrap();
        let iban2 = f2.iter().find(|x| x.entity_type == "iban").unwrap();
        assert_eq!(iban1.token, iban2.token);
    }

    #[test]
    fn detects_persons_via_gazetteer() {
        let f = detect("Sehr geehrter Herr Müller, bitte kontaktieren Sie Max Mustermann.");
        let persons: Vec<_> = f.iter().filter(|x| x.entity_type == "person").collect();
        assert!(
            persons.iter().any(|p| p.original == "Müller"),
            "expected Müller in {:?}",
            persons
        );
        // Compound-Expansion: „Max Mustermann" wird als ein Treffer erkannt.
        assert!(
            persons.iter().any(|p| p.original == "Max Mustermann"),
            "expected compound 'Max Mustermann' in {:?}",
            persons
        );
    }

    #[test]
    fn detects_foreign_name_via_salutation() {
        let f = detect("Sehr geehrter Herr Kowalski, ihre Anfrage…");
        let persons: Vec<_> = f.iter().filter(|x| x.entity_type == "person").collect();
        assert!(
            persons.iter().any(|p| p.original == "Kowalski"),
            "expected Kowalski via salutation in {:?}",
            persons
        );
    }

    #[test]
    fn compound_name_without_salutation() {
        let f = detect("Bitte cc'en Sie Jan Kowalski in der Mail.");
        let persons: Vec<_> = f.iter().filter(|x| x.entity_type == "person").collect();
        assert!(
            persons.iter().any(|p| p.original == "Jan Kowalski"),
            "expected 'Jan Kowalski' as single compound finding in {:?}",
            persons
        );
        // Sollte nicht zusätzlich „Jan" allein erscheinen.
        assert_eq!(
            persons.iter().filter(|p| p.original == "Jan").count(),
            0,
            "single 'Jan' should be deduped, got {:?}",
            persons
        );
    }

    #[test]
    fn salutation_captures_two_word_name() {
        let f = detect("Hallo Dr. Anna Kowalski, danke für Ihre Mail.");
        let persons: Vec<_> = f.iter().filter(|x| x.entity_type == "person").collect();
        assert!(
            persons.iter().any(|p| p.original == "Anna Kowalski"),
            "expected 'Anna Kowalski' as one finding in {:?}",
            persons
        );
    }

    #[test]
    fn person_tokens_use_p_prefix() {
        let f = detect("Frau Schmidt grüßt.");
        let schmidt = f.iter().find(|x| x.original == "Schmidt").expect("Schmidt missing");
        assert_eq!(schmidt.entity_type, "person");
        assert!(schmidt.token.starts_with("«P_"), "token was {}", schmidt.token);
    }

    #[test]
    fn detects_city_via_gazetteer() {
        let f = detect("Treffen in Berlin am Donnerstag.");
        assert!(
            f.iter().any(|x| x.entity_type == "location" && x.original == "Berlin"),
            "expected Berlin in {:?}",
            f
        );
    }

    #[test]
    fn detects_org_via_suffix() {
        let f = detect("Bitte überweisen an die Acme GmbH oder die Deutsche Bahn AG.");
        let orgs: Vec<_> = f.iter().filter(|x| x.entity_type == "organization").collect();
        assert!(
            orgs.iter().any(|o| o.original.contains("GmbH")),
            "expected GmbH org in {:?}",
            orgs
        );
        assert!(
            orgs.iter().any(|o| o.original.contains("AG")),
            "expected AG org in {:?}",
            orgs
        );
    }

    #[test]
    fn detects_steuer_id_with_context() {
        // Gültige Test-IdNr (korrekte Prüfziffer + Struktur) mit Kontextwort.
        let f = detect("Meine Steuer-ID: 86095742719, bitte notieren.");
        assert!(
            f.iter().any(|x| x.entity_type == "steuer_id" && x.original == "86095742719"),
            "expected Steuer-ID 86095742719 in {f:?}"
        );
    }

    #[test]
    fn steuer_id_without_context_is_ignored() {
        // Dieselbe prüfsummen-gültige Zahl OHNE Kontextwort — eine harmlose
        // Bestellnummer. Darf NICHT als Steuer-ID pseudonymisiert werden.
        let f = detect("Bestellnummer 86095742719 wurde versandt.");
        assert!(
            !f.iter().any(|x| x.entity_type == "steuer_id"),
            "harmless 11-digit order number must not be flagged as Steuer-ID: {f:?}"
        );
    }

    #[test]
    fn steuer_id_context_variants() {
        // Verschiedene Kontext-Schreibweisen greifen.
        for ctx in ["IdNr. 86095742719", "Identifikationsnummer 86095742719", "steuerid 86095742719"] {
            let f = detect(ctx);
            assert!(
                f.iter().any(|x| x.entity_type == "steuer_id"),
                "expected Steuer-ID for context {ctx:?}, got {f:?}"
            );
        }
    }

    #[test]
    fn steuer_id_check_validates_structure_and_checksum() {
        // Gültig: korrekte Prüfziffer, erste Ziffer ≠ 0, genau eine Ziffer doppelt.
        assert!(steuer_id_check("86095742719"));
        // Falsche Prüfziffer.
        assert!(!steuer_id_check("86095742718"));
        // Führende 0 ist formal unzulässig.
        assert!(!steuer_id_check("06095742719"));
        // Falsche Länge.
        assert!(!steuer_id_check("8609574271"));
    }

    #[test]
    fn steuer_id_with_leading_zero_rejected_even_with_context() {
        // Formale Regel schlägt zu, selbst wenn ein Kontextwort danebensteht.
        let f = detect("Steuer-ID 01234567890 (Tippfehler)");
        assert!(
            !f.iter().any(|x| x.entity_type == "steuer_id"),
            "IdNr with leading zero must be rejected: {f:?}"
        );
    }

    #[test]
    fn detects_vat_id() {
        let f = detect("USt.Id.Nr. DE124652081 ist die Nummer.");
        assert!(
            f.iter().any(|x| x.entity_type == "steuer_id" && x.original == "DE124652081"),
            "expected DE124652081 in {f:?}"
        );
    }

    #[test]
    fn detects_german_bic() {
        let f = detect("BIC: DORTDE33 für die Sparkasse.");
        assert!(
            f.iter().any(|x| x.entity_type == "iban" && x.original == "DORTDE33"),
            "expected DORTDE33 BIC in {f:?}"
        );
    }

    #[test]
    fn detects_postal_code() {
        let f = detect("Anschrift: Freistuhl 2, 44137 Dortmund.");
        assert!(
            f.iter().any(|x| x.entity_type == "location" && x.original == "44137"),
            "expected PLZ 44137 in {f:?}"
        );
    }

    #[test]
    fn detects_street_with_number() {
        let f = detect("Marienstraße 14 in Berlin.");
        assert!(
            f.iter().any(|x| x.entity_type == "location" && x.original.starts_with("Marienstraße")),
            "expected Marienstraße in {f:?}"
        );
    }

    #[test]
    fn detects_compound_street_with_capital_abbrev() {
        // „Bürgermeister-Heidenreich-Str. 5" — Bindestriche, interior
        // Großbuchstaben, Suffix mit großem S.
        let f = detect("Anschrift: Bürgermeister-Heidenreich-Str. 5, 26316 Varel.");
        assert!(
            f.iter().any(|x| x.entity_type == "location"
                && x.original.starts_with("Bürgermeister-Heidenreich-Str")),
            "expected compound street in {f:?}"
        );
    }

    #[test]
    fn detects_phone_with_slash_and_spaces() {
        // „04451 / 15 234" — DE-Format mit Slash und gruppierter Hauptnummer.
        let f = detect("Telefon: 04451 / 15 234 erreichbar.");
        assert!(
            f.iter().any(|x| x.entity_type == "phone" && x.original.contains("04451")),
            "expected phone with slash in {f:?}"
        );
    }

    #[test]
    fn detects_url_https_and_www() {
        let f = detect("Mehr unter https://npl-forum.com oder www.volksbank-jade-weser.de gefunden.");
        let urls: Vec<_> = f.iter().filter(|x| x.entity_type == "url").collect();
        assert!(
            urls.iter().any(|u| u.original == "https://npl-forum.com"),
            "expected https URL in {urls:?}"
        );
        assert!(
            urls.iter().any(|u| u.original == "www.volksbank-jade-weser.de"),
            "expected www URL in {urls:?}"
        );
    }

    #[test]
    fn detects_silke_via_extended_gazetteer() {
        let f = detect("Mit freundlichen Grüßen\n\nSilke Bohnenkamp\nVorstandsstab");
        let persons: Vec<_> = f.iter().filter(|x| x.entity_type == "person").collect();
        assert!(
            persons.iter().any(|p| p.original == "Silke Bohnenkamp"),
            "expected compound 'Silke Bohnenkamp' in {persons:?}"
        );
    }

    #[test]
    fn detects_varel_as_city() {
        let f = detect("Sitz: 26316 Varel.");
        assert!(
            f.iter().any(|x| x.entity_type == "location" && x.original == "Varel"),
            "expected Varel as city in {f:?}"
        );
    }

    #[test]
    fn detects_outlook_name_format() {
        // „Kroll, Gabriele" — Outlook-Adressbuch-Konvention.
        let f = detect("An: Kroll, Gabriele <gabriele.kroll@example.de>");
        let persons: Vec<_> = f.iter().filter(|x| x.entity_type == "person").collect();
        assert!(
            persons.iter().any(|p| p.original == "Kroll"),
            "expected Kroll as person in {persons:?}"
        );
        assert!(
            persons.iter().any(|p| p.original == "Gabriele"),
            "expected Gabriele as person in {persons:?}"
        );
    }

    #[test]
    fn strict_mode_assigns_readable_placeholders() {
        let text = "Müller traf Schmidt. Müller war zuvor in Berlin.";
        let findings = detect_strict(text);
        // Mindestens zwei Personen + eine Location erkannt.
        let persons: Vec<&Finding> = findings.iter().filter(|f| f.entity_type == "person").collect();
        let locs: Vec<&Finding> = findings.iter().filter(|f| f.entity_type == "location").collect();
        assert!(!persons.is_empty(), "expected person findings: {findings:?}");
        assert!(!locs.is_empty(), "expected location findings: {findings:?}");

        // Müller (zweimal) muss DENSELBEN Platzhalter haben — sonst geht
        // dem LLM die „dieselbe Entität"-Information verloren.
        let mueller_tokens: Vec<&String> = persons
            .iter()
            .filter(|f| f.original == "Müller")
            .map(|f| &f.token)
            .collect();
        assert!(mueller_tokens.len() >= 2, "expected two Müller findings");
        assert_eq!(
            mueller_tokens[0], mueller_tokens[1],
            "Müller-Wiederholung muss identisch tokenisiert sein"
        );

        // Formatprüfung: Klartext-Platzhalter mit Guillemets.
        for f in &findings {
            assert!(
                f.token.starts_with("«") && f.token.ends_with("»"),
                "strict token muss in « » stehen: {}",
                f.token
            );
            assert!(
                !f.token.contains("_"),
                "strict token darf KEIN underscore haben (Hash-Format): {}",
                f.token
            );
        }
    }

    #[test]
    fn strict_mode_letter_sequence_handles_overflow() {
        // Mehr als 26 Personen → AA, AB, … funktioniert.
        assert_eq!(letter_for_index(1), "A");
        assert_eq!(letter_for_index(26), "Z");
        assert_eq!(letter_for_index(27), "AA");
        assert_eq!(letter_for_index(28), "AB");
        assert_eq!(letter_for_index(52), "AZ");
        assert_eq!(letter_for_index(53), "BA");
    }

    #[test]
    fn strict_mode_hint_does_not_mention_reverse() {
        let findings = detect_strict("Müller war hier.");
        let out = apply_strict_with_hint("Müller war hier.", &findings);
        assert!(
            !out.contains("automatisch zurück übersetzt"),
            "Strict-Hint darf KEIN Reverse-Versprechen enthalten"
        );
        assert!(
            out.contains("«Person A»"),
            "expected Klartext-Platzhalter, got: {out}"
        );
    }

    #[test]
    fn detects_juergen_via_extended_gazetteer() {
        // Jürgen mit Umlaut, plus Compound-Expansion zum Nachnamen.
        let f = detect("Mit besten Grüßen\n\nJürgen Sonder\nPräsident");
        let persons: Vec<_> = f.iter().filter(|x| x.entity_type == "person").collect();
        assert!(
            persons.iter().any(|p| p.original == "Jürgen Sonder"),
            "expected compound 'Jürgen Sonder' in {persons:?}"
        );
    }

    #[test]
    fn detects_ev_organization() {
        let f = detect("Verein: Mustermann e.V. spendet.");
        assert!(
            f.iter().any(|x| x.entity_type == "organization" && x.original.contains("e.V.")),
            "expected e.V. org in {:?}",
            f
        );
    }

    #[test]
    fn hint_appended_when_findings_present() {
        let text = "Mail: foo@bar.de";
        let findings = detect(text);
        let result = apply_tokens_with_hint(text, &findings, "test");
        assert!(result.contains("«E_"));
        assert!(result.contains("Hinweis"));
        assert!(result.contains("Pseudonyme"));
    }

    #[test]
    fn no_hint_when_no_findings_present() {
        let text = "Plain text without any PII.";
        let findings = detect(text);
        let result = apply_tokens_with_hint(text, &findings, "test");
        assert_eq!(result, text, "hint should not be appended when nothing was redacted");
    }

    // ===================================================== Bindestrich-Namen + URL-Trailing-Fixes
    //
    // Code-Review-Fixes für: Bindestrich-Doppelnamen (Müller-Lüdenscheidt,
    // Anna-Maria), Bindestrich-Städte (Baden-Baden), URL-Pfad-Ende auf
    // Satzzeichen.

    #[test]
    fn salutation_captures_hyphenated_lastname() {
        let f = detect("Bitte fragen Sie Herr Müller-Lüdenscheidt nochmal.");
        let persons: Vec<_> = f.iter().filter(|x| x.entity_type == "person").collect();
        assert!(
            persons.iter().any(|p| p.original == "Müller-Lüdenscheidt"),
            "expected hyphenated lastname captured as one finding, got: {:?}",
            persons
        );
    }

    #[test]
    fn salutation_captures_hyphenated_firstname() {
        let f = detect("Dr. Anna-Maria Schmidt war gestern hier.");
        let persons: Vec<_> = f.iter().filter(|x| x.entity_type == "person").collect();
        assert!(
            persons.iter().any(|p| p.original == "Anna-Maria Schmidt"),
            "expected hyphenated firstname captured with lastname, got: {:?}",
            persons
        );
    }

    #[test]
    fn city_candidate_keeps_hyphenated_compound() {
        // "Baden-Baden" muss als ein Wort an den Gazetteer gehen — sonst
        // findet der Lookup nur "Baden" und das ist nicht im Set.
        let f = detect("Wir treffen uns in Baden-Baden am Wochenende.");
        assert!(
            f.iter().any(|x| x.entity_type == "location" && x.original == "Baden-Baden"),
            "expected Baden-Baden as one location finding, got: {:?}",
            f
        );
    }

    #[test]
    fn url_does_not_capture_trailing_punctuation_in_path() {
        // Pfad-Trailing-Satzzeichen ist der echte Bug-Case (greedy
        // [^\s<>()]* hätte den Punkt im Pfad inkludiert).
        let f = detect("Mehr Infos unter https://example.de/seite. Danke.");
        let url = f.iter().find(|x| x.entity_type == "url").expect("expected URL finding");
        assert_eq!(
            url.original, "https://example.de/seite",
            "URL must not include trailing sentence period"
        );
    }

    #[test]
    fn url_with_root_slash_is_preserved() {
        // Root-URL mit `/` allein muss weiter funktionieren — der neue Regex
        // erlaubt einen Pfad bestehend nur aus dem Slash.
        let f = detect("Site: https://example.de/ — alles OK.");
        let url = f.iter().find(|x| x.entity_type == "url").expect("expected URL");
        assert!(
            url.original == "https://example.de/" || url.original == "https://example.de",
            "expected URL with optional trailing slash, got: {}",
            url.original
        );
    }

    #[test]
    fn url_without_path_unaffected() {
        // www.foo.de am Satzende: der trailing Punkt war hier schon korrekt
        // außerhalb des Matches. Test sichert die Regression ab.
        let f = detect("Gehen Sie auf www.beispiel.de.");
        let url = f.iter().find(|x| x.entity_type == "url").expect("expected URL");
        assert_eq!(url.original, "www.beispiel.de");
    }

    #[test]
    fn apply_tokens_replaces_findings() {
        let text = "Mail: foo@bar.de, IBAN DE89370400440532013000.";
        let findings = detect(text);
        let pseud = apply_tokens(text, &findings, "test-case");
        assert!(!pseud.contains("foo@bar.de"));
        assert!(!pseud.contains("DE89370400440532013000"));
        assert!(pseud.contains("«E_"));
        assert!(pseud.contains("«B_"));
    }
}
