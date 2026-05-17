//! Smart-Paste-Hotkey-Handler.
//!
//! Der Handler wird vom `tauri-plugin-global-shortcut` aufgerufen, sobald
//! der konfigurierte Hotkey (Default: Strg+B / Cmd+B) gedrückt wurde. Er ist
//! die **primäre UX** der App.
//!
//! # Clipboard-Verhalten nach Forward (kein Auto-Restore)
//!
//! Beim Strg+B-Druck schreiben wir die transformierte Version ins Clipboard
//! und feuern einen synthetischen Strg+V. Das transformierte Pseudonym
//! **bleibt danach im Clipboard** — wir restoren das Original *nicht*
//! automatisch zurück.
//!
//! ```text
//!  T=0    User kopiert Email mit PII (Original im Clipboard)
//!  T=...  User drückt Strg+B
//!         ├─ App liest Original
//!         ├─ Detect → PII gefunden, Pseudo-Version erzeugt
//!         ├─ Pseudo ins Clipboard schreiben
//!         └─ Synthetic Ctrl+V → Ziel-App pastet Pseudo
//!  T=...  Clipboard enthält jetzt das Pseudonym (nicht mehr den Klartext)
//!         → ein zweiter Strg+B würde via Token-Erkennung Reverse
//!           triggern und das Original wieder einfügen
//! ```
//!
//! ## Warum nicht Auto-Restore?
//!
//! Frühere Versionen haben nach ~500 ms das Original zurückgeschrieben.
//! Problem: Strg+V ist ein asynchrones OS-Event. Träge Ziel-Apps
//! (Electron-LLM-Clients unter Last, VMs, langsame Remote-Sessions)
//! lesen das Clipboard erst Sekunden später aus — die kriegen dann den
//! Klartext statt des Pseudonyms. **Genau das, was wir verhindern
//! wollen.** Das automatische Wiederherstellen ist daher entfernt.
//!
//! Trade-off: Wer manuell weiterpastet, bekommt das Pseudonym, nicht
//! das Original. Wer das Original braucht, kopiert aus der Quelle neu —
//! oder triggert über den zweiten Strg+B-Druck den Reverse-Pfad
//! (Tokens → Originale).
//!
//! # Entscheidungslogik (decide_action)
//!
//! - Enthält der Clipboard-Text **PII** → pseudonymisieren und einfügen
//! - Enthält der Clipboard-Text **bekannte Tokens** → Originale wiederherstellen
//!   und einfügen
//! - Sonst → einfach normales Strg+V durchreichen (Pass-Through)
//!
//! # Threat-Model-Anmerkung
//!
//! Der globale Hotkey wird über `RegisterHotKey` (Win) / Carbon Events
//! (Mac) registriert. Das ist **kein Keylogger-Hook** — der OS-Kernel
//! routet nur diesen konkreten Shortcut an uns. Synthetisches Strg+V
//! über `SendInput` (Win) / `CGEvent` (Mac, via `enigo`) ist die
//! Standard-API, die auch Password-Manager und Text-Expander nutzen
//! — kein EDR-Triggern.

use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

use crate::{detection, storage};

/// Hartes Größen-Limit für Clipboard-Inhalte (10 MB).
///
/// Schutz gegen versehentlich kopierte Riesentexte (Logfiles,
/// Base64-Image-Strings): Regex-Iteration über 50+ MB würde den
/// Tauri-Prozess Sekunden bis Minuten einfrieren oder OOM erzeugen.
/// Oberhalb des Limits machen wir Pass-Through statt Detection.
///
/// Reale PII-Texte sind selten >50 KB; 10 MB sind komfortabel über
/// jedem realistischen Use-Case und gleichzeitig sicher unterhalb der
/// Heap-Fragmentations-Klippe der Regex-Engine.
const MAX_CLIPBOARD_BYTES: usize = 10 * 1024 * 1024;

