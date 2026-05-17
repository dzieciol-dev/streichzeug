# Streichzeug

> Lokales Desktop-Tool, das personenbezogene Daten in der Zwischenablage
> erkennt und durch Pseudonyme ersetzt — bevor du sie in einen LLM-Chat
> (ChatGPT, Claude, Gemini, Copilot, …) pastest.

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)

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

- **Mapping-DB ist plain SQLite** (kein Encryption-at-Rest). Eine zukünftige
  Version kann SQLCipher + OS-Keychain einbinden (`keyring`-Crate).
- **Master-Secret als File auf der Platte** (statt OS-Keychain). Mit
  lokalem Filezugriff lesbar.
- **Memory-Dump** der laufenden App leakt Klartext-Mappings. SecureZeroMemory
  noch nicht implementiert.

Diese Schwächen sind im Repo als Issues getrackt — Pull Requests willkommen.

---

## Setup auf neuem Gerät

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

git clone https://github.com/lurning/streichzeug.git
cd streichzeug
npm install
cargo tauri dev
```

Beim ersten Drücken von **Cmd+Option+B** fragt macOS nach
**Accessibility-Permissions** für das synthetische Cmd+V — bestätigen,
sonst funktioniert das Auto-Paste nicht. Die Permission ist eine
OS-Sicherheitsmaßnahme, kein Bug der App.

Die Mac-App ist eine **reine Menubar-App** (kein Dock-Icon, kein Cmd+Tab-
Eintrag) mit monochromem Template-Tray-Icon — siehe
[`MAC_SETUP.md`](MAC_SETUP.md) → „UX-Erwartung auf macOS".

### Windows (PowerShell)

```powershell
winget install Rustlang.Rustup
# Terminal komplett neu öffnen
cargo install tauri-cli --version "^2.0"
winget install OpenJS.NodeJS.LTS
winget install Microsoft.VisualStudio.2022.BuildTools

git clone https://github.com/lurning/streichzeug.git
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
https://github.com/lurning/streichzeug/issues

Sicherheits-Findings: siehe **[`SECURITY.md`](SECURITY.md)**.

---

## Lizenz

[AGPL-3.0-only](LICENSE). Volltext in [`LICENSE`](LICENSE).

Wer den Code in einer eigenen Anwendung (auch SaaS / Cloud-Service)
verwendet, muss seine Anpassungen unter derselben Lizenz veröffentlichen.
