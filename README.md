# Streichzeug

> Lokales Desktop-Tool, das personenbezogene Daten in der Zwischenablage
> erkennt und durch Pseudonyme ersetzt — bevor du sie in einen LLM-Chat
> (ChatGPT, Claude, Gemini, Copilot, …) pastest.

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT_OR_Apache--2.0-blue.svg)](#lizenz)

```
                  Original-Text                          Pseudonymisiert
                  ─────────────                          ───────────────
"Herr Müller, IBAN DE89370400440532013000"    "Herr «P_a4b», IBAN «B_t7t»"
       ↑                                              ↓
   Strg+C                                       Strg+Alt+B
   (Outlook)                                       (Claude Desktop)
```

---

## Bedienung in zwei Sätzen

1. Text in beliebiger App kopieren (Strg+C).
2. In der Zielapp **Strg+Alt+B** drücken (statt Strg+V).
   - Enthält der Text PII → fügt pseudonymisierten Text ein.
   - Enthält der Text bekannte Pseudonyme → fügt Originale ein (Reverse).
   - Sonst → ganz normales Paste.

Die App läuft als Tray-Icon im Hintergrund, der globale Hotkey funktioniert
in **jeder** Anwendung.

### Zweiter Workflow: Schwärz-Bühne

Für „erst schwärzen, später verwenden": Text in einer beliebigen App
**markieren** und **Strg+Alt+Shift+B** drücken. Streichzeug holt die
Markierung, bringt das eigene Fenster nach vorn und schwärzt die
Fundstellen sichtbar vor deinen Augen (Marker-Animation). Das Ergebnis
liegt sofort im Clipboard **und** in der **Ablage** der App — dort
gespeichert wird ausschließlich die geschwärzte Fassung, nie der
Originaltext. Ohne Markierung nimmt die Bühne den aktuellen
Clipboard-Inhalt (erst kopieren, dann Hotkey — nützlich z. B. in
Terminals, wo Strg+C anders belegt ist).

---

## Was erkannt wird

- **E-Mail-Adressen**
- **IBAN** (Mod-97-validiert)
- **Kreditkartennummern** (Luhn-validiert)
- **Deutsche Telefonnummern**
- **Steuer-ID** (ISO-7064-validiert)
- **USt-IdNr.** (`DE` + 9 Ziffern)
- **BIC** (SWIFT-Codes)
- **Datumsangaben** (DD.MM.YYYY)
- **Personen** — kuratiertes Gazetteer + Anrede-Kontext (Herr/Frau/Dr./Prof./Mr./Mrs.) + Compound-Expansion (Vor- + Nachname)
- **Städte** — kuratiertes Gazetteer (~120 DACH-Städte)
- **Organisationen** — Rechtsform-Suffixe (GmbH/AG/SE/KG/OHG/UG/e.V./eG)
- **Adressen** — Straßen + PLZ
- **URLs / Hostnames**

Optional ein zuschaltbares **lokales NER-Modell** (DistilBERT, ONNX-Runtime,
quantisiert, ~145 MB) für statistische Erkennung in unstrukturierten Texten —
das Modell läuft komplett offline, keine Daten verlassen den Rechner.

---

## Zwei Modi

### Reversibel (Default)

PII wird durch deterministische Tokens ersetzt: `«P_a4b3z2»`, `«E_xy7zk9»`, …
Tokens sind HMAC-SHA256-Hashes mit einem pro-Installation zufälligen Master-
Secret. Die Mappings werden lokal in einer SQLite-Datenbank gespeichert
(mit konfigurierbarer Retention — default 60 Minuten).

**Reverse-Pfad:** Strg+Alt+B auf einer LLM-Antwort, die Tokens enthält,
übersetzt die Tokens zurück in die Originale.

### Strict (volle Anonymisierung)

Statt Tokens werden lesbare Platzhalter erzeugt: `«Person A»`,
`«Organisation B»`, `«Ort C»`, … Pro Forward-Vorgang ein eigener Counter,
und **keine Mapping-Tabelle wird angelegt** — die Zuordnung existiert
weder lokal noch beim LLM. Damit gelten die Daten beim LLM-Anbieter als
**anonym** im Sinne von ErwGr. 26 DSGVO.

**Kein Reverse-Pfad** in Strict — der User muss die LLM-Antwort manuell
auf den Kontext zurückführen.

---

## Privacy-Design

- **Keine Outbound-Verbindungen** — die App ruft nichts im Netz auf.
  Auditierbar per Wireshark, kein Telemetry-Code im Repo.
- **Pro-Installation-zufälliger Master-Secret** (32 Byte aus dem OS-RNG)
  in `$DATA_DIR/de.streichzeug.app/secret.bin` mit `0600`-Permissions
  auf Unix bzw. per-User-ACL auf Windows.
- **Pro-Forward-Cases** — derselbe Klartext in zwei separaten Forward-Aktionen
  produziert unterschiedliche Tokens (Cross-Session-Frequency-Analyse
  ausgeschlossen).
- **Configurable Retention** — alte Mappings werden automatisch nach
  Ablauf gelöscht (15 min / 1 h / 8 h / 24 h / Session-only).

### Bekannte Schwächen

- **Zugriff bei entsperrtem Konto.** Die Mapping-DB ist mit SQLCipher
  (AES-256) verschlüsselt und der Schlüssel liegt im OS-Keychain (macOS
  Keychain / Windows Credential Manager, mit Datei-Fallback wo kein Keychain
  verfügbar) — aber solange dein Benutzerkonto entsperrt ist, kann die App
  und damit auch Schadsoftware unter demselben Konto die Daten lesen.
- **Memory-Dump** der laufenden App leakt Klartext-Mappings. SecureZeroMemory
  noch nicht implementiert.
- **Nur Windows und macOS** — auf Linux ist das Kernfeature (Clipboard-Erkennung
  + Smart-Paste) funktionslos (Stub-Watcher); die App warnt beim Start
  entsprechend.

Diese Schwächen sind im Repo als Issues getrackt — Pull Requests willkommen.

---

## Installation

### macOS — empfohlen: Homebrew Cask

```bash
brew install --cask dzieciol-dev/streichzeug/streichzeug
```

Brew lädt das DMG, entfernt das Quarantäne-Attribut und installiert
nach `/Applications/`. Kein „App ist beschädigt"-Dialog. Updates:
`brew upgrade --cask streichzeug`.

### macOS — manueller Download

Aktuelles DMG aus den
[GitHub Releases](https://github.com/dzieciol-dev/streichzeug/releases).
Nach dem Drag-and-Drop in `/Applications/` blockt macOS die App,
weil die Binary noch nicht Apple-Developer-signiert ist (Apple
Developer Account kostet 99 €/Jahr — kommt später). Einmaliger
Workaround im Terminal:

```bash
xattr -dr com.apple.quarantine /Applications/Streichzeug.app
```

Dann normal über Launchpad starten.

### Windows

MSI-Installer aus den
[GitHub Releases](https://github.com/dzieciol-dev/streichzeug/releases).
Doppelklick → SmartScreen kann „Computer wurde geschützt" zeigen
(unsignierter Installer) → „Weitere Informationen" → „Trotzdem
ausführen".

---

## Build aus Source

### macOS

Vollständige Schritt-für-Schritt-Anleitung inkl. Modell-Download,
NER-Feature-Build und DMG-Erstellung: siehe **[`MAC_SETUP.md`](MAC_SETUP.md)**.

Kurzfassung Dev-Mode (ohne NER, ohne DMG-Bundling):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
brew install node
xcode-select --install
cargo install tauri-cli --version "^2.0"

git clone https://github.com/dzieciol-dev/streichzeug.git
cd streichzeug
npm install
cargo tauri dev
```

Beim ersten Drücken von **Cmd+Option+B** fragt macOS nach
**Accessibility-Permissions** für das synthetische Cmd+V — bestätigen,
sonst funktioniert das Auto-Paste nicht. Die Permission ist eine
OS-Sicherheitsmaßnahme, kein Bug der App.

Die Mac-App erscheint im **Dock** (rotes X versteckt nur das Fenster, die
App läuft im Hintergrund weiter; Dock-Klick holt sie zurück) und hat
zusätzlich ein monochromes Template-Tray-Icon in der Menüleiste — siehe
[`MAC_SETUP.md`](MAC_SETUP.md) → „UX-Erwartung auf macOS".

### Windows (PowerShell)

```powershell
winget install Rustlang.Rustup
# Terminal komplett neu öffnen
cargo install tauri-cli --version "^2.0"
winget install OpenJS.NodeJS.LTS
winget install Microsoft.VisualStudio.2022.BuildTools

git clone https://github.com/dzieciol-dev/streichzeug.git
cd streichzeug
npm install
cargo tauri dev
```

Der globale Hotkey funktioniert sofort, keine zusätzlichen Permissions nötig.

---

## Tägliche Befehle

```bash
cargo tauri dev               # Dev-Mode (Window auf, Hotkey aktiv, Hot-Reload)
cd src-tauri && cargo test    # Unit-Tests
cargo tauri build             # Production-Bundle (MSI / DMG)
```

Für ausführliches Logging:

```powershell
$env:RUST_LOG = "streichzeug=debug"
cargo tauri dev
```

---

## Architektur

Code-Tour: siehe **[`ARCHITECTURE.md`](ARCHITECTURE.md)**.

Kurzfassung:

```
                    ╭───────────────────────╮
   Tray-Icon ───────│  Tauri-App (1 Prozess) │──── Window (Svelte-UI)
                    │                       │
   Strg+Alt+B ──────│   ┌───────────────┐   │
   global hotkey    │   │ hotkey.rs     │   │
                    │   │  → detection  │   │
   System-Clipboard │   │  → storage    │   │
   ◀──── R/W ───────│   │  → enigo →    │───── synthetic Strg+V → aktive App
                    │   └───────────────┘   │
                    │                       │
                    │   storage.rs (SQLite) │
                    │   secrets.rs          │
                    ╰───────────────────────╯
```

---

## Beitragen

Pull Requests sind willkommen. Siehe **[`CONTRIBUTING.md`](CONTRIBUTING.md)**
für den DCO-Workflow (Developer Certificate of Origin) und unsere
Code-Konventionen.

Vor einem PR bitte:

```bash
cd src-tauri && cargo test          # alle Unit-Tests grün
cd src-tauri && cargo clippy -- -D warnings
cargo fmt
```

Bug-Reports und Feature-Requests:
https://github.com/dzieciol-dev/streichzeug/issues

Sicherheits-Findings: siehe **[`SECURITY.md`](SECURITY.md)**.

---

## Lizenz

**Dual-lizenziert unter `MIT` ODER `Apache-2.0` — du wählst.**
Das ist der Rust-Ökosystem-Standard.

- [`LICENSE-MIT`](LICENSE-MIT) — minimal, „behalte den Hinweis bei"
- [`LICENSE-APACHE`](LICENSE-APACHE) — wie MIT, plus expliziter Patent-Grant
  für mehr Enterprise-Use-Trust

Beide Lizenzen sind permissiv: nimm den Code, baue darauf auf, ob für
private, kommerzielle oder Open-Source-Projekte — egal. Wir bestehen
nur darauf, dass die Copyright-Notice und die jeweilige Lizenz erhalten
bleiben (sofern aus rechtlicher Sicht überhaupt eine Notice nötig ist).

### Hinweis zu KI-generiertem Code

Teile dieser Codebase wurden mithilfe von KI-Coding-Assistenten erzeugt.
Da die urheberrechtliche Schöpfungshöhe rein KI-generierter Werke in
mehreren Jurisdiktionen (insbesondere DE/EU) ungeklärt bzw. eher
verneint wird, treffen wir keine Aussage darüber, **welcher Teil dieses
Repos eigentum-fähig ist und welcher nicht**. Permissive Lizenzen wie
MIT/Apache-2.0 funktionieren trotzdem — sie geben dir alle Rechte, die
wir geben können, und mehr als das geht ohnehin nicht.

### Beitragen

Beiträge unterliegen automatisch denselben beiden Lizenzen (siehe
[`CONTRIBUTING.md`](CONTRIBUTING.md) — Rust-Standard-Klausel).
