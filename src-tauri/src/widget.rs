//! Schwebendes Mini-Widget — klickbarer Bühnen-Einstieg direkt neben der
//! Arbeits-App.
//!
//! Ein kleines Always-on-top-Fenster mit einem einzigen Button. Der Witz ist,
//! dass es als **nicht-aktivierendes NSPanel** läuft: Ein Klick darauf nimmt
//! der Quell-App (Outlook, Word, Browser) den Fokus NICHT weg — deren
//! Text-Markierung bleibt aktiv, und das synthetische Cmd+C des
//! [`crate::stage`]-Capture-Flows landet weiterhin dort. Erst wenn die Bühne
//! das Ergebnis zeigt, wechselt der Fokus bewusst zu Streichzeug.
//!
//! # Mechanik (macOS)
//!
//! Tauri erzeugt reguläre `NSWindow`s; die `nonactivatingPanel`-Style-Mask
//! wird aber nur von `NSPanel` respektiert. Deshalb wird das fertige Fenster
//! per `object_setClass` zur Laufzeit auf `NSPanel` umgeklassifiziert und
//! anschließend als Floating-Palette konfiguriert. Dieses Vorgehen ist dem
//! Community-Plugin `tauri-nspanel` (github.com/ahkohd/tauri-nspanel,
//! Apache-2.0) nachempfunden — bewusst nachimplementiert statt eingebunden,
//! damit keine fremde Dependency in die Privacy-App einkompiliert wird.
//! Anders als Spotlight-artige Apps brauchen wir **kein** Key-Window
//! (keine Tastatur-Eingabe im Widget): `becomesKeyOnlyIfNeeded` lässt den
//! Button-Klick durch, ohne dass das Panel der Ziel-App irgendetwas nimmt.
//! `object_setClass` auf eine Klasse ohne zusätzliche Ivars (NSPanel fügt
//! gegenüber NSWindow keine hinzu) ist derselbe erprobte Trick wie im Plugin.
//!
//! # Mechanik (Windows)
//!
//! Dasselbe Ziel, anderes Werkzeug: Am Raw-HWND wird der Extended-Style um
//! `WS_EX_NOACTIVATE` ergänzt — Klicks aktivieren das Fenster nicht, die
//! Ziel-App behält Fokus und Markierung. `WS_EX_TOOLWINDOW` hält es
//! zusätzlich aus Alt+Tab heraus (skip_taskbar deckt nur die Taskbar ab).
//! Transparenz kommt hier direkt von Tauri (`.transparent(true)` — das
//! Private-API-Gate gilt nur auf macOS).
//!
//! # Andere Plattformen
//!
//! Linux: kein Watcher, kein Widget (Setting wird ignoriert, nur geloggt).

use std::sync::Mutex;

use tauri::{AppHandle, Manager};

use crate::settings::Settings;

/// Fenster-Label des Widgets — muss in `capabilities/default.json` stehen,
/// damit die Webview `invoke` aufrufen darf.
pub const WIDGET_LABEL: &str = "widget";

/// Kantenlänge des quadratischen Widget-Fensters (logische Pixel).
/// Nur auf Plattformen mit Widget-Implementierung — sonst wäre die
/// Konstante unused (CI verbietet Warnings).
#[cfg(any(target_os = "macos", target_os = "windows"))]
const WIDGET_SIZE: f64 = 56.0;

/// Zuletzt beobachtete Fensterposition (logische Pixel). Wird bei jedem
/// Moved-Event aktualisiert und erst beim App-Exit in die Settings
/// geschrieben — ein Drag feuert Dutzende Events, die sollen nicht je einen
/// Datei-Write auslösen.
static LAST_POSITION: Mutex<Option<(f64, f64)>> = Mutex::new(None);

