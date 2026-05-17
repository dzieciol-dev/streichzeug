// Auf Linux/anderen Plattformen, auf denen wir keinen ClipboardWatcher
// haben, wird dieses Modul nicht referenziert. Dead-Code-Warning gibt's
// dann zwangsweise — explizit erlauben, damit `-D warnings` nicht crasht.
#![cfg_attr(
    not(any(target_os = "windows", target_os = "macos")),
    allow(dead_code)
)]

//! Detektion der aktuell im Vordergrund laufenden Anwendung.
//!
//! Zweck: bestimmt, ob der Clipboard-Watcher reagieren soll. Nur wenn die
//! Foreground-App ein bekannter LLM-Client ist, lohnt sich Detection +
//! Notification. Spart Permission-Prompts auf macOS Sonoma (wir lesen das
//! Pasteboard nur dann, wenn's einen Anlass gibt) und reduziert die
//! False-Positive-Rate auf normalen Texten.
//!
//! Auf Windows liefern wir den **Executable-Namen** (z. B. `claude.exe`),
//! auf macOS die **Bundle-ID** (z. B. `com.anthropic.claudefordesktop`).
//! Die Whitelist in [`is_llm_app`] enthält beide Sorten.

#[cfg(target_os = "windows")]
pub fn current_process_name() -> Option<String> {
    use windows::Win32::Foundation::{CloseHandle, HMODULE};
    use windows::Win32::System::ProcessStatus::GetModuleFileNameExW;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    // SAFETY: Alle Aufrufe sind Win32-FFI mit gut dokumentierten Verträgen:
    //
    // - `GetForegroundWindow()`: thread-safe, kann von jedem Thread aufgerufen
    //   werden, gibt entweder einen gültigen HWND oder null zurück. Wir checken null.
    // - `GetWindowThreadProcessId()`: verlangt einen gültigen HWND (haben wir),
    //   schreibt PID in das gelieferte mut ref. Das mut ref ist Stack-lokal,
    //   lebt für die Dauer des Calls — kein Aliasing möglich.
    // - `OpenProcess()`: kann fehlschlagen (z. B. wegen ACL). Wir behandeln Err.
    //   Bei Ok gibt's einen HANDLE, den wir am Ende mit `CloseHandle` schließen
    //   müssen.
    // - `GetModuleFileNameExW()`: braucht einen gültigen Prozess-HANDLE und
    //   einen schreibbaren Puffer. Beide haben wir. Der Puffer ist Stack-lokal
    //   (260 u16) und kann nicht überschrieben werden, weil die Funktion die
    //   geschriebene Länge zurückgibt und wir den Slice mit `[..len]` cappen.
    // - `CloseHandle()`: Pflicht-Cleanup. Wir rufen es unconditional auf, auch
    //   wenn die folgende Length-Berechnung fehlschlägt — damit kein Handle-Leak.
    //
    // Keine Pointer-Arithmetik, kein Lifetime-Trick, kein Aliasing. Alle
    // Indirektionen liegen im OS.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }

        // PROCESS_QUERY_LIMITED_INFORMATION reicht für GetModuleFileNameExW und
        // wirft im Gegensatz zu PROCESS_QUERY_INFORMATION kein Berechtigungs-
        // Problem bei elevated Prozessen (z. B. UAC-erhöhte Apps).
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 260];
        // hmodule = HMODULE::default() (Nullpointer) → liefert den Pfad der
        // Haupt-EXE des Prozesses, was wir wollen.
        let len = GetModuleFileNameExW(handle, HMODULE::default(), &mut buf);
        let _ = CloseHandle(handle);

        if len == 0 {
            return None;
        }

        let path = String::from_utf16_lossy(&buf[..len as usize]);
        // Nur den Datei-Basename behalten und lowercase, damit der Match in
        // is_llm_app() case-insensitive funktioniert.
        path.rsplit(['\\', '/']).next().map(|s| s.to_lowercase())
    }
}

#[cfg(target_os = "macos")]
pub fn current_process_name() -> Option<String> {
    use objc2_app_kit::NSWorkspace;
    // SAFETY: objc2's `Retained<T>` ist ein owning Smart-Pointer, der
    // automatisch das Objective-C-Reference-Counting macht (retain/release).
    // `NSWorkspace::sharedWorkspace()` gibt einen Singleton zurück; keine
    // Ownership-Tricks. `frontmostApplication()` kann nil (None) liefern,
    // wenn keine App aktiv ist (z. B. Login-Screen). `bundleIdentifier()`
    // kann ebenfalls None liefern für Apps ohne Bundle (selten, z. B.
    // Helper-Tools). Beide Cases werden via `?` und `Option::map` behandelt.
    unsafe {
        let workspace = NSWorkspace::sharedWorkspace();
        let app = workspace.frontmostApplication()?;
        app.bundleIdentifier().map(|s| s.to_string())
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn current_process_name() -> Option<String> {
    None
}

/// Prüft, ob `identifier` zu einem bekannten LLM-Desktop-Client gehört.
///
/// Win: Executable-Name in lowercase (`claude.exe`, `chatgpt.exe`).
/// macOS: Bundle-ID (`com.anthropic.claudefordesktop`).
///
/// Browser sind hier **nicht** dabei — für die Browser-Pipeline ist die
/// WebExtension zuständig, die kennt sogar die Tab-URL.
pub fn is_llm_app(identifier: &str) -> bool {
    matches!(
        identifier,
        // macOS Bundle-IDs
        "com.anthropic.claudefordesktop"
        | "com.openai.chat"
        | "com.openai.chatgpt"
        | "com.microsoft.copilot"
        | "ai.perplexity.mac"
        // Windows Executable-Names (lowercase, mit .exe)
        | "claude.exe"
        | "chatgpt.exe"
        | "copilot.exe"
    )
}
