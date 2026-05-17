//! Layer-2 Personen-Erkennung über statische Namenslisten.
//!
//! Diese Datei enthält zwei kuratierte Listen — häufige deutsche Vor- und
//! Nachnamen — und einen O(1)-Lookup über ein vorinitialisiertes `HashSet`.
//!
//! # Designentscheidungen
//!
//! - **Statisch im Binary**, kein Datei-Loading: simpel, kein I/O-Fehlerpfad,
//!   kein Modell-File auf der Platte. Listen wachsen die Binary nur um ein paar
//!   Kilobytes.
//! - **Lowercase-Vergleich**: der Aufrufer (`collect_persons`) sendet das
//!   Kandidatenwort kleingeschrieben rein. Spart Case-Mapping auf jeder
//!   Lookup-Seite.
//! - **Mehrdeutige Namen ausgelassen** (Klein/Lang/Roth/Braun/Schwarz): diese
//!   sind in Office-Texten gleichzeitig häufige Adjektive. Das Sicherheits-Plus
//!   durch das Auslassen überwiegt die fehlenden Matches; sie würden sonst zu
//!   nervigen False Positives in Code- und Diskussions-Texten führen.
//! - **`Mustermann`/`Musterfrau`** sind statistisch keine Top-Namen, aber als
//!   kanonische Beispieldaten in Office-Kontexten omnipräsent.
//!
//! # Quellen
//!
//! Top-Listen aus Destatis und allgemeine deutsche Namensverteilung. Bei
//! Erweiterung gilt: Vornamen lieber großzügig, Nachnamen vorsichtig (höheres
//! False-Positive-Risiko, weil Nachnamen oft Substantive/Adjektive sein können).

use once_cell::sync::Lazy;
use std::collections::HashSet;

/// Häufige deutsche und international gängige Vornamen.
const FIRST_NAMES: &[&str] = &[
    // männlich klassisch
    "max", "maximilian", "alexander", "paul", "leon", "lukas", "luca", "felix",
    "jonas", "elias", "noah", "finn", "ben", "niklas", "tim", "tom", "moritz",
    "david", "julian", "simon", "linus", "jan", "sebastian", "daniel", "christian",
    "thomas", "michael", "andreas", "stefan", "markus", "martin", "peter", "klaus",
    "hans", "wolfgang", "helmut", "gerhard", "heinz", "frank", "uwe", "jörg",
    "matthias", "florian", "kevin", "marco", "patrick", "philipp", "tobias", "oliver",
    "manuel", "fabian", "dominik", "sven", "achim", "rolf", "dieter", "joachim",
    "günter", "horst", "günther", "rainer", "norbert", "bernd",
    // männlich Umlaut / weniger häufig im Gazetteer
    "jürgen", "björn", "jörn", "rüdiger", "sönke", "thorsten", "torsten",
    "harald", "udo", "volker", "axel", "armin", "lars", "carsten", "karsten",
    "holger", "ralf", "ralph", "wilfried", "detlef", "gerd", "werner",
    "siegfried", "ekkehard", "manfred", "hartmut", "reinhold", "konrad", "kurt",
    "andre", "andré", "waldemar", "dirk", "ingo", "olaf", "uli", "ulrich",
    "kai", "knut", "henning", "hendrik", "jens", "ole", "ove", "broder",
    "edgar", "egon", "erwin", "willi", "willy", "fritz", "alfred",
    "lothar", "günther", "günter", "siegmund", "heinrich", "friedhelm",
    // weiblich klassisch
    "marie", "sophie", "sophia", "anna", "lisa", "lena", "hannah", "mia", "emma",
    "lara", "laura", "sarah", "julia", "katharina", "jana", "nina", "christina",
    "petra", "sabine", "andrea", "birgit", "monika", "ute", "susanne", "claudia",
    "renate", "elisabeth", "helga", "brigitte", "christine", "gisela", "ingrid",
    "karin", "ursula", "gabriele", "barbara", "angelika", "heike", "michaela",
    "stefanie", "kerstin", "doris", "silvia", "tanja", "melanie", "katrin", "nadine",
    "jessica", "vanessa", "lina", "leonie", "amelie", "clara", "luise", "luisa",
    "carla", "merle", "frieda", "greta", "ida", "johanna",
    // weiblich Umlaut / weniger häufig
    "marlies", "sigrid", "ilse", "edith", "hildegard", "marianne", "waltraud",
    "inge", "elke", "elfriede", "annegret", "annemarie", "rosemarie",
    "silke", "silvia", "antje", "anke", "kirsten", "kirstin", "wiebke",
    "ingeborg", "irmgard", "gerda", "hilde", "rita", "ute", "uta", "ulla",
    "marion", "martina", "manuela", "beate", "bettina", "dagmar", "elvira",
    "evelyn", "hannelore", "ilona", "jutta", "marlene",
];

/// Häufige deutsche Nachnamen. Bewusst ohne mehrdeutige Wörter wie
/// „Klein", „Lang", „Roth", „Braun", „Schwarz", „Weiß" — die wären zwar
/// statistisch häufige Nachnamen, aber in Office-Text fast immer Adjektive
/// (siehe Module-Docstring).
const LAST_NAMES: &[&str] = &[
    "müller", "schmidt", "schneider", "fischer", "weber", "meyer", "wagner",
    "becker", "schulz", "hoffmann", "schäfer", "koch", "bauer", "richter",
    "wolf", "schröder", "neumann", "zimmermann", "krüger", "hofmann", "hartmann",
    "schmitt", "schmitz", "krause", "meier", "lehmann", "schmid", "schulze",
    "maier", "köhler", "herrmann", "könig", "mayer", "huber", "kaiser", "fuchs",
    "peters", "scholz", "möller", "hahn", "schubert", "günther", "winkler",
    "berger", "wolff", "stein", "kraus", "jäger", "winter", "engel", "vogel",
    "friedrich", "keller", "ziegler", "wagner", "frank", "albrecht", "sommer",
    "graf", "seidel", "heinrich", "böhm", "thomas", "stahl", "mertens",
    "thomsen", "petersen", "hansen", "nielsen", "jensen", "böhme",
    // Kanonische Platzhalter
    "mustermann", "musterfrau",
];