/// Erzeugt das Widget-Fenster (versteckt), konvertiert es zum
/// nicht-aktivierenden Panel und zeigt es, wenn das Setting an ist.
/// Wird aus `setup()` gerufen; alle Fehlerpfade nur loggen — das Widget ist
/// Komfort, es darf den App-Start nie verhindern.
pub fn init(app: &AppHandle, cfg: &Settings) {
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = init_macos(app, cfg) {
            log::warn!("widget: Initialisierung fehlgeschlagen ({e}) — Widget aus");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Err(e) = init_windows(app, cfg) {
            log::warn!("widget: Initialisierung fehlgeschlagen ({e}) — Widget aus");
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = app;
        if cfg.show_widget {
            log::info!("widget: auf dieser Plattform nicht verfügbar (macOS/Windows)");
        }
    }
}

/// Zeigt bzw. versteckt das Widget-Fenster. `Err`, wenn das Widget auf der
/// Plattform nicht existiert.
pub fn set_visible(app: &AppHandle, visible: bool) -> Result<(), String> {
    let Some(window) = app.get_webview_window(WIDGET_LABEL) else {
        return Err("Widget ist auf dieser Plattform nicht verfügbar (macOS/Windows)".into());
    };
    let result = if visible {
        window.show()
    } else {
        window.hide()
    };
    result.map_err(|e| e.to_string())
}

/// Schreibt die zuletzt beobachtete Widget-Position in die Settings. Wird am
/// App-Exit gerufen (gesammelt statt pro Moved-Event, s. [`LAST_POSITION`]).
pub fn persist_position() {
    let Some(pos) = *LAST_POSITION.lock().expect("widget position lock") else {
        return;
    };
    let mut s = Settings::load();
    if s.widget_position == Some(pos) {
        return;
    }
    s.widget_position = Some(pos);
    if let Err(e) = s.save() {
        log::warn!("widget: Position nicht gespeichert: {e}");
    }
}

/// Windows-Pendant zu [`init_macos`]: reguläres Tauri-Fenster, danach wird
/// der Extended-Style am Raw-HWND um `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`
/// ergänzt. Klicks aktivieren das Fenster damit nicht — die Markierung der
/// Quell-App bleibt bestehen und der volle Capture-Flow (synthetisches
/// Strg+C) funktioniert wie beim macOS-Panel.
#[cfg(target_os = "windows")]
fn init_windows(app: &AppHandle, cfg: &Settings) -> Result<(), String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder, WindowEvent};

    let mut builder = WebviewWindowBuilder::new(
        app,
        WIDGET_LABEL,
        WebviewUrl::App("index.html?widget=1".into()),
    )
    .title("Streichzeug")
    .inner_size(WIDGET_SIZE, WIDGET_SIZE)
    .resizable(false)
    .decorations(false)
    // Anders als macOS braucht Windows kein Private-API-Feature für
    // Fenster-Transparenz — die runde Form kommt aus dem Widget-CSS.
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .visible(false)
    .accept_first_mouse(true)
    .shadow(false);

    if let Some((x, y)) = cfg.widget_position {
        builder = builder.position(x, y);
    }

    let window = builder.build().map_err(|e| e.to_string())?;

    apply_noactivate_style(&window)?;

    // Position für den nächsten Start merken (Schreiben erst am Exit).
    window.on_window_event(move |event| {
        if let WindowEvent::Moved(pos) = event {
            let logical = pos.to_logical::<f64>(1.0);
            *LAST_POSITION.lock().expect("widget position lock") = Some((logical.x, logical.y));
        }
    });

    if cfg.show_widget {
        let _ = window.show();
    }
    log::info!(
        "widget: initialisiert (sichtbar={}, position={:?})",
        cfg.show_widget,
        cfg.widget_position
    );
    Ok(())
}

/// Setzt `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` am Raw-HWND.
///
/// - `WS_EX_NOACTIVATE`: Maus-Klicks aktivieren das Fenster nicht — das
///   Windows-Gegenstück zur `nonactivatingPanel`-Mask des NSPanel.
/// - `WS_EX_TOOLWINDOW`: hält das Widget aus Alt+Tab heraus;
///   `skip_taskbar` deckt nur die Taskbar-Schaltfläche ab.
///
/// Tauris `hwnd()` liefert den HWND-Typ seiner eigenen windows-Crate-Version;
/// über den rohen Pointer (`.0`) wird er in unsere Version überführt —
/// beide sind `#[repr(transparent)]` über `*mut c_void`.
#[cfg(target_os = "windows")]
fn apply_noactivate_style(window: &tauri::WebviewWindow) -> Result<(), String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };

    let raw = window.hwnd().map_err(|e| e.to_string())?.0;
    let hwnd = HWND(raw);

    // SAFETY: hwnd stammt aus einem frisch gebauten, lebenden Tauri-Fenster;
    // Get/SetWindowLongPtrW auf GWL_EXSTYLE sind für eigene Fenster des
    // Prozesses dokumentiert sicher.
    unsafe {
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_style =
            ex_style | (WS_EX_NOACTIVATE.0 as isize) | (WS_EX_TOOLWINDOW.0 as isize);
        if SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style) == 0 && ex_style != 0 {
            // Rückgabe 0 kann „vorheriger Wert war 0" ODER Fehler bedeuten —
            // nur im eindeutigen Fehlerfall abbrechen.
            return Err("SetWindowLongPtrW(GWL_EXSTYLE) fehlgeschlagen".into());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn init_macos(app: &AppHandle, cfg: &Settings) -> Result<(), String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder, WindowEvent};

    let mut builder = WebviewWindowBuilder::new(
        app,
        WIDGET_LABEL,
        WebviewUrl::App("index.html?widget=1".into()),
    )
    .title("Streichzeug")
    .inner_size(WIDGET_SIZE, WIDGET_SIZE)
    .resizable(false)
    .decorations(false)
    // Kein .transparent(true): das ist in Tauri hinter dem Feature
    // `macos-private-api` gegattert. Die Transparenz setzen wir selbst in
    // `convert_to_nonactivating_panel` (setOpaque/clearColor/KVC).
    .always_on_top(true)
    .skip_taskbar(true)
    .visible(false)
    // Erster Klick auf das (nie aktive) Panel soll sofort den Button
    // treffen, nicht erst das Fenster „anwählen".
    .accept_first_mouse(true)
    .shadow(false);

    if let Some((x, y)) = cfg.widget_position {
        builder = builder.position(x, y);
    }

    let window = builder.build().map_err(|e| e.to_string())?;

    convert_to_nonactivating_panel(&window)?;

    // Position für den nächsten Start merken (Schreiben erst am Exit).
    window.on_window_event(move |event| {
        if let WindowEvent::Moved(pos) = event {
            let logical = pos.to_logical::<f64>(1.0);
            *LAST_POSITION.lock().expect("widget position lock") = Some((logical.x, logical.y));
        }
    });

    if cfg.show_widget {
        let _ = window.show();
    }
    log::info!(
        "widget: initialisiert (sichtbar={}, position={:?})",
        cfg.show_widget,
        cfg.widget_position
    );
    Ok(())
}