/// Vom `tauri-plugin-global-shortcut`-Handler aufgerufen, sobald der Hotkey
/// (Strg+B / Cmd+B) gedrückt wurde.
pub fn handle(app: &AppHandle) {
    log::info!("hotkey pressed (Smart-Paste)");

    // 1. Aktuellen Clipboard-Inhalt lesen.
    let Some(original) = crate::clipboard::read_clipboard_text() else {
        log::info!("hotkey: clipboard empty or unreadable, sending plain paste");
        send_paste();
        return;
    };
    if original.is_empty() {
        log::info!("hotkey: clipboard text is empty, sending plain paste");
        send_paste();
        return;
    }

    // 1b. Größen-Cap. Oberhalb 10 MB überspringen wir Detection komplett
    //     und reichen ein normales Strg+V durch — sonst friert die App
    //     auf großen Logfiles/Base64-Blobs ein.
    if original.len() > MAX_CLIPBOARD_BYTES {
        log::warn!(
            "hotkey: clipboard {} bytes exceeds {}-byte limit, pass-through",
            original.len(),
            MAX_CLIPBOARD_BYTES
        );
        send_paste();
        if crate::settings::Settings::load().enable_notifications {
            let _ = app
                .notification()
                .builder()
                .title("Clipboard-PII")
                .body(format!(
                    "Inhalt zu groß ({} MB) — Pass-Through ohne PII-Prüfung",
                    original.len() / (1024 * 1024)
                ))
                .show();
        }
        return;
    }

    // 2. Inhalt analysieren — PII detektieren, Tokens detektieren, oder nix.
    let action = decide_action(&original);
    log::info!(
        "hotkey: text len={}, replacement={}, notification={}",
        original.len(),
        action.replacement.is_some(),
        action.notification.is_some()
    );

    if let Some(ref transformed) = action.replacement {
        // 3a. Transformierte Version ins Clipboard schreiben.
        if let Err(e) = crate::clipboard::write_clipboard_text(transformed) {
            log::warn!("hotkey: clipboard write failed: {e}, sending plain paste");
            send_paste();
            return;
        }
        log::info!(
            "hotkey: clipboard rewritten to transformed text ({} chars)",
            transformed.len()
        );

        // 3b. Synthetic Strg+V → Ziel-App pastet die transformierte Version.
        //
        // Das transformierte Pseudonym bleibt im Clipboard. Frühere
        // Versionen haben nach ~500 ms das Original zurückgeschrieben —
        // bei trägen Ziel-Apps (Electron-LLM-Clients unter Last, VMs,
        // Remote-Sessions) konnte die App das Clipboard erst nach dem
        // Restore auslesen und kriegte dann den Klartext statt des
        // Pseudonyms. Sicherer ist: kein Restore.
        send_paste();
        log::info!("hotkey: synthetic paste sent, pseudonym remains in clipboard");
    } else {
        // Kein Transform — einfach den Standard-Paste durchreichen.
        send_paste();
    }

    // 4. User informieren — Notification mit kurzer Erfolgsmeldung.
    //
    // **Default off** seit der Beta: Windows-Tray-Notifications klauen
    // bei manchen Konfigurationen den Window-Focus, was unser synthetisches
    // Strg+V ins Leere laufen lässt. Wer das Feedback explizit will,
    // schaltet es in den Settings ein.
    let show_notifications = crate::settings::Settings::load().enable_notifications;
    if show_notifications {
        if let Some(msg) = action.notification {
            let _ = app
                .notification()
                .builder()
                .title("Streichzeug")
                .body(msg)
                .show();
        }
    }
}

/// Entscheidung, was beim Hotkey passieren soll. Ausgelagert, damit man
/// es unit-testen kann ohne `tauri`/`enigo`-Infrastruktur.
struct Action {
    /// `Some(text)`, wenn das Clipboard mit einem neuen Inhalt überschrieben
    /// werden soll. `None` bei „nichts transformieren, einfach pasten".
    replacement: Option<String>,
    /// Toast-Text. `None` bei Pass-Through (kein Toast-Spam für normales Paste).
    notification: Option<String>,
}

