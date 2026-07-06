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
mod imaging; // Stufe 3: Bild-Pipeline (Boxen-Mapping, PNG-Redaction — reine Logik)
mod ner; // Layer-3 NER (optional, feature = "ner")
mod ocr; // Stufe 3: lokale OS-Texterkennung (Apple Vision / Windows.Media.Ocr)
mod richtext; // Stufe 2: HTML-Sanitizing + Finding-Mapping (reine Logik)
mod secrets; // HMAC-Master-Secret-Verwaltung
mod settings; // User-Settings (Hotkey, Auto-Detection-Toggle)
mod stage; // Schwärz-Bühne: Capture-Flow des sichtbaren zweiten Workflows
mod storage; // SQLite-basierter Mapping-Store
mod tokens;
mod widget; // Schwebendes Mini-Widget (nicht-aktivierender Klick-Einstieg) // Token-Generierung «T_<hash>»

use clipboard::{ClipboardWatcher, PlatformWatcher};
use detection::Finding;
use settings::Settings;
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
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
    ner_status_json()
}

/// Frontend-Command: Erweiterte Erkennung an-/abschalten. Persistiert das
/// Setting, zieht das Laufzeit-Gate nach und lädt die Engine bei Bedarf
/// SOFORT — kein Neustart nötig (der Tray-Toggle, der nur das Setting
/// flippte und einen Neustart verlangte, ist ersatzlos entfallen).
/// `async`, damit der blockierende Engine-Load (~300 ms + ORT-Init) nicht
/// auf dem Main-Thread läuft. Liefert den frischen NER-Status.
#[tauri::command]
async fn set_ner_enabled(enabled: bool) -> Result<serde_json::Value, String> {
    let mut s = Settings::load();
    s.enable_ner = enabled;
    s.save().map_err(|e| e.to_string())?;
    ner::set_enabled(enabled);
    if enabled {
        let ready = ner::ensure_loaded();
        log::info!("ner: über die App aktiviert, engine_ready={ready}");
    } else {
        log::info!("ner: über die App deaktiviert (wirkt sofort)");
    }
    Ok(ner_status_json())
}

fn ner_status_json() -> serde_json::Value {
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
    new_settings.save().map_err(|e| e.to_string())?;
    // Laufzeit-Gate der NER-Schicht mitziehen — Settings-Änderungen (auch
    // aus dem Onboarding) wirken damit sofort, ohne Neustart.
    ner::set_enabled(new_settings.enable_ner);
    Ok(())
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

// ------------------------------------------------------------ Ablage (Schwärz-Bühne)
//
// Commands der Ablage. Die Bühne selbst schreibt Einträge über
// `storage::stash_insert`; das Frontend liest/kopiert/löscht über diese
// Commands. Alle geben nur die geschwärzte Fassung heraus — nie Originaltext.

/// Frontend-Command: alle Ablage-Einträge (neueste zuerst), ohne Volltext.
#[tauri::command]
fn stash_list() -> Vec<storage::StashMeta> {
    storage::stash_list()
}

/// Frontend-Command: geschwärzter Volltext eines Eintrags.
#[tauri::command]
fn stash_get_text(id: i64) -> Result<String, String> {
    storage::stash_get_text(id)
}

/// Frontend-Command: „Nochmal kopieren" — Volltext ins System-Clipboard.
#[tauri::command]
fn stash_copy(id: i64) -> Result<(), String> {
    storage::stash_copy(id)
}

/// Frontend-Command: einzelnen Eintrag löschen.
#[tauri::command]
fn stash_delete(id: i64) -> Result<(), String> {
    storage::stash_delete(id)
}

/// Frontend-Command: Ablage leeren, liefert Anzahl gelöschter Einträge.
#[tauri::command]
fn stash_clear() -> usize {
    storage::stash_clear()
}

/// Frontend-Command: Bühne mit dem aktuellen Clipboard-Inhalt starten —
/// klickbarer Einstieg über den Button im Fenster (Dock-Klick → Button),
/// für alle, die weder Hotkey noch Tray nutzen wollen/können.
///
/// Läuft auf einem eigenen Thread: seit Stufe 3 kann hier ein Bild im
/// Clipboard liegen, dessen Decode + OCR + Redaction sekundenlang dauern —
/// synchrone Tauri-Commands laufen sonst auf dem Main-Thread und frieren
/// die UI ein. Kein enigo im Pfad (keine Main-Thread-Pflicht).
#[tauri::command]
fn stage_clipboard(app: tauri::AppHandle) {
    std::thread::spawn(move || stage::capture_from_clipboard(&app));
}

/// Frontend-Command: Bühne mit per Drag & Drop hereingezogenem Text starten.
/// Der Text kommt aus dem HTML5-Drop-Event — die Markierung wandert damit
/// OHNE Clipboard und ohne synthetische Tastendrücke in die App.
#[tauri::command]
fn stage_text(app: tauri::AppHandle, text: String) {
    stage::capture_from_text(&app, text);
}

/// Frontend-Command: Bühne mit einer per Drag & Drop hereingezogenen
/// Bilddatei starten (Stufe 3). Die Bytes kommen als Raw-Body (`invoke`
/// mit `Uint8Array`) — kein JSON-Umweg für Megabyte-Payloads.
///
/// Die Pipeline (Decode, OCR, Redaction, base64) läuft auf einem eigenen
/// Thread — synchrone Tauri-Commands laufen auf dem Main-Thread, und ein
/// großer Scan würde die UI sonst sekundenlang einfrieren. Kein enigo im
/// Pfad (keine Main-Thread-Pflicht; Vision/WinRT sind thread-safe).
#[tauri::command]
fn stage_image(app: tauri::AppHandle, request: tauri::ipc::Request<'_>) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("stage_image erwartet Raw-Bytes (Uint8Array)".into());
    };
    let bytes = bytes.clone();
    std::thread::spawn(move || stage::capture_from_image(&app, bytes));
    Ok(())
}