/// Größte deutsche Städte. Mehrdeutige Wörter („Essen", „Halle", „Hof",
/// „Bad …") sind bewusst weggelassen, um False Positives bei normalem
/// Office-Text zu vermeiden.
const CITIES: &[&str] = &[
    "berlin", "münchen", "hamburg", "köln", "frankfurt", "stuttgart",
    "düsseldorf", "leipzig", "dortmund", "bremen", "dresden", "hannover",
    "nürnberg", "duisburg", "bochum", "wuppertal", "bielefeld", "bonn",
    "münster", "karlsruhe", "mannheim", "augsburg", "wiesbaden",
    "mönchengladbach", "gelsenkirchen", "braunschweig", "chemnitz", "kiel",
    "aachen", "magdeburg", "freiburg", "krefeld", "mainz", "lübeck",
    "erfurt", "oberhausen", "rostock", "kassel", "potsdam", "saarbrücken",
    "ludwigshafen", "osnabrück", "solingen", "leverkusen", "heidelberg",
    "darmstadt", "regensburg", "ingolstadt", "ulm", "würzburg", "fürth",
    "wolfsburg", "offenbach", "pforzheim", "göttingen", "bottrop",
    "trier", "reutlingen", "bremerhaven", "koblenz", "jena", "siegen",
    "salzgitter", "cottbus",
    // Mittelgroße/kleinere DE-Städte, die in Office-Korrespondenz häufig
    // auftauchen (Banken-/Behördensitze). Mehrdeutige Wörter („Bad …",
    // „Hof", „Essen") bleiben weiterhin draußen.
    "varel", "oldenburg", "donauwörth", "ratingen", "esslingen", "tübingen",
    "kempten", "konstanz", "lüneburg", "flensburg", "stralsund", "schwerin",
    "passau", "bayreuth", "memmingen", "ravensburg", "celle", "hildesheim",
    "wilhelmshaven", "emden", "delmenhorst", "minden", "paderborn", "hamm",
    "iserlohn", "recklinghausen", "moers", "marl", "neuss", "remscheid",
    "viersen", "düren", "herford", "marburg", "gießen", "fulda", "hanau",
    "wetzlar", "limburg", "speyer", "worms", "neustadt", "kaiserslautern",
    "saarlouis", "neunkirchen", "merseburg", "dessau", "weimar",
    "gera", "plauen", "zwickau", "görlitz", "freiberg",
    // Österreich + Schweiz: Major
    "wien", "graz", "linz", "salzburg", "innsbruck", "klagenfurt",
    "zürich", "genf", "basel", "lausanne", "bern",
    // Bindestrich-Städte. RE_CITY_CANDIDATE matcht jetzt
    // Bindestrich-Komposita als zusammenhängendes Wort — kuratierte
    // Bindestrich-Städte können hier ergänzt werden.
    "baden-baden", "castrop-rauxel", "müllheim-britzingen",
    "halle-saale", "frankfurt-oder",
];

/// Kombiniertes HashSet aller Personennamen (Vor + Nach) für O(1)-Lookup.
static NAME_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    let mut s = HashSet::with_capacity(FIRST_NAMES.len() + LAST_NAMES.len());
    s.extend(FIRST_NAMES.iter().copied());
    s.extend(LAST_NAMES.iter().copied());
    s
});

/// Städte-HashSet, getrennt vom Namens-Set, damit der Aufrufer eindeutig
/// Person vs. Ort unterscheiden kann.
static CITY_SET: Lazy<HashSet<&'static str>> = Lazy::new(|| CITIES.iter().copied().collect());

/// Prüft, ob `lower` ein bekannter Vor- oder Nachname ist.
///
/// **Wichtig:** `lower` muss bereits klein geschrieben sein — der Aufrufer
/// ist dafür verantwortlich. Das spart pro Aufruf ein `to_lowercase()`-Roundtrip
/// im Hot-Path.
pub fn is_known_name(lower: &str) -> bool {
    NAME_SET.contains(lower)
}

/// Prüft, ob `lower` eine bekannte (deutschsprachige) Stadt ist.
/// Lowercase-Convention wie bei [`is_known_name`].
pub fn is_known_city(lower: &str) -> bool {
    CITY_SET.contains(lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_common_last_names() {
        assert!(is_known_name("müller"));
        assert!(is_known_name("schmidt"));
        assert!(is_known_name("mustermann"));
    }

    #[test]
    fn knows_common_first_names() {
        assert!(is_known_name("max"));
        assert!(is_known_name("marie"));
        assert!(is_known_name("anna"));
    }

    #[test]
    fn rejects_unknown() {
        assert!(!is_known_name("xyz123"));
        assert!(!is_known_name("anonymisierung"));
    }

    #[test]
    fn knows_major_cities() {
        assert!(is_known_city("berlin"));
        assert!(is_known_city("münchen"));
        assert!(is_known_city("wien"));
    }

    #[test]
    fn rejects_unknown_city() {
        assert!(!is_known_city("essen")); // mehrdeutig, bewusst raus
        assert!(!is_known_city("xyz"));
    }
}