fn decide_action(text: &str) -> Action {
    let settings = crate::settings::Settings::load();

    // **Strict Mode hat eigenen Pfad.** Anonymisierung statt Pseudonymisierung:
    // - lesbare Platzhalter („Person A", „Organisation B", …)
    // - kein Mapping wird gespeichert
    // - kein Reverse-Pfad — Strg+Alt+B macht immer Forward
    // Damit sind die Daten beim LLM anonym (ErwGr. 26 DSGVO),
    // außerhalb des DSGVO-Geltungsbereichs. Trade-off: User muss
    // manuell zurückführen.
    if settings.strict_mode {
        let findings = detection::detect_strict(text);
        if findings.is_empty() {
            log::info!("hotkey: strict mode, pass-through — no PII");
            return Action {
                replacement: None,
                notification: None,
            };
        }
        let anonymized = detection::apply_strict_with_hint(text, &findings);
        log::info!(
            "hotkey: strict mode forward — {} PII anonymisiert (kein Mapping gespeichert)",
            findings.len()
        );
        return Action {
            replacement: Some(anonymized),
            notification: Some(format!(
                "{} PII anonymisiert (Strict Mode — nicht reversibel)",
                findings.len()
            )),
        };
    }

    // **Reverse hat Vorrang, sobald Tokens im Text sind.**
    //
    // Begründung: Layer-3 NER produziert auf bereits tokenisiertem Text
    // fast immer False-Positives — das Modell wurde auf normalem Text
    // trainiert, kennt unsere `«…»`-Klammern nicht und klassifiziert
    // gerne Wörter wie „Vorstand", „Damen", „Herren" als PER/ORG.
    // Würden wir wie früher zuerst `detect()` aufrufen, käme nahezu
    // immer ein nicht-leerer Findings-Vec, Forward würde statt Reverse
    // laufen, und der User bekäme erneut tokenisierten Text statt
    // seiner Originale.
    if has_any_token(text) {
        // Reverse durchsucht **alle Cases** — Forward-Operationen
        // generieren jeweils frische case_ids, also können wir nicht
        // einen festen case_id-Filter verwenden.
        let restored = storage::restore_all_cases(text);
        if restored != text {
            let replaced_count = text
                .matches('«')
                .count()
                .saturating_sub(restored.matches('«').count());
            log::info!("hotkey: reverse path — {replaced_count} token(s) restored");
            return Action {
                replacement: Some(restored),
                notification: Some(format!(
                    "{replaced_count} Pseudonym(e) zurück zu Originalen — eingefügt"
                )),
            };
        }
        log::debug!("hotkey: tokens present but none in mappings, falling through to forward");
    }

    // Forward: hat der Text PII?
    //
    // **DSGVO-Pseudonymisierung:** jede Forward-Operation bekommt eine
    // frische case_id. Damit ist derselbe Klartext in zwei separaten
    // Forward-Calls **nicht durch identische Tokens verknüpfbar** —
    // selbst wenn jemand LLM-Logs zusammenklebt. Innerhalb desselben
    // Forwards bleibt die Tokenisierung stabil (wiederholte Substrings
    // bekommen denselben Token, weil HMAC deterministisch ist), damit
    // der LLM Wiederholungen als „dieselbe Entität" verstehen kann.
    let case_id = crate::secrets::new_case_id();
    let findings = detection::detect_with_case(text, &case_id);
    if !findings.is_empty() {
        for f in &findings {
            storage::record(&case_id, &f.token, &f.original);
        }
        let pseud = detection::apply_tokens_with_hint(text, &findings, &case_id);
        log::info!(
            "hotkey: forward path — {} PII finding(s) replaced in case {}",
            findings.len(),
            case_id
        );
        return Action {
            replacement: Some(pseud),
            notification: Some(format!(
                "{} PII durch Pseudonyme ersetzt — eingefügt",
                findings.len()
            )),
        };
    }

    // Kein PII, keine bekannten Tokens — normales Paste durchreichen.
    log::info!("hotkey: pass-through — no PII, no known tokens");
    Action {
        replacement: None,
        notification: None,
    }
}

/// Sucht im Text mindestens ein Vorkommen unseres Token-Patterns
/// `«X_abc123»`. Reine Form-Erkennung — ob das Token in der Mapping-DB
/// existiert, prüft erst `storage::restore`.
fn has_any_token(text: &str) -> bool {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(crate::tokens::TOKEN_REGEX_PATTERN).unwrap());
    RE.is_match(text)
}

