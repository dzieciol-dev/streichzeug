//! Cross-Platform Clipboard-Watcher — Schnittstelle.
//!
//! Die hier definierten Typen sind das gemeinsame Interface; die echten
//! Implementierungen liegen in den plattform-spezifischen Submodulen
//! `windows_impl` und `macos_impl`. Per `#[cfg(target_os)]` wird genau eine
//! davon eingebunden.
//!
//! # Plattform-Verhalten
//!
//! - **Windows**: aktuell `GetClipboardSequenceNumber`-Polling im festen
//!   250-ms-Takt (nicht das geplante Push-API via `AddClipboardFormatListener`
//!   / `WM_CLIPBOARDUPDATE`).
//! - **macOS**: `NSPasteboard.changeCount`-Polling (kein Push-API), ebenfalls
//!   im festen 250-ms-Takt.
//!
//! TODO(adaptives-Polling): Geplant, aber **nicht** implementiert, war eine
//! adaptive Frequenz (z. B. 200 ms wenn eine LLM-App im Vordergrund ist,
//! 1000 ms sonst, gegen Battery-Drain auf dem MacBook Air). Beide Impls pollen
//! derzeit konstant mit 250 ms — separates Vorhaben.
//!
//! Beide rufen den Callback mit dem Plain-Text-Inhalt auf, sobald sich das
//! Clipboard ändert. Self-Loop wird über Sequence-Number-Tracking vermieden:
//! vor jedem eigenen `SetClipboardData` den Counter merken und beim nächsten
//! Event vergleichen.

use std::sync::Arc;

/// Plattform-spezifischer Fehler beim Clipboard-Zugriff.
///
/// Aktuell werden beide Varianten noch nicht produziert (die Polling-Loop
/// schluckt Read-Fehler intern), sind aber als öffentliche API für künftige
/// Konsumenten verfügbar. `#[allow(dead_code)]` deshalb.
#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    /// OS-Aufruf ist fehlgeschlagen (Win32-Error, NSError, etc.).
    #[error("platform error: {0}")]
    Platform(String),

    /// Clipboard enthält keinen Text (z. B. nur Bild oder leer).
    #[error("no text content")]
    NoText,
}

/// Callback-Signatur für Clipboard-Änderungen.
///
/// `Arc<dyn Fn>` weil der Watcher den Callback an einen Worker-Thread oder
/// eine Polling-Loop weiterreicht — er muss `Send + Sync + 'static` sein.
///
/// **Rückgabewert:** wenn `Some(text)`, schreibt der Watcher den Text als
/// neuen Clipboard-Inhalt zurück (für Auto-Replace im Desktop-LLM-App-Flow).
/// Bei `None` macht der Watcher nichts weiter — die Callback hat selbst
/// entschieden, dass der Original-Inhalt bleiben soll.
pub type ClipboardCallback = Arc<dyn Fn(String) -> Option<String> + Send + Sync + 'static>;

/// Plattform-spezifischer Watcher. Eine Instanz hält den nötigen State
/// (Polling-Thread, AtomicBool flag, etc.); `start` registriert den Callback,
/// `stop` räumt auf.
///
/// `stop` wird heute noch nicht aufgerufen — die App lässt den Watcher beim
/// Quit einfach mit dem Prozess sterben. Für sauberes Shutdown (Phase 2)
/// nötig, daher Teil des Traits.
pub trait ClipboardWatcher: Send + Sync {
    fn start(&mut self, callback: ClipboardCallback) -> Result<(), ClipboardError>;
    #[allow(dead_code)]
    fn stop(&mut self);
}

#[cfg(target_os = "windows")]
mod windows_impl;
#[cfg(target_os = "windows")]
pub use windows_impl::WindowsClipboardWatcher as PlatformWatcher;

#[cfg(target_os = "macos")]
mod macos_impl;
#[cfg(target_os = "macos")]
pub use macos_impl::MacosClipboardWatcher as PlatformWatcher;

// =================================================================== Cross-Platform Helpers
//
// Direkter Read/Write des System-Clipboards, unabhängig vom Watcher-Polling.
// Wird primär vom [`crate::hotkey`]-Modul verwendet, damit das Smart-Paste
// auf User-Hotkey-Druck synchron lesen/schreiben kann.

/// Liefert den aktuellen Plain-Text-Inhalt des System-Clipboards.
/// Gibt `None` zurück bei leerem oder nicht-textuellem Clipboard, sowie bei
/// OS-Fehlern (z. B. kurzzeitige Sperre durch andere Apps).
pub fn read_clipboard_text() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        clipboard_win::get_clipboard_string().ok()
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: read_text liest die NSPasteboard.generalPasteboard. Threading-
        // sicher für Reads (siehe macos_impl Modul-Doc).
        unsafe { macos_impl::read_text() }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

/// Schreibt `text` als neuen Plain-Text-Inhalt ins System-Clipboard.
/// Liefert `Err` als String bei OS-Fehlern.
pub fn write_clipboard_text(text: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        clipboard_win::set_clipboard_string(text).map_err(|e| format!("{e:?}"))
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: write_text macht clearContents + setString — kein Aliasing,
        // kein Lifetime-Trick.
        unsafe { macos_impl::write_text(text) };
        Ok(())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = text;
        Err("unsupported platform".to_string())
    }
}

// Linux/Unbekannt: kein Watcher — Detection läuft dann nur über die UI.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod stub_impl {
    use super::*;

    pub struct StubWatcher;

    impl StubWatcher {
        // `new()` muss API-konsistent zur Win/Mac-Impl sein, damit
        // `PlatformWatcher::new()` in main.rs auf jeder Plattform compiliert.
        pub fn new() -> Self {
            Self
        }
    }

    impl Default for StubWatcher {
        fn default() -> Self { Self::new() }
    }

    impl ClipboardWatcher for StubWatcher {
        fn start(&mut self, _cb: ClipboardCallback) -> Result<(), ClipboardError> {
            Err(ClipboardError::Platform("unsupported platform".into()))
        }
        fn stop(&mut self) {}
    }
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub use stub_impl::StubWatcher as PlatformWatcher;
