//! User-konfigurierbare Settings, persistiert als JSON in `$DATA_DIR`.
//!
//! Wird beim App-Start einmal geladen. Änderungen über die Tray-UI oder die
//! Frontend-Settings-Page rufen [`Settings::save`]. Aktuell erfordern viele
//! Toggles (z. B. `auto_detection`) einen App-Neustart, weil sie zur
//! `setup()`-Zeit konsumiert werden — eine Live-Toggle-Architektur würde
//! die Komplexität ohne MVP-Wert erhöhen.
//!
//! # Pfad
//!
//! - Windows: `%APPDATA%\Roaming\de.streichzeug.app\settings.json`
//! - macOS:   `~/Library/Application Support/de.streichzeug.app/settings.json`
//!
//! # Schema-Versionierung
//!
//! Aktuell keine. Wenn das Schema in Zukunft inkompatibel ändert, sollte
//! ein `version`-Feld dazu — beim Lesen prüfen, ggf. migrieren.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const APP_DIR: &str = "de.streichzeug.app";
const SETTINGS_FILENAME: &str = "settings.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Hotkey für „Smart-Paste". Format: Tauri-Accelerator-String, z. B.
    /// `"CmdOrCtrl+B"` für Strg+B (Win) / Cmd+B (Mac). Auflösbar zur Laufzeit
    /// via `Shortcut::from_str`.
    pub hotkey: String,

    /// Wenn `true`: Foreground-aware Clipboard-Watcher läuft im Hintergrund
    /// und ersetzt automatisch. **Default: false** —
    /// der Hotkey ist die primäre UX. Auto-Detection ist als Power-User-
    /// Option erhalten.
    pub auto_detection: bool,

    /// Layer-3 NER aktivieren — lokales ONNX-Modell für statistische
    /// Erkennung von Personen/Orten/Organisationen, die L1/L2 verpassen.
    ///
    /// Wirkt nur, wenn die Binary mit Feature-Flag `ner` gebaut **und**
    /// das Modell unter `$EXE_DIR/models/` vorhanden ist. Default off,
    /// damit auch der Slim-Build (~15 MB) unverändert nutzbar bleibt.
    #[serde(default)]
    pub enable_ner: bool,

    /// Toast-Notifications nach Smart-Paste anzeigen.
    ///
    /// **Default off**, weil Tester-Erfahrung zeigt: Windows-Tray-
    /// Notifications klauen kurz den Window-Focus, was dazu führen kann,
    /// dass unser synthetisches Strg+V nicht in der Ziel-App landet.
    /// Bei mute-ten Notifications klappte Paste; bei aktiver Notification-
    /// Anzeige war's bei manchen Testern stumm. User die das Feedback
    /// trotzdem wollen, können's hier einschalten.
    #[serde(default)]
    pub enable_notifications: bool,

    /// Aufbewahrungsdauer der Token-Mapping-Tabelle in Minuten.
    ///
    /// **DSGVO-Hintergrund:** solange die Mapping-Tabelle existiert, ist
    /// die Pseudonymisierung reversibel — d. h. die Tokens beim LLM
    /// bleiben weiterhin personenbezogene Daten (Art. 4(5) DSGVO). Erst
    /// nach Löschung der Mappings werden sie zu anonymen Daten außerhalb
    /// des DSGVO-Geltungsbereichs.
    ///
    /// Default: 60 Minuten (typische LLM-Session). Konfigurierbar im UI.
    /// Spezialwert **0 = nur diese Session** — Mappings werden beim
    /// nächsten App-Start gelöscht, nicht aber periodisch während der
    /// Session (sonst wäre Reverse direkt nach Forward unmöglich).
    #[serde(default = "default_retention_minutes")]
    pub retention_minutes: u32,

    /// Strict Mode: echte Anonymisierung statt reversibler Pseudonymisierung.
    ///
    /// Wenn aktiv:
    /// - Findings werden durch lesbare Platzhalter ersetzt
    ///   („Person A", „Organisation B", „Ort C" …)
    /// - **Keine Mapping-Tabelle wird angelegt** — die Zuordnungs-
    ///   information existiert nirgends
    /// - **Kein Reverse-Pfad** — Strg+Alt+B macht im strict_mode immer
    ///   Forward (Anonymisierung)
    ///
    /// Rechtsfolge: der Text ist im Moment des Transfers an einen LLM
    /// **anonym** im Sinne von Erwägungsgrund 26 DSGVO. Die DSGVO
    /// findet auf diese Daten keine Anwendung — kein AVV-Bedarf,
    /// kein Drittlandtransfer-Problem. Trade-off: User muss die
    /// LLM-Antwort manuell auf den Kontext zurückführen.
    ///
    /// Default off (= reversibler Modus).
    #[serde(default)]
    pub strict_mode: bool,

    /// First-Run-Onboarding bereits durchgelaufen?
    ///
    /// Default `false` → beim ersten App-Start zeigt das Frontend einen
    /// Wizard, der Hotkey, Modus, Retention, NER-Download und Permissions
    /// abfragt. Nach Abschluss wird das Flag auf `true` gesetzt und der
    /// Wizard erscheint nicht mehr.
    ///
    /// Wer den Wizard wieder sehen will: Settings-File löschen oder
    /// dieses Feld manuell auf `false` setzen.
    #[serde(default)]
    pub onboarded: bool,

    /// Ablage-Einträge der Schwärz-Bühne beim App-Quit löschen
    /// (Session-only-Ablage). Default off — die Ablage überlebt Neustarts,
    /// bis der User manuell löscht.
    #[serde(default)]
    pub stash_clear_on_quit: bool,

    /// Hotkey für „Markierung schwärzen & ablegen" (Capture → Bühne).
    /// Default: `CmdOrCtrl+Alt+Shift+B`. **Leerer String = Feature aus** —
    /// dann wird kein zweiter Shortcut registriert und der Smart-Paste-Hotkey
    /// bleibt der einzige. Bewusst separat vom Smart-Paste-Hotkey (`hotkey`),
    /// weil die Bühne ein paralleler, sichtbarer Workflow ist.
    #[serde(default = "default_stage_hotkey")]
    pub stage_hotkey: String,

    /// Animations-Stil der Schwärz-Bühne:
    /// `"slow"` | `"normal"` | `"fast"` | `"off"`.
    /// Das Backend interpretiert den Wert **nicht** — es reicht ihn nur ans
    /// Frontend durch, das die Marker-Animation entsprechend abspielt.
    /// (Der frühere Wert `"full"` wird vom Frontend als `"normal"` behandelt.)
    #[serde(default = "default_stage_animation")]
    pub stage_animation: String,

    /// Schwebendes Mini-Widget anzeigen (nicht-aktivierender Klick-Einstieg
    /// in die Bühne, aktuell nur macOS). Default off — ein permanent
    /// schwebendes Fensterchen muss eine bewusste Entscheidung sein.
    #[serde(default)]
    pub show_widget: bool,

    /// Zuletzt gemerkte Widget-Position (logische Pixel, Ursprung oben
    /// links). `None` = noch nie bewegt, OS platziert das Fenster.
    #[serde(default)]
    pub widget_position: Option<(f64, f64)>,
}

