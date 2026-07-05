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
                                if let Err(e) = unsafe { write_text(&replacement) } {
                                    log::warn!("clipboard auto-replace failed: {e}");
                                }
                                // Nach dem Write (auch dem fehlgeschlagenen —
                                // clearContents zählt hoch) hat sich changeCount
                                // erhöht. Diesen neuen Stand als verarbeitet
                                // merken, sonst feuert der nächste Tick wegen
                                // unseres eigenen Writes.
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
                // TODO(adaptives-Polling): konstante 250 ms. ARCHITECTURE.md /
                // clipboard.rs-Moduldoc beschreiben ein geplantes adaptives
                // Intervall (200 ms bei LLM-App im Vordergrund, sonst 1000 ms),
                // das hier noch NICHT umgesetzt ist — separates Vorhaben.
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
/// `Err`, wenn `setString:forType:` NO liefert (Pasteboard von anderer App
/// gehalten o. ä.) — der Aufrufer MUSS das melden können: nach dem
/// `clearContents` wäre das Clipboard sonst leer, während die Bühne
/// „liegt im Clipboard" behauptet.
///
/// # Safety
///
/// Gleiche Threading-Annahme wie [`current_change_count`].
pub(super) unsafe fn write_text(text: &str) -> Result<(), String> {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let ns_type = NSString::from_str("public.utf8-plain-text");
    let ns_text = NSString::from_str(text);
    if pb.setString_forType(&ns_text, &ns_type) {
        Ok(())
    } else {
        Err("NSPasteboard setString:forType: lieferte NO (Pasteboard belegt?)".into())
    }
}

/// Liest den HTML-Flavor (`public.html`) der allgemeinen Pasteboard. Word,
/// Outlook und Browser legen ihn beim Kopieren formatierter Inhalte neben den
/// Plain-Text. Gleiche Prompt-Vorsicht wie [`read_text`]: erst nach einer
/// gemeldeten Änderung aufrufen.
///
/// # Safety
///
/// Gleiche Threading-Annahme wie [`current_change_count`].
pub(super) unsafe fn read_html() -> Option<String> {
    let pb = NSPasteboard::generalPasteboard();
    let ns_type = NSString::from_str("public.html");
    pb.stringForType(&ns_type).map(|s| s.to_string())
}

/// Schreibt HTML- und Plain-Text-Flavor in einem Zug: EIN `clearContents`,
/// dann beide `setString_forType` — so sieht jede Ziel-App genau einen
/// konsistenten Clipboard-Zustand (formatiert für Word/Outlook, Text für
/// alles andere).
///
/// `Err`, sobald einer der beiden Writes NO liefert — der Aufrufer (Bühne)
/// versucht dann den Text-only-Fallback bzw. warnt, statt fälschlich
/// „liegt im Clipboard" zu melden.
///
/// # Safety
///
/// Gleiche Threading-Annahme wie [`current_change_count`].
pub(super) unsafe fn write_html(html: &str, text_fallback: &str) -> Result<(), String> {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let html_ok = pb.setString_forType(
        &NSString::from_str(html),
        &NSString::from_str("public.html"),
    );
    let text_ok = pb.setString_forType(
        &NSString::from_str(text_fallback),
        &NSString::from_str("public.utf8-plain-text"),
    );
    match (html_ok, text_ok) {
        (true, true) => Ok(()),
        (h, t) => Err(format!(
            "NSPasteboard setString:forType: lieferte NO (html_ok={h}, text_ok={t})"
        )),
    }
}

/// Liest den Bild-Flavor der Pasteboard: bevorzugt `public.png`, sonst
/// `public.tiff` (viele Apps legen nur TIFF ab — dekodiert die
/// Bild-Pipeline genauso). Gleiche Prompt-Vorsicht wie [`read_text`].
///
/// # Safety
///
/// Gleiche Threading-Annahme wie [`current_change_count`].
pub(super) unsafe fn read_image() -> Option<Vec<u8>> {
    let pb = NSPasteboard::generalPasteboard();
    for uti in ["public.png", "public.tiff"] {
        let ns_type = NSString::from_str(uti);
        if let Some(data) = pb.dataForType(&ns_type) {
            return Some(data.bytes().to_vec());
        }
    }
    None
}

/// Schreibt PNG-Bytes und Text-Fallback in einem Zug (EIN `clearContents`).
/// `Err`, sobald einer der Writes NO liefert — gleiche Ehrlichkeits-Regel
/// wie [`write_html`].
///
/// # Safety
///
/// Gleiche Threading-Annahme wie [`current_change_count`].
pub(super) unsafe fn write_image(png: &[u8], text_fallback: &str) -> Result<(), String> {
    use objc2_foundation::NSData;
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let png_ok = pb.setData_forType(
        Some(&NSData::with_bytes(png)),
        &NSString::from_str("public.png"),
    );
    let text_ok = pb.setString_forType(
        &NSString::from_str(text_fallback),
        &NSString::from_str("public.utf8-plain-text"),
    );
    match (png_ok, text_ok) {
        (true, true) => Ok(()),
        (p, t) => Err(format!(
            "NSPasteboard-Write lieferte NO (png_ok={p}, text_ok={t})"
        )),
    }
}
