//! Windows Clipboard-Watcher via Sequence-Number-Polling.
//!
//! Statt `WM_CLIPBOARDUPDATE` + Message-Only-Window: simple Polling-Schleife
//! mit [`GetClipboardSequenceNumber`]. Vorteile: kein versteckter HWND,
//! keine `unsafe extern "system"`-WndProc, kein HWND-Send-Problem. Nachteile:
//! 250 ms Latenz statt Push (akzeptabel — User-Input vs. CPU-Polling).
//!
//! Clipboard-Read übernimmt das `clipboard-win`-Crate (es macht
//! `OpenClipboard`/`GlobalLock`/`CloseClipboard` mit Retry-Loop).
//!
//! # Trigger-Logik
//!
//! Der Callback feuert wenn **beide** Bedingungen gleichzeitig erfüllt sind:
//!
//! 1. Foreground-App ist ein bekannter LLM-Client (siehe [`crate::foreground`])
//! 2. Clipboard hat einen Inhalt, den wir in dieser Session noch nicht
//!    gemeldet haben (Vergleich via `GetClipboardSequenceNumber`)
//!
//! Damit greift der Auto-Detection-Flow in zwei Szenarien:
//! - User kopiert *innerhalb* der LLM-App → fires direkt
//! - User kopiert woanders (Outlook/Word/Edge) und switcht dann zur LLM-App
//!   → fires beim Foreground-Wechsel, weil die Sequence-Number nach wie vor
//!   „unverarbeitet" ist

use super::{ClipboardCallback, ClipboardError, ClipboardWatcher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;

pub struct WindowsClipboardWatcher {
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WindowsClipboardWatcher {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }
}

impl Default for WindowsClipboardWatcher {
    fn default() -> Self { Self::new() }
}

impl ClipboardWatcher for WindowsClipboardWatcher {
    fn start(&mut self, callback: ClipboardCallback) -> Result<(), ClipboardError> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Ok(()); // bereits aktiv — kein doppelter Watcher
        }
        let running = self.running.clone();

        let handle = thread::spawn(move || {
            // SAFETY: `GetClipboardSequenceNumber()` ist parameterlos, gibt
            // einen `u32` zurück und ist explizit als thread-safe dokumentiert
            // (MSDN: "no clipboard handle is required"). Kein State, kein
            // Aliasing — kanonisch sicher zu wrappen.
            let initial = unsafe { GetClipboardSequenceNumber() };
            // `last_processed_seq` ist die Seq, für die wir den Callback
            // bereits gefeuert haben. Wir setzen sie initial auf den
            // aktuellen Stand — was vor App-Start im Clipboard liegt,
            // ist Geschichte und wird nicht nachgemeldet.
            let mut last_processed_seq = initial;
            log::info!("Windows clipboard watcher started (initial seq={initial})");

            while running.load(Ordering::SeqCst) {
                // SAFETY: dito wie oben — parameterloser Read-Call.
                let seq = unsafe { GetClipboardSequenceNumber() };

                // Beide Bedingungen prüfen — siehe Modul-Doc „Trigger-Logik".
                let fg = crate::foreground::current_process_name();
                let in_llm = fg.as_deref().map(crate::foreground::is_llm_app).unwrap_or(false);
                let clipboard_unprocessed = seq != last_processed_seq;

                if in_llm && clipboard_unprocessed {
                    match clipboard_win::get_clipboard_string() {
                        Ok(text) if !text.is_empty() => {
                            log::debug!(
                                "clipboard watcher firing: fg={:?}, seq {} -> {}",
                                fg, last_processed_seq, seq
                            );
                            // Callback liefert ggf. Ersatztext zurück — dann
                            // schreiben wir den ins Clipboard (Auto-Replace).
                            //
                            // `set_clipboard_string` ist die High-Level-Function
                            // mit Open/Close + Retry-Loop. Der Lower-Level-
                            // `Unicode.write_clipboard()` würde einen bereits
                            // geöffneten Clipboard-Handle voraussetzen.
                            if let Some(replacement) = callback(text) {
                                match clipboard_win::set_clipboard_string(&replacement) {
                                    Ok(()) => {
                                        // Nach unserem Write hat sich die Seq erhöht.
                                        // Diese neue Seq als „verarbeitet" markieren,
                                        // damit der nächste Loop-Tick keinen Echo-Fire
                                        // auslöst.
                                        // SAFETY: parameterloser thread-safe Read.
                                        let new_seq = unsafe { GetClipboardSequenceNumber() };
                                        last_processed_seq = new_seq;
                                        log::info!(
                                            "auto-replaced clipboard with pseudonymized text (new seq={new_seq})"
                                        );
                                    }
                                    Err(e) => {
                                        log::warn!("clipboard write failed: {e:?}");
                                        last_processed_seq = seq;
                                    }
                                }
                            } else {
                                last_processed_seq = seq;
                            }
                        }
                        Ok(_) => {
                            // Leerer Inhalt — markieren als „verarbeitet",
                            // damit wir nicht ständig dieselbe leere Seq probieren.
                            last_processed_seq = seq;
                        }
                        Err(e) => log::debug!("clipboard read failed: {e:?}"),
                    }
                }
                thread::sleep(Duration::from_millis(250));
            }
            log::info!("Windows clipboard watcher stopped");
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
