//! macOS Clipboard-Watcher via NSPasteboard.changeCount-Polling.
//!
//! macOS hat keine Push-API für Pasteboard-Änderungen — der einzige Weg ist
//! `changeCount` periodisch abzufragen. Wir pollen mit 250 ms; das ist
//! User-spürbar nicht von einem Push zu unterscheiden, und der CPU-Cost
//! eines `changeCount`-Calls ist vernachlässigbar.
//!
//! # Trigger-Logik
//!
//! Identisch zum Windows-Watcher: Callback feuert wenn (a) eine bekannte
//! LLM-App im Vordergrund ist **und** (b) der Pasteboard-Inhalt seit der
//! letzten Meldung nicht erneut verarbeitet wurde. Damit greift die App
//! sowohl beim Copy-innerhalb-der-LLM-App als auch beim Switch-zur-LLM-App-
//! nach-Copy-anderswo.
//!
//! Nebeneffekt: erst Foreground-Bundle-ID checken, dann Pasteboard lesen
//! → unter macOS Sonoma kein Permission-Prompt für Source-Apps, die wir
//! gar nicht behandeln.

use super::{ClipboardCallback, ClipboardError, ClipboardWatcher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use objc2_app_kit::NSPasteboard;
use objc2_foundation::NSString;

pub struct MacosClipboardWatcher {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MacosClipboardWatcher {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }
}

impl Default for MacosClipboardWatcher {
    fn default() -> Self { Self::new() }
}

impl ClipboardWatcher for MacosClipboardWatcher {
    fn start(&mut self, callback: ClipboardCallback) -> Result<(), ClipboardError> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(()); // bereits aktiv
        }
        let running = self.running.clone();

        let handle = thread::spawn(move || {
            // Aktuelles changeCount als Baseline — was vor App-Start drin lag,
            // gilt als bereits verarbeitet.
            let mut last_processed_count: isize = unsafe { current_change_count() };
            log::info!("macOS clipboard watcher started (initial count={last_processed_count})");

            while running.load(Ordering::SeqCst) {
                let count = unsafe { current_change_count() };
                let fg = crate::foreground::current_process_name();
                let in_llm = fg.as_deref().map(crate::foreground::is_llm_app).unwrap_or(false);
                let unprocessed = count != last_processed_count;

                if in_llm && unprocessed {
                    let text = unsafe { read_text() };
                    match text {
                        Some(t) if !t.is_empty() => {
                            log::debug!(
                                "clipboard watcher firing: fg={:?}, count {} -> {}",
                                fg, last_processed_count, count
                            );
                            // Callback liefert ggf. Ersatztext → Auto-Replace.
                            if let Some(replacement) = callback(t) {
                                unsafe { write_text(&replacement) };
                                // Nach dem Write hat sich changeCount erhöht.
                                // Diesen neuen Stand als verarbeitet merken,
                                // sonst feuert der nächste Tick wegen unseres
                                // eigenen Writes.
                                let new_count = unsafe { current_change_count() };
                                last_processed_count = new_count;
                                log::debug!("auto-replaced clipboard, new count={new_count}");
                            } else {
                                last_processed_count = count;
                            }
                        }
                        _ => {
                            // Leer oder unlesbar — als „verarbeitet" markieren.
                            last_processed_count = count;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(250));
            }
            log::info!("macOS clipboard watcher stopped");
        });
        self.thread = Some(handle);
        Ok(())
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Aktuelles `changeCount` der allgemeinen Pasteboard. Reiner Counter-Read,
/// löst auf macOS Sonoma **keinen** Permission-Prompt aus.
///
/// # Safety
///
/// Aufrufer garantiert, dass die Funktion auf einem Thread läuft, von dem
/// objc2-Calls erlaubt sind. NSPasteboard ist threading-mäßig „nicht-thread-safe
/// für Mutation", aber `generalPasteboard()` + Read-Calls sind in der Praxis
/// von beliebigen Threads sicher (siehe Apple-Dev-Forum-Diskussionen).
unsafe fn current_change_count() -> isize {
    let pb = NSPasteboard::generalPasteboard();
    pb.changeCount()
}

/// Liest den Plain-Text-Inhalt der allgemeinen Pasteboard. **Erst** aufrufen,
/// nachdem [`current_change_count`] eine Änderung gemeldet hat, sonst löst
/// das unnötig Sonoma-Permission-Prompts pro Source-App aus.
///
/// # Safety
///
/// Gleiche Threading-Annahme wie [`current_change_count`].
pub(super) unsafe fn read_text() -> Option<String> {
    let pb = NSPasteboard::generalPasteboard();
    // UTI für reinen Text. Funktioniert auf 10.6+; ab Big Sur ist der
    // moderne Weg `NSPasteboardTypeString` (= dieselbe UTI).
    let ns_type = NSString::from_str("public.utf8-plain-text");
    pb.stringForType(&ns_type).map(|s| s.to_string())
}

/// Schreibt `text` als neuen Plain-Text-Inhalt ins Pasteboard. Das
/// `clearContents:` vor `setString:` ist nötig, sonst überlagert NSPasteboard
/// nur den Text-Type — andere Formate (HTML/RTF) blieben aus dem alten Inhalt
/// erhalten.
///
/// # Safety
///
/// Gleiche Threading-Annahme wie [`current_change_count`].
pub(super) unsafe fn write_text(text: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let ns_type = NSString::from_str("public.utf8-plain-text");
    let ns_text = NSString::from_str(text);
    let _ = pb.setString_forType(&ns_text, &ns_type);
}