/// Klassifiziert das NSWindow zur Laufzeit auf `NSPanel` um und setzt die
/// Palette-Flags. Muss auf dem Main-Thread laufen (setup() tut das).
#[cfg(target_os = "macos")]
fn convert_to_nonactivating_panel(window: &tauri::WebviewWindow) -> Result<(), String> {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};

    // AppKit-Konstanten (stabil dokumentiert):
    // NSWindowStyleMaskNonactivatingPanel — nur von NSPanel respektiert.
    const STYLE_NONACTIVATING_PANEL: usize = 1 << 7;
    // NSWindowCollectionBehavior: CanJoinAllSpaces | FullScreenAuxiliary —
    // das Widget soll auf jedem Space und über Vollbild-Apps sichtbar sein.
    const COLLECTION_ALL_SPACES_AUX: usize = (1 << 0) | (1 << 8);
    // NSStatusWindowLevel — hoch genug, um über normalen Fenstern und
    // Vollbild-Toolbars zu schweben.
    const LEVEL_STATUS: isize = 25;

    let ns_window = window.ns_window().map_err(|e| e.to_string())? as *mut AnyObject;
    if ns_window.is_null() {
        return Err("ns_window ist null".into());
    }

    unsafe {
        // Klassen-Swap NSWindow → NSPanel: NSPanel fügt keine Instanz-
        // variablen hinzu, der Swap auf der lebenden Instanz ist daher
        // sicher (gleicher Mechanismus wie in tauri-nspanel).
        objc2::ffi::object_setClass(
            ns_window.cast(),
            (class!(NSPanel) as *const objc2::runtime::AnyClass).cast(),
        );

        let current_mask: usize = msg_send![&*ns_window, styleMask];
        let _: () = msg_send![
            &*ns_window,
            setStyleMask: current_mask | STYLE_NONACTIVATING_PANEL
        ];
        // Kein Key-Window-Bedarf: der Button braucht nur Mouse-Events. So
        // bleibt selbst das Key-Window der Ziel-App unangetastet.
        let _: () = msg_send![&*ns_window, setBecomesKeyOnlyIfNeeded: true];
        let _: () = msg_send![&*ns_window, setFloatingPanel: true];
        let _: () = msg_send![&*ns_window, setHidesOnDeactivate: false];
        let _: () = msg_send![&*ns_window, setLevel: LEVEL_STATUS];
        let _: () = msg_send![&*ns_window, setCollectionBehavior: COLLECTION_ALL_SPACES_AUX];

        // Fenster-Transparenz ohne das tauri-Feature `macos-private-api`:
        // Fensterhintergrund auf clearColor, Deckung aus — die runde Form
        // kommt komplett aus dem CSS des Widget-HTML.
        let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![&*ns_window, setOpaque: false];
        let _: () = msg_send![&*ns_window, setBackgroundColor: clear];
    }

    // Die WKWebView selbst zeichnet sonst einen weißen Hintergrund. Der
    // KVC-Schalter `drawsBackground` ist der etablierte Weg ohne Private-
    // API-Linkage; schlägt er fehl, bleibt das Widget eckig-weiß — Fehler
    // nur loggen.
    let result = window.with_webview(|webview| {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        unsafe {
            let wk: *mut AnyObject = webview.inner().cast();
            let no: *mut AnyObject = msg_send![class!(NSNumber), numberWithBool: false];
            let key = objc2_foundation::NSString::from_str("drawsBackground");
            let _: () = msg_send![&*wk, setValue: no, forKey: &*key];
        }
    });
    if let Err(e) = result {
        log::warn!("widget: Webview-Transparenz nicht gesetzt: {e}");
    }

    Ok(())
}