/// Widget-Command: voller Capture-Flow inklusive synthetischem Strg+C.
///
/// MUSS auf dem Main-Thread laufen: enigo nutzt auf macOS die Text-Services-
/// APIs (`TSMGetInputSourceProperty`), die per `dispatch_assert_queue` die
/// Main-Queue erzwingen — von einem Tokio-Worker aus crasht das mit SIGTRAP
/// (Crash-Report 2026-07-05, macOS 26). Deshalb kein `async`, sondern
/// explizites Einreihen auf den Main-Thread — derselbe Ausführungskontext
/// wie der Global-Shortcut-Handler, der seit der Beta stabil läuft. Das
/// Polling blockiert den Main-Thread dabei kurz (wie beim Hotkey-Pfad).
#[tauri::command]
fn stage_capture(app: tauri::AppHandle) {
    let handle = app.clone();
    if let Err(e) = app.run_on_main_thread(move || stage::capture(&handle)) {
        log::warn!("stage_capture: run_on_main_thread failed: {e}");
    }
}

/// Frontend-Command: Widget ein-/ausblenden und die Wahl persistieren.
/// Wirkt sofort (kein Restart) — das Fenster existiert versteckt weiter.
#[tauri::command]
fn set_widget_visible(app: tauri::AppHandle, visible: bool) -> Result<(), String> {
    widget::set_visible(&app, visible)?;
    let mut s = Settings::load();
    s.show_widget = visible;
    s.save().map_err(|e| e.to_string())
}

