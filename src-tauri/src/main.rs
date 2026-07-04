//! Streichzeug — Tauri-App-Entry.
//!
//! Diese App ist eine **Tray-residente Hintergrundanwendung**. Die primäre
//! User-Interaktion ist ein **globaler Hotkey** (Default: Strg+B / Cmd+B),
//! der das Clipboard inhaltsbasiert transformiert:
//!
//! - Klartext mit PII → pseudonymisierte Tokens (Forward)
//! - Text mit bekannten Tokens → Originale (Reverse)
//! - Sonst → unverändertes Paste durchreichen
//!
//! Siehe [`hotkey`] für die Smart-Paste-Logik, [`settings`] für die
//! Persistenz der User-Konfiguration.
//!
//! # Optionale Auto-Detection
//!
//! Als Power-User-Feature gibt es einen Foreground-aware Clipboard-Watcher
//! ([`clipboard`] + [`foreground`]), der **default off** ist. Aktivierung
//! über das Tray-Menü → erfordert App-Neustart, weil der Watcher in der
//! `setup()`-Phase gespawnt wird.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod applog; // Logging (stdout + rotierende Datei)
mod clipboard; // plattform-spezifische Clipboard-APIs (Watcher + Read/Write)
mod detection; // PII-Erkennung (Regex + Gazetteer + Heuristiken)
mod foreground; // Foreground-App-Detektion (nur bei Auto-Detection genutzt)
mod gazetteer; // statische DE-Namensliste für Layer 2
mod hotkey; // Smart-Paste-Handler (primäre UX)
mod ner; // Layer-3 NER (optional, feature = "ner")
mod secrets; // HMAC-Master-Secret-Verwaltung
mod settings; // User-Settings (Hotkey, Auto-Detection-Toggle)
mod storage; // SQLite-basierter Mapping-Store
mod tokens; // Token-Generierung «T_<hash>»

use clipboard::{ClipboardWatcher, PlatformWatcher};
use detection::Finding;
use settings::Settings;
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_notification::NotificationExt;

// =================================================================== Tauri-Commands
//
// Vom Svelte-Frontend per `invoke("name", { args })` aufrufbar.

/// Frontend-Command: PII-Detection ohne Pseudonymisierung. Liefert Findings
/// zurück, damit das UI sie anzeigen kann.
#[tauri::command]
fn detect_pii(text: String) -> Vec<Finding> {
    detection::detect(&text)
}

/// Frontend-Command: erkennt + pseudonymisiert in einem Schritt. Speichert
/// die Mappings für späteres Reverse.
#[tauri::command]
fn pseudonymize(text: String, case_id: String) -> String {
    let findings = detection::detect(&text);
    for f in &findings {
        storage::record(&case_id, &f.token, &f.original);
    }
    detection::apply_tokens_with_hint(&text, &findings, &case_id)
}

/// Frontend-Command: tauscht alle bekannten Tokens im Text durch ihre
/// Originale. Wird vom Reverse-Pfad genutzt.
#[tauri::command]
fn restore_text(text: String, case_id: String) -> String {
    storage::restore(&case_id, &text)
}

/// Frontend-Command: aktuelle Settings lesen.
#[tauri::command]
fn get_settings() -> Settings {
    Settings::load()
}

/// Öffnet den Log-Ordner im OS-File-Manager (Explorer auf Win, Finder
/// auf Mac). Vom Tray-Menü und vom UI-Button gerufen, damit der User
/// für Bug-Reports einfachen Zugriff hat.
fn open_log_folder_impl() {
    let Some(dir) = applog::log_dir_path() else {
        log::warn!("open_log_folder: log_dir_path() returned None");
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("open_log_folder: create_dir_all failed: {e}");
    }
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer.exe").arg(&dir).spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&dir).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(&dir).spawn();
    if let Err(e) = result {
        log::warn!("open_log_folder: spawn failed: {e}");
    }
}

