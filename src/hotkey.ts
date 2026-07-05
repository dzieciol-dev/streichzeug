// Gemeinsamer Helfer zur menschenlesbaren Darstellung eines Tauri-Shortcuts.
// Bewusst in eine eigene Datei extrahiert, damit App.svelte und StashView.svelte
// dieselbe Formatierung nutzen (keine Duplikate).
export function prettyHotkey(s: string): string {
  // "CmdOrCtrl+B" → "Strg + B" auf Win, "⌘ + B" auf Mac (best effort
  // ohne UA-Sniffing).
  const isMac = navigator.platform.toLowerCase().includes("mac");
  return s
    .replace(/CmdOrCtrl/g, isMac ? "⌘" : "Strg")
    .replace(/Ctrl/g, "Strg")
    .replace(/Alt/g, isMac ? "⌥" : "Alt")
    .replace(/Shift/g, isMac ? "⇧" : "Umschalt")
    .replace(/\+/g, " + ");
}