fn default_retention_minutes() -> u32 {
    60
}

fn default_stage_hotkey() -> String {
    "CmdOrCtrl+Alt+Shift+B".into()
}

fn default_stage_animation() -> String {
    "normal".into()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            // Strg+Alt+B (Win/Linux) / Cmd+Option+B (Mac). Beta-Lessons:
            //   - „CmdOrCtrl+B" kollidiert mit Bold-Toggle (Notepad24H2+,
            //     Word, Outlook, Web-Editoren)
            //   - „CmdOrCtrl+Shift+V" ist „Paste plain" in Browser/Word
            //   - „CmdOrCtrl+Alt+V" ist „Inhalte einfügen…" in Office
            // Strg+Alt+B ist in Office, Browsern und Standard-Editoren
            // nicht belegt. „B" semantisch für „anonyMize/Block PII".
            hotkey: "CmdOrCtrl+Alt+B".into(),
            auto_detection: false,
            enable_ner: false,
            enable_notifications: false,
            retention_minutes: 60,
            strict_mode: false,
            onboarded: false,
            stash_clear_on_quit: false,
            stage_hotkey: default_stage_hotkey(),
            stage_animation: default_stage_animation(),
            show_widget: false,
            widget_position: None,
        }
    }
}

impl Settings {
    /// Lädt Settings aus dem JSON-File. Bei Fehler (File fehlt, Parse-Fehler,
    /// kein data_dir verfügbar) liefert [`Default`].
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            log::debug!("settings: no data_dir, using defaults");
            return Self::default();
        };
        let mut loaded: Self = if let Ok(content) = std::fs::read_to_string(&path) {
            serde_json::from_str(&content).unwrap_or_else(|e| {
                log::warn!("settings: parse failed: {e}, using defaults");
                Self::default()
            })
        } else {
            Self::default()
        };

        // **Hotkey-Auto-Migration:** bisherige Defaults migrieren auf
        // den aktuellen „CmdOrCtrl+Alt+B":
        //   - „CmdOrCtrl+B" (frühe Beta) → Bold-Toggle-Kollision
        //   - „CmdOrCtrl+Shift+V" (Zwischenversion) → Paste-plain-Kollision
        //   - „CmdOrCtrl+Alt+V" (Zwischenversion) → Office Paste-Special
        // Da bisher kein UI für Hotkey-Wahl existierte, sind alle drei
        // ausschließlich unsere alten Defaults gewesen — eine
        // Auto-Migration überstimmt also keine User-Wahl.
        if matches!(
            loaded.hotkey.as_str(),
            "CmdOrCtrl+B" | "CmdOrCtrl+Shift+V" | "CmdOrCtrl+Alt+V"
        ) {
            log::info!(
                "settings: upgrading hotkey {} → CmdOrCtrl+Alt+B",
                loaded.hotkey
            );
            loaded.hotkey = "CmdOrCtrl+Alt+B".into();
            let _ = loaded.save();
        }

        loaded
    }

    /// Speichert die Settings als „pretty-printed" JSON. Erstellt den
    /// Parent-Ordner falls nötig.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path().ok_or_else(|| std::io::Error::other("no data_dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)
    }

    fn path() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join(APP_DIR).join(SETTINGS_FILENAME))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_safe_values() {
        let s = Settings::default();
        assert_eq!(s.hotkey, "CmdOrCtrl+Alt+B");
        assert!(!s.auto_detection, "auto_detection must be off by default");
        assert!(!s.enable_ner, "enable_ner must be off by default");
        assert!(
            !s.enable_notifications,
            "notifications must be off by default (focus-steal)"
        );
        assert_eq!(s.retention_minutes, 60, "1h Retention als Default");
        assert!(
            !s.strict_mode,
            "strict_mode off als Default (reversibel ist häufiger gewünscht)"
        );
        assert!(
            !s.stash_clear_on_quit,
            "stash_clear_on_quit off als Default (Ablage überlebt Neustart)"
        );
        assert_eq!(
            s.stage_hotkey, "CmdOrCtrl+Alt+Shift+B",
            "Capture-Hotkey-Default aus Vertrag 2.1"
        );
        assert_eq!(
            s.stage_animation, "normal",
            "Animations-Stil-Default aus Vertrag 2.1"
        );
        assert!(!s.show_widget, "Widget off als Default (bewusstes Opt-in)");
        assert!(
            s.widget_position.is_none(),
            "keine Widget-Position bis zum ersten Move"
        );
    }

    #[test]
    fn json_roundtrip() {
        let s = Settings {
            hotkey: "CmdOrCtrl+Shift+P".into(),
            auto_detection: true,
            enable_ner: true,
            enable_notifications: true,
            retention_minutes: 15,
            strict_mode: true,
            onboarded: true,
            stash_clear_on_quit: true,
            show_widget: true,
            widget_position: Some((120.0, 240.0)),
            stage_hotkey: "CmdOrCtrl+Alt+G".into(),
            stage_animation: "fast".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        let s2: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(s.hotkey, s2.hotkey);
        assert_eq!(s.auto_detection, s2.auto_detection);
        assert_eq!(s.enable_ner, s2.enable_ner);
        assert_eq!(s.enable_notifications, s2.enable_notifications);
        assert_eq!(s.retention_minutes, s2.retention_minutes);
        assert_eq!(s.strict_mode, s2.strict_mode);
        assert_eq!(s.stash_clear_on_quit, s2.stash_clear_on_quit);
        assert_eq!(s.stage_hotkey, s2.stage_hotkey);
        assert_eq!(s.stage_animation, s2.stage_animation);
        assert_eq!(s.show_widget, s2.show_widget);
        assert_eq!(s.widget_position, s2.widget_position);
    }

    #[test]
    fn missing_enable_ner_field_loads_as_false() {
        // Forward-Compat: alte Settings-Files ohne `enable_ner` müssen
        // weiterhin laden. Der #[serde(default)] auf dem Feld macht das.
        let legacy_json = r#"{"hotkey":"CmdOrCtrl+B","auto_detection":false}"#;
        let s: Settings = serde_json::from_str(legacy_json).unwrap();
        assert!(!s.enable_ner);
    }

    #[test]
    fn missing_stage_fields_load_as_defaults() {
        // Forward-Compat: eine settings.json aus der Zeit vor der
        // Schwärz-Bühne kennt `stage_hotkey`/`stage_animation` nicht. Die
        // `#[serde(default = …)]`-Attribute müssen die Vertragswerte liefern,
        // sonst bricht das Laden bestehender Installationen.
        let legacy_json = r#"{"hotkey":"CmdOrCtrl+Alt+B","auto_detection":false}"#;
        let s: Settings = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(s.stage_hotkey, "CmdOrCtrl+Alt+Shift+B");
        assert_eq!(s.stage_animation, "normal");
    }
}