#[tauri::command]
fn open_log_folder() {
    open_log_folder_impl();
}

/// Frontend-Command: aktuelle App-Version aus Cargo.toml.
/// Beim Bug-Report mitschicken, damit der Tester nicht raten muss
/// welche MSI installiert ist.
#[tauri::command]
fn get_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Frontend-Command: kopiert die letzten N Zeilen aus app.log ins
/// Clipboard. Der User pastet's dann in seine Bug-Report-Mail.
/// 200 Zeilen ≈ Snapshot der letzten paar App-Starts, reicht für
/// typische Diagnose ohne den Mail-Anhang aufzublähen.
#[tauri::command]
fn copy_log_to_clipboard() -> Result<usize, String> {
    let tail = applog::read_tail(200).ok_or_else(|| "kein Log gefunden".to_string())?;
    clipboard::write_clipboard_text(&tail).map_err(|e| e.to_string())?;
    Ok(tail.lines().count())
}

/// Frontend-Command: Status der Layer-3-NER-Engine.
///
/// - `built_with_ner_feature`: wurde die Binary mit `--features ner` kompiliert?
/// - `enabled`: User-Setting `enable_ner`
/// - `ready`: Engine ist tatsächlich geladen (Modell-Files vorhanden,
///   ORT-Library findet, Init durchgelaufen)
#[tauri::command]
fn get_ner_status() -> serde_json::Value {
    let built = cfg!(feature = "ner");
    let cfg = Settings::load();
    let ready = ner::is_ready();
    let model_files_present = ner::model_files_present();
    let user_models_dir = ner::user_models_dir().map(|p| p.display().to_string());
    serde_json::json!({
        "built_with_ner_feature": built,
        "enabled": cfg.enable_ner,
        "ready": ready,
        "model_files_present": model_files_present,
        "user_models_dir": user_models_dir,
    })
}

/// Frontend-Command: lädt das NER-Modell zur Laufzeit in den User-Daten-
/// Pfad. Asynchron, weil's mehrere 100 MB sind. Liefert den Pfad zurück,
/// in den die Files geschrieben wurden.
#[tauri::command]
async fn download_ner_model() -> Result<String, String> {
    ner::download_models()
        .await
        .map(|p| p.display().to_string())
}

/// Frontend-Command: Settings aktualisieren. Manche Felder erfordern
/// App-Neustart (auto_detection, hotkey) — der Aufrufer ist verantwortlich,
/// den User darauf hinzuweisen.
#[tauri::command]
fn update_settings(new_settings: Settings) -> Result<(), String> {
    new_settings.save().map_err(|e| e.to_string())
}

/// Frontend-Command: Anzahl der aktuell gespeicherten Mappings + Retention.
/// Wird im Datenspeicherungs-Bereich angezeigt.
#[tauri::command]
fn get_storage_status() -> serde_json::Value {
    let count = storage::mapping_count();
    let retention = Settings::load().retention_minutes;
    serde_json::json!({
        "mapping_count": count,
        "retention_minutes": retention,
    })
}

/// Frontend-Command: löscht alle Mappings sofort. Wird vom Button
/// „Jetzt alle Mappings löschen" gerufen. Liefert die Anzahl der
/// gelöschten Einträge zurück.
#[tauri::command]
fn clear_all_mappings() -> usize {
    storage::purge_all()
}

/// Zeigt die einmalige Warnung, dass das Master-Secret nicht persistiert
/// werden konnte und diese Sitzung ein temporäres (ephemeres) Secret nutzt —
/// bestehende Tokens sind dann nicht mehr rückübersetzbar. Fehler-Kanal,
/// bewusst unabhängig von `enable_notifications`.
fn warn_ephemeral_fallback(app: &tauri::AppHandle) {
    log::error!(
        "master secret ephemeral fallback active — bestehende Tokens sind nicht mehr rückübersetzbar"
    );
    let _ = app
        .notification()
        .builder()
        .title("Streichzeug — Schlüssel nicht gespeichert")
        .body(
            "Der Verschlüsselungs-Schlüssel konnte nicht gespeichert werden. \
Diese Sitzung nutzt einen temporären Schlüssel: bereits pseudonymisierte Texte lassen \
sich nicht mehr zurückübersetzen, und neue Pseudonyme gelten nur bis zum Beenden der App. \
Bitte Schreibrechte im App-Datenverzeichnis prüfen und die App neu starten.",
        )
        .show();
}