/// Simuliert ein Strg+V (Cmd+V auf macOS) an die aktive App.
///
/// # Bold-Toggle-Vermeidung
///
/// Wenn der User Strg+B drückt und sehr schnell schreibt, hat er die
/// Strg-Taste oft noch gedrückt, während wir Strg+V senden. Das OS
/// sieht dann nahtlos „Strg+B → Strg+V", und je nach Ziel-App kann das
/// Strg+B als Bold-Toggle aktiviert lassen oder die Rich-Text-Modifier
/// vermischen.
///
/// Daher:
/// 1. **100 ms warten** (statt 50) — gibt dem User mehr Zeit loszulassen
/// 2. **Alle Modifier explizit auf Release setzen** vor der Strg+V-
///    Sequenz. Wenn die schon released sind, ist das ein No-Op.
/// 3. Erst dann unsere Strg+V-Press/Click/Release-Sequenz schicken.
fn send_paste() {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings as EnigoSettings};

    std::thread::sleep(Duration::from_millis(100));

    let mut enigo = match Enigo::new(&EnigoSettings::default()) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("hotkey: enigo init failed: {e:?}");
            return;
        }
    };

    // Belt-and-suspenders: alle Modifier vor unserer Sequenz „löschen",
    // damit kein vom User noch gehaltener Strg/Shift/Alt die folgende
    // Strg+V-Sequenz kontaminiert. Auf macOS zusätzlich Meta/Cmd.
    let _ = enigo.key(Key::Control, Direction::Release);
    let _ = enigo.key(Key::Shift, Direction::Release);
    let _ = enigo.key(Key::Alt, Direction::Release);
    #[cfg(target_os = "macos")]
    {
        let _ = enigo.key(Key::Meta, Direction::Release);
    }

    // macOS nutzt Cmd (Meta), Win/Linux nutzt Strg.
    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;

    // Jedes enigo-Result loggen, damit wir bei „kein Paste sichtbar"
    // im Tester-Log sehen, ob SendInput überhaupt durchkam oder vom
    // EDR/AV blockiert wurde.
    let r1 = enigo.key(modifier, Direction::Press);
    let r2 = enigo.key(Key::Unicode('v'), Direction::Click);
    let r3 = enigo.key(modifier, Direction::Release);
    if let Err(e) = &r1 {
        log::warn!("hotkey: enigo modifier press failed: {e:?}");
    }
    if let Err(e) = &r2 {
        log::warn!("hotkey: enigo V click failed: {e:?}");
    }
    if let Err(e) = &r3 {
        log::warn!("hotkey: enigo modifier release failed: {e:?}");
    }
    if r1.is_ok() && r2.is_ok() && r3.is_ok() {
        log::info!("hotkey: enigo Strg+V sequence returned Ok for all 3 calls");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_pii_no_tokens_no_replacement() {
        let action = decide_action("Hello world without any sensitive data.");
        assert!(action.replacement.is_none());
        assert!(action.notification.is_none());
    }

    #[test]
    fn pii_triggers_forward_replacement() {
        let action = decide_action("Email an foo@bar.de bitte.");
        assert!(action.replacement.is_some());
        let r = action.replacement.unwrap();
        assert!(r.contains("«E_"), "expected email token, got: {r}");
        assert!(r.contains("Hinweis"), "should append LLM hint");
    }

    #[test]
    fn unknown_tokens_dont_trigger_reverse() {
        // Token-Pattern matcht 6 lowercase alphanum nach dem Underscore.
        // "unknown" hat 7 Zeichen, also: has_any_token = false → fall
        // through zu Forward, der bei klartext-armem Input nichts findet
        // → Action None.
        let action = decide_action("Hier ist «P_unknown» drin.");
        assert!(action.replacement.is_none());
    }

    #[test]
    fn known_token_triggers_reverse_first() {
        // Reverse hat Vorrang vor Forward, sobald Tokens im Text sind —
        // sonst würde NER auf den Klartext-Teilen False-Positives
        // produzieren und Forward statt Reverse laufen lassen.
        crate::storage::record("default", "«P_xyz789»", "Max Müller");
        let action = decide_action("Schreiben an «P_xyz789» mit Vorstand-Bezug");
        assert!(action.replacement.is_some(), "expected reverse to fire");
        let r = action.replacement.unwrap();
        assert!(
            r.contains("Max Müller"),
            "expected reverse to insert 'Max Müller', got: {r}"
        );
        assert!(
            !r.contains("«P_xyz789»"),
            "expected token to be replaced, got: {r}"
        );
    }
}
