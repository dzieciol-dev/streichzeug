# Mac-Build und Test

Anleitung, um Streichzeug auf einem Mac (Apple Silicon oder Intel)
selbst zu bauen und zu testen. Cross-Compile von Windows zu macOS ist
nicht praktikabel — daher muss der Mac-Build auf einem Mac laufen.

## 1. Code aufs Mac bringen

Optionen:

- **Git-Clone vom privaten Repo** (falls Zugriff über GitHub-Account):
  ```bash
  git clone <repo-url>
  cd streichzeug
  git checkout feature/ner-layer3
  ```
- **Direkt vom Windows-Rechner kopieren** (Dropbox / iCloud / scp /
  USB). Wichtig: `.git`-Ordner mitkopieren, sonst fehlt die Branch-
  Information. `node_modules/`, `src-tauri/target*/` und
  `src-tauri/models/` ausschließen — die werden lokal neu erzeugt.

## 2. Voraussetzungen installieren

Einmalig pro Mac:

```bash
# Xcode Command Line Tools (für C-Toolchain)
xcode-select --install

# Homebrew, falls noch nicht da
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# Rust (stable)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# Node.js (LTS reicht)
brew install node

# Tauri CLI
cargo install tauri-cli --version "^2.0"
```

## 3. Dependencies + Modell

```bash
cd streichzeug   # bzw. dein Repo-Verzeichnis
npm install

# Modell + ORT-Runtime + Manifest + Win-DLL-Placeholder
bash scripts/download-ner-model.sh
```

Das Script erkennt automatisch Apple Silicon (`arm64`) vs. Intel
(`x86_64`) und zieht die passende `libonnxruntime.dylib`.

## 4. Build

```bash
cd src-tauri
cargo tauri build --features ner
```

Erstmaliger Build: ~10-15 Minuten (Tauri + ORT-Crate kompilieren).
Folgebuilds: 1-2 Minuten dank Cargo-Cache.

Output:
- App-Bundle: `target/release/bundle/macos/Streichzeug.app`
- DMG-Installer: `target/release/bundle/dmg/Streichzeug_0.4.0_x64.dmg`
  (bzw. `_aarch64.dmg` auf Apple Silicon)

## 5. App-Installation

```bash
# DMG mounten + App nach /Applications ziehen, ODER:
cp -R target/release/bundle/macos/Streichzeug.app /Applications/
```

**Erster Start blockiert von Gatekeeper** (DMG nicht signiert):

```bash
# Quarantäne-Attribut entfernen — App wird beim nächsten Start
# durchgewunken
xattr -dr com.apple.quarantine "/Applications/Streichzeug.app"
```

Oder UI-Weg: Systemeinstellungen → Datenschutz & Sicherheit → unten
„Trotzdem öffnen" für Streichzeug.

## 6. Permissions

Beim ersten Hotkey-Druck (Cmd+Option+B / Strg+Alt+B) wird macOS nach
**Eingabehilfen-Permissions** fragen (Accessibility). Diese braucht
`enigo`, um synthetisches Cmd+V zu senden:

- Systemeinstellungen → Datenschutz & Sicherheit → **Bedienungshilfen**
- Streichzeug aktivieren

Plus für Clipboard-Read: Systemeinstellungen → Datenschutz & Sicherheit
→ **Eingabeüberwachung** falls's nachgefragt wird.

## 7. Test-Setup

Dasselbe Workflow wie auf Windows:

1. Strg+Alt+B = Cmd+Option+B auf Mac (der Tauri-Accelerator
   `CmdOrCtrl+Alt+B` löst auf macOS zu `Cmd+Option+B` auf)
2. Text mit PII kopieren → in Editor `Cmd+Option+B` drücken →
   pseudonymisiert
3. UI öffnen über Tray-Icon (oben in der Menubar) → „Fenster anzeigen"
4. Verarbeitungsmodus zwischen Reversibel und Strict wechseln

## UX-Erwartung auf macOS

Die App erscheint auf macOS mit **Dock-Icon** (ActivationPolicy = Regular),
wie eine normale App:

- **Dock-Icon** und **Cmd+Tab-Eintrag** vorhanden.
- Rotes Schließen-Kreuz / Cmd+W blendet das Fenster nur aus, beendet die
  App aber nicht — sie läuft im Hintergrund weiter. Ein Klick aufs
  Dock-Icon (oder erneutes Öffnen) holt das Fenster zurück; alternativ
  übers Tray-Menü → „Fenster anzeigen".
- Beenden übers Tray-Menü → „Beenden" (oder Cmd+Q bei aktivem Fenster).

Das **Tray-Icon ist eine schwarze P-Silhouette** (Template-Image),
das vom System in der Menubar-Akzentfarbe gerendert wird — passt
sich an Dark/Light Mode automatisch an, sieht aus wie native Apps.

## Erwartete Hürden

- **Tray-Icon scheinbar unsichtbar**
  → Auf MacBook Pro mit Notch werden Menubar-Items hinter den Notch
  geschoben, wenn viele Apps gleichzeitig laufen. Test: **Cmd
  gedrückt halten und ein vorhandenes Tray-Icon nach links ziehen** —
  wenn Clipboard-PII dahinter versteckt war, taucht es auf.
- **`bundle_dmg.sh` schlägt beim zweiten Build mit nichtssagender
  Fehlermeldung fehl**
  → Ein altes DMG-Volume blieb gemounted (`/Volumes/Clipboard-PII`)
  und blockiert das Bundling. Cleanup:
  ```bash
  hdiutil info | grep "/Volumes/Clipboard-PII" | awk '{print $1}' \
    | xargs -I{} hdiutil detach {} -force
  rm -f src-tauri/target/release/bundle/macos/rw.*.dmg
  ```
  Danach `cargo tauri build --features ner` erneut.
- **DMG-Build schlägt fehl mit „file not found: models/onnxruntime.dll"**
  → Das `download-ner-model.sh` erzeugt einen 0-Byte-Placeholder.
  Falls's nicht da ist: `touch src-tauri/models/onnxruntime.dll`.
- **„App ist beschädigt"-Dialog**
  → Quarantäne-Attribut: `xattr -dr com.apple.quarantine
  "/Applications/Streichzeug.app"`
- **Hotkey reagiert nicht**
  → Accessibility-Permission nicht erteilt; siehe Schritt 6.
- **NER lädt nicht**
  → Log unter `~/Library/Application Support/de.streichzeug.app/logs/app.log`
  prüfen. Mit hoher Wahrscheinlichkeit findet `models_dir()` das
  Models-Verzeichnis nicht (Mac-Bundle-Struktur ist von Windows
  abweichend). Pfad-Hinweis im Log zeigt alle probierten Locations.

## Bekannte Lücken auf macOS

- `enigo` braucht Accessibility-Permissions — der erste Hotkey-Druck
  scheitert daher leise, bis die Permission erteilt ist
- Falls die ORT-Dylib nicht über Standard-Suchpfade gefunden wird:
  `install_name_tool` mit rpath-Adjustment nötig
- Code-Signing (für DMG-Notarization) komplett ausstehend — DMG ist
  ad-hoc-signiert von der Tauri-Toolchain