/// Frontend-Command: schließt die Ersteinrichtung ab, indem der
/// Verschlüsselungs-Schlüssel initialisiert wird. Genau hier fragt
/// macOS/Windows **einmalig** nach Schlüsselbund-Zugriff — der Onboarding-
/// Wizard kündigt das im letzten Schritt direkt davor an, damit der Dialog
/// nicht unvermittelt kommt. Öffnet zusätzlich die verschlüsselte Mapping-DB
/// (legt sie an bzw. migriert eine bestehende Klartext-DB), damit auch der
/// abgeleitete SQLCipher-Schlüssel jetzt mit Kontext angefordert wird.
///
/// Gibt `true` zurück, wenn kein persistenter Schlüssel möglich war
/// (ephemerer Fallback aktiv) — das Frontend zeigt dann einen Hinweis.
#[tauri::command]
fn finalize_secret_setup(app: tauri::AppHandle) -> bool {
    let ephemeral = secrets::init();
    let _ = storage::mapping_count();
    if ephemeral {
        warn_ephemeral_fallback(&app);
    }
    ephemeral
}

// =================================================================== Entry-Point

fn main() {
    applog::init();

    let cfg = Settings::load();
    log::info!(
        "starting Streichzeug v{} (hotkey={}, auto_detection={}, enable_ner={}, retention_minutes={}, strict_mode={})",
        env!("CARGO_PKG_VERSION"),
        cfg.hotkey, cfg.auto_detection, cfg.enable_ner, cfg.retention_minutes, cfg.strict_mode
    );
    if cfg.enable_ner {
        // Eager-Init beim Start, damit der erste Hotkey-Druck nicht
        // die Modell-Lade-Latenz absorbieren muss (~300 ms typisch).
        let ready = ner::is_ready();
        log::info!("ner: enable_ner=true, engine_ready={ready}");
    }

    // DSGVO-Retention beim App-Start anwenden.
    //
    // - retention_minutes > 0: Mappings älter als N Minuten löschen
    //   (alte Sessions wegräumen)
    // - retention_minutes == 0: „nur diese Session" — alle Mappings
    //   aus vorigen Sessions löschen, in dieser Session werden Mappings
    //   wieder aufgebaut und beim nächsten Start wieder weggeräumt
    // Der Start-Purge öffnet die verschlüsselte Mapping-DB und löst damit
    // den ersten Schlüsselbund-Zugriff (macOS-Dialog) aus. Beim allerersten
    // Start wird das bewusst aufgeschoben: erst nach dem Onboarding, in dem
    // wir den Dialog ankündigen (siehe finalize_secret_setup). Vor dem
    // Onboarding existiert ohnehin keine DB, es gäbe nichts zu löschen.
    if cfg.onboarded {
        if cfg.retention_minutes == 0 {
            storage::purge_all();
        } else {
            storage::purge_older_than(cfg.retention_minutes);
        }
    }

    // Periodischer Purge-Thread. Läuft alle 5 Minuten und löscht
    // Mappings, die die Retention überschritten haben. Bei retention=0
    // läuft kein periodischer Purge — sonst wäre Reverse direkt nach
    // Forward unmöglich.
    let retention_for_thread = cfg.retention_minutes;
    if retention_for_thread > 0 {
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(300));
            storage::purge_older_than(retention_for_thread);
        });
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        hotkey::handle(app);
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            detect_pii,
            pseudonymize,
            restore_text,
            get_settings,
            update_settings,
            get_ner_status,
            download_ner_model,
            open_log_folder,
            copy_log_to_clipboard,
            get_version,
            get_storage_status,
            clear_all_mappings,
            finalize_secret_setup,
        ])
        .setup(move |app| {
            // ----------------------------------- Master-Secret prüfen
            // Erzwingt die Init des HMAC-Master-Secrets. Konnte es nicht
            // persistiert werden, läuft die App mit einem pro Start neu
            // zufälligen Secret — dann sind **alle bisherigen Tokens für
            // immer unlesbar**. Das darf nicht still passieren: einmalige
            // Warnung an den Nutzer (Fehler-Kanal, unabhängig von
            // enable_notifications).
            //
            // Nur für bereits eingerichtete Installationen (onboarded). Beim
            // allerersten Start wird der Secret-/Schlüsselbund-Zugriff
            // aufgeschoben, bis der Nutzer ihn im Onboarding bestätigt hat
            // (Command `finalize_secret_setup`) — sonst käme der macOS-Dialog
            // unangekündigt vor dem Wizard.
            if Settings::load().onboarded && secrets::init() {
                warn_ephemeral_fallback(app.handle());
            }

            // ----------------------------------- macOS: Regular-Policy
            // Dock-Icon + Cmd+Tab-Eintrag wie eine normale App (Mail/Notes).
            // Zusammen mit Hide-on-Close und dem Reopen-Handler (siehe unten
            // in run()) ergibt das Standard-macOS-Verhalten: Rotes X versteckt
            // das Fenster, App bleibt im Dock, Dock-Klick holt es zurück.
            // Bewusst *kein* Accessory/Tray-only mehr — die geplanten
            // fenster-zentrierten Features (z. B. Live-Schwärzung geladener
            // Dateien) brauchen eine sichtbare, per Cmd+Tab erreichbare App.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Regular);

            // ----------------------------------- Hide-on-close
            if let Some(window) = app.get_webview_window("main") {
                let win = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        let _ = win.hide();
                        api.prevent_close();
                    }
                });
            }

            // ----------------------------------- Tray-Icon mit Toggle
            let auto_item = CheckMenuItem::with_id(
                app,
                "toggle_auto",
                "Auto-Detection (experimentell, Restart nötig)",
                true,
                cfg.auto_detection,
                None::<&str>,
            )?;
            // L3-NER-Toggle. Label hängt davon ab, ob das Feature gebaut
            // wurde — andernfalls wäre ein aktivierbarer Eintrag irreführend.
            let ner_label = if cfg!(feature = "ner") {
                "Erweiterte Erkennung (lokales KI-Modell, Restart nötig)"
            } else {
                "Erweiterte Erkennung — Build ohne --features ner"
            };
            let ner_item = CheckMenuItem::with_id(
                app,
                "toggle_ner",
                ner_label,
                cfg!(feature = "ner"),
                cfg.enable_ner,
                None::<&str>,
            )?;
            let show_item =
                MenuItem::with_id(app, "show", "Fenster anzeigen", true, None::<&str>)?;
            let log_item =
                MenuItem::with_id(app, "open_log", "Log-Ordner öffnen", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Beenden", true, None::<&str>)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let sep3 = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(
                app,
                &[&auto_item, &ner_item, &sep1, &show_item, &log_item, &sep2, &sep3, &quit_item],
            )?;

            // Tray-Icon: auf macOS ein monochromes Template (P-Silhouette
            // auf transparentem Grund), das die System-Akzentfarbe annimmt —
            // konsistent mit nativen Apps wie 1Password. Auf anderen Platt-
            // formen weiter das bunte App-Icon.
            #[cfg(target_os = "macos")]
            let tray_icon = tauri::include_image!("icons/tray-icon.png");
            #[cfg(not(target_os = "macos"))]
            let tray_icon = app.default_window_icon().unwrap().clone();

            // `mut` ist nur auf macOS gebraucht — Clippy/rustc warnt sonst
            // auf Win/Linux mit `unused_mut`. Conditional-Modifier per cfg-Attr.
            #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
            let mut tray_builder = TrayIconBuilder::with_id("main-tray")
                .menu(&menu)
                .icon(tray_icon)
                .tooltip(format!("Streichzeug — Hotkey: {}", cfg.hotkey))
                .show_menu_on_left_click(true);
            #[cfg(target_os = "macos")]
            {
                tray_builder = tray_builder.icon_as_template(true);
            }
            // Clones für den Menü-Callback: bei einem Speicher-Fehler setzen
            // wir den Haken wieder auf den tatsächlich persistierten Wert
            // zurück, damit die UI nicht einen nicht-gespeicherten Zustand
            // vorgaukelt.
            let auto_item_cb = auto_item.clone();
            let ner_item_cb = ner_item.clone();
            let _tray = tray_builder
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "open_log" => open_log_folder_impl(),
                    "quit" => app.exit(0),
                    "toggle_auto" => {
                        // Persistieren, User-Hinweis dass Restart nötig ist.
                        let mut s = Settings::load();
                        s.auto_detection = !s.auto_detection;
                        let state_label = if s.auto_detection {
                            "aktiviert"
                        } else {
                            "deaktiviert"
                        };
                        match s.save() {
                            Ok(()) => {
                                let _ = app
                                    .notification()
                                    .builder()
                                    .title("Streichzeug")
                                    .body(format!(
                                        "Auto-Detection {state_label}. Bitte App neu starten, damit es greift."
                                    ))
                                    .show();
                            }
                            Err(e) => {
                                // Speichern fehlgeschlagen: Haken zurück auf
                                // den alten (persistierten) Wert und Fehler
                                // sichtbar machen, statt Erfolg vorzugaukeln.
                                log::error!("settings save failed (toggle_auto): {e}");
                                let _ = auto_item_cb.set_checked(!s.auto_detection);
                                let _ = app
                                    .notification()
                                    .builder()
                                    .title("Streichzeug — nicht gespeichert")
                                    .body(format!(
                                        "Auto-Detection konnte nicht gespeichert werden: {e}. Die Einstellung wurde nicht übernommen."
                                    ))
                                    .show();
                            }
                        }
                    }
                    "toggle_ner" => {
                        let mut s = Settings::load();
                        s.enable_ner = !s.enable_ner;
                        let state_label = if s.enable_ner {
                            "aktiviert"
                        } else {
                            "deaktiviert"
                        };
                        match s.save() {
                            Ok(()) => {
                                let _ = app
                                    .notification()
                                    .builder()
                                    .title("Streichzeug")
                                    .body(format!(
                                        "Erweiterte Erkennung {state_label}. Bitte App neu starten, damit es greift."
                                    ))
                                    .show();
                            }
                            Err(e) => {
                                log::error!("settings save failed (toggle_ner): {e}");
                                let _ = ner_item_cb.set_checked(!s.enable_ner);
                                let _ = app
                                    .notification()
                                    .builder()
                                    .title("Streichzeug — nicht gespeichert")
                                    .body(format!(
                                        "Erweiterte Erkennung konnte nicht gespeichert werden: {e}. Die Einstellung wurde nicht übernommen."
                                    ))
                                    .show();
                            }
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            // ----------------------------------- Plattform-Hinweis: Kernfeature
            // Clipboard-Read/Write ist nur für Windows und macOS implementiert.
            // Auf Linux (und sonstigen Plattformen) ist der Watcher ein Stub und
            // read/write_clipboard_text liefern None/Err — Smart-Paste tut also
            // nichts. Das darf beim Start nicht stillschweigend passieren.
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            {
                log::warn!(
                    "=================================================================\n\
                     ACHTUNG: Diese Plattform (nicht Windows/macOS) wird NICHT unterstützt.\n\
                     Das Kernfeature (Clipboard-Erkennung + Smart-Paste) ist funktionslos —\n\
                     der Clipboard-Watcher ist ein Stub. Streichzeug leistet hier nichts.\n\
                     ================================================================="
                );
                let _ = app
                    .notification()
                    .builder()
                    .title("Streichzeug — Plattform nicht unterstützt")
                    .body(
                        "Clipboard-Erkennung und Smart-Paste funktionieren nur unter \
                         Windows und macOS. Auf dieser Plattform tut die App nichts.",
                    )
                    .show();
            }

            // ----------------------------------- Globaler Hotkey registrieren
            // Tauri's `GlobalShortcutExt::register` akzeptiert den Accelerator-
            // String und löst zu `CmdOrCtrl+B` → Strg+B (Win) / Cmd+B (Mac) auf.
            match app.global_shortcut().register(cfg.hotkey.as_str()) {
                Ok(()) => log::info!("registered hotkey: {}", cfg.hotkey),
                Err(e) => log::error!(
                    "failed to register hotkey '{}': {e}. Smart-Paste wird nicht funktionieren.",
                    cfg.hotkey
                ),
            }

            // ----------------------------------- Optional: Auto-Detection-Watcher
            if cfg.auto_detection {
                spawn_watcher(app);
            } else {
                log::info!("auto-detection disabled (default) — hotkey is primary UX");
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Dock-Klick (bzw. erneutes Öffnen via Finder/Launchpad/`open -a`)
            // bei verstecktem Fenster holt es zurück nach vorn. macOS liefert
            // hierfür kein WindowEvent, sondern das RunEvent::Reopen — ohne
            // diesen Handler bliebe das Dock-Icon nach dem Schließen (Hide)
            // wirkungslos.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Some(w) = app_handle.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app_handle, event);
        });
}