/// Widget-Command: natives Kontextmenü am Widget zeigen (Rechtsklick).
/// Menü-Aufbau + Popup müssen auf dem Main-Thread laufen; die Auswahl landet
/// im globalen `on_menu_event`-Handler (IDs `widget_*`, siehe `setup()`).
#[tauri::command]
fn widget_menu(app: tauri::AppHandle) {
    use tauri::menu::{ContextMenu, MenuBuilder, MenuItem};
    let handle = app.clone();
    let result = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window(widget::WIDGET_LABEL) else {
            return;
        };
        let open_item = MenuItem::with_id(
            &handle,
            "widget_open_app",
            "Streichzeug öffnen",
            true,
            None::<&str>,
        );
        let hide_item = MenuItem::with_id(
            &handle,
            "widget_hide",
            "Widget ausblenden",
            true,
            None::<&str>,
        );
        let (Ok(open_item), Ok(hide_item)) = (open_item, hide_item) else {
            log::warn!("widget_menu: Menü-Items nicht erstellbar");
            return;
        };
        let menu = MenuBuilder::new(&handle)
            .item(&open_item)
            .separator()
            .item(&hide_item)
            .build();
        match menu {
            Ok(menu) => {
                if let Err(e) = menu.popup(window.as_ref().window()) {
                    log::warn!("widget_menu: popup failed: {e}");
                }
            }
            Err(e) => log::warn!("widget_menu: build failed: {e}"),
        }
    });
    if let Err(e) = result {
        log::warn!("widget_menu: run_on_main_thread failed: {e}");
    }
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
    // Laufzeit-Gate der NER-Schicht aus dem Setting spiegeln — classify()
    // prüft das Gate pro Aufruf (kein Datei-I/O im Hot-Path).
    ner::set_enabled(cfg.enable_ner);
    if cfg.enable_ner {
        // Eager-Init beim Start, damit der erste Hotkey-Druck nicht
        // die Modell-Lade-Latenz absorbieren muss (~300 ms typisch).
        let ready = ner::ensure_loaded();
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

    // Capture-Hotkey der Schwärz-Bühne für die Handler-Weiche vorparsen.
    // Bei leerem String / Kollision mit dem Smart-Paste-Hotkey / nicht
    // parsebarem Accelerator bleibt die Weiche `None` — der Handler routet dann
    // ausschließlich zum bestehenden Smart-Paste-Pfad, der so nie brechen kann.
    // Die eigentliche Registrierung (inkl. Logging der drei Fälle) macht
    // `stage::register_stage_hotkey` weiter unten in `setup()`.
    let stage_shortcut: Option<Shortcut> = {
        let s = &cfg.stage_hotkey;
        if s.is_empty() || s == &cfg.hotkey {
            None
        } else {
            s.parse::<Shortcut>().ok()
        }
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if event.state() != ShortcutState::Pressed {
                        return;
                    }
                    // Weiche: der Capture-Hotkey öffnet die Bühne, jeder andere
                    // (also der Smart-Paste-Hotkey) läuft in den bestehenden Pfad.
                    match &stage_shortcut {
                        Some(sc) if shortcut == sc => stage::capture(app),
                        _ => hotkey::handle(app),
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
            set_ner_enabled,
            download_ner_model,
            open_log_folder,
            copy_log_to_clipboard,
            get_version,
            get_storage_status,
            clear_all_mappings,
            finalize_secret_setup,
            stash_list,
            stash_get_text,
            stash_copy,
            stash_delete,
            stash_clear,
            stage_clipboard,
            stage_text,
            stage_image,
            stage_capture,
            set_widget_visible,
            widget_menu,
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
            // Der frühere NER-Toggle lebt jetzt im „Erkennung"-Tab der App —
            // dort mit Modell-Download und Sofort-Wirkung statt Setting-Flip
            // plus Neustart-Bitte (Beta-Befund: Haken ohne Modell tat nichts).
            let show_item =
                MenuItem::with_id(app, "show", "Fenster anzeigen", true, None::<&str>)?;
            // Klickbarer Bühnen-Einstieg ohne Hotkey: schwärzt den aktuellen
            // Clipboard-Inhalt (KEIN synthetisches Strg+C — der Menü-Klick
            // hat der Quell-App bereits den Fokus genommen, die Markierung
            // wäre nicht mehr abholbar). Flow: Text kopieren → Tray → Klick.
            let stage_item = MenuItem::with_id(
                app,
                "stage_clipboard",
                "Zwischenablage schwärzen",
                true,
                None::<&str>,
            )?;
            let log_item =
                MenuItem::with_id(app, "open_log", "Log-Ordner öffnen", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Beenden", true, None::<&str>)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let sep3 = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(
                app,
                &[
                    &auto_item,
                    &sep1,
                    &stage_item,
                    &show_item,
                    &log_item,
                    &sep2,
                    &sep3,
                    &quit_item,
                ],
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
            let _tray = tray_builder
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "stage_clipboard" => stage::capture_from_clipboard(app),
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

            // ----------------------------------- Capture-Hotkey (Schwärz-Bühne)
            // Zweiter, paralleler Hotkey. Robustheit liegt komplett in
            // `register_stage_hotkey`: leer → aus, == Smart-Paste-Hotkey → aus,
            // nicht registrierbar → aus. In allen drei Fällen bleibt der oben
            // registrierte Smart-Paste-Hotkey unberührt, die App startet normal.
            stage::register_stage_hotkey(app.handle(), &cfg.stage_hotkey, &cfg.hotkey);

            // ----------------------------------- Schwebendes Widget (macOS)
            // Nicht-aktivierender Klick-Einstieg in die Bühne. Fehler werden
            // in widget::init nur geloggt — Komfort-Feature, darf den Start
            // nicht verhindern.
            widget::init(app.handle(), &cfg);

            // Auswahl aus dem Widget-Kontextmenü (Rechtsklick, siehe Command
            // `widget_menu`). Läuft über den App-weiten Menü-Handler — die
            // Tray-Menü-IDs sind disjunkt, es kann nichts doppelt feuern.
            app.on_menu_event(|app, event| match event.id().as_ref() {
                "widget_hide" => {
                    if let Err(e) = widget::set_visible(app, false) {
                        log::warn!("widget_hide: {e}");
                    }
                    let mut s = Settings::load();
                    s.show_widget = false;
                    if let Err(e) = s.save() {
                        log::warn!("widget_hide: Setting nicht gespeichert: {e}");
                    }
                }
                "widget_open_app" => {
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.show();
                        let _ = w.unminimize();
                        let _ = w.set_focus();
                    }
                }
                _ => {}
            });

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
            // Session-only-Ablage: beim App-Beenden die Schwärz-Bühnen-Ablage
            // leeren, falls der User das gewählt hat. Bewusst hier am
            // RunEvent::Exit (nicht beim Fenster-Schließen), damit „Fenster zu,
            // App bleibt im Tray" die Ablage nicht überraschend löscht.
            if let tauri::RunEvent::Exit = event {
                if Settings::load().stash_clear_on_quit {
                    let n = storage::stash_clear();
                    log::info!("stash: cleared {n} entries on quit (stash_clear_on_quit)");
                }
                // Widget-Position gesammelt persistieren (nicht pro
                // Moved-Event — ein Drag feuert Dutzende davon).
                widget::persist_position();
            }

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