// =================================================================== Optionaler Watcher

/// Spawns den Auto-Detection-Watcher (Foreground-LLM-aware). Wird nur
/// aufgerufen, wenn `settings.auto_detection == true`. Bei Settings-Toggle
/// wirkt der Wechsel erst nach App-Restart — wir reichern dafür keine
/// Live-Toggle-Komplexität an.
fn spawn_watcher(app: &tauri::App) {
    let app_handle = app.handle().clone();
    let callback: Arc<dyn Fn(String) -> Option<String> + Send + Sync> =
        Arc::new(move |text: String| {
            // Forward: PII detektieren.
            let findings = detection::detect(&text);
            if !findings.is_empty() {
                for f in &findings {
                    storage::record("default", &f.token, &f.original);
                }
                let pseud = detection::apply_tokens_with_hint(&text, &findings, "default");
                log::info!("auto-anonymize: {} PII finding(s)", findings.len());
                let _ = app_handle
                    .notification()
                    .builder()
                    .title("Streichzeug — automatisch anonymisiert")
                    .body(format!("{} PII durch Pseudonyme ersetzt.", findings.len()))
                    .show();
                return Some(pseud);
            }
            // Reverse: bekannte Tokens?
            let restored = storage::restore("default", &text);
            if restored != text {
                let replaced = text
                    .matches('«')
                    .count()
                    .saturating_sub(restored.matches('«').count());
                log::info!("auto-restore: {replaced} token(s)");
                let _ = app_handle
                    .notification()
                    .builder()
                    .title("Streichzeug — automatisch zurückübersetzt")
                    .body(format!("{replaced} Pseudonym(e) durch Originale ersetzt."))
                    .show();
                return Some(restored);
            }
            None
        });

    let mut watcher = PlatformWatcher::new();
    if let Err(e) = watcher.start(callback) {
        log::warn!("clipboard watcher start failed: {e}");
    }
    // Watcher am App-State festhalten, damit er nicht gedroppt wird.
    app.manage(std::sync::Mutex::new(watcher));
}
