# Architektur

Code-Tour der Codebase. Als Einstieg nach `git clone` gedacht — wenn du nach längerer Pause wieder reinspringst, ist das hier der schnellste Weg zurück.

Für strategische Entscheidungen 

---

## High-Level-Datenfluss

```
                    ╭───────────────────────╮
   Tray-Icon ───────│  Tauri-App (1 Prozess) │──── Window (Svelte-UI)
                    │                       │
   Strg+B ──global──│   ┌───────────────┐   │
   Hotkey           │   │ hotkey.rs     │   │
                    │   │  → detection  │   │
   System-Clipboard │   │  → storage    │   │
   ◀──── R/W ───────│   │  → enigo →    │───── synthetic Strg+V → aktive App
                    │   └───────────────┘   │
                    │                       │
                    │   [opt-in Watcher]    │
                    │   foreground.rs +     │
                    │   clipboard/*_impl    │
                    │                       │
                    │   storage.rs (SQLite) │
                    │   secrets.rs          │
                    ╰───────────────────────╯
```

Die App ist **ein einziger Prozess**: Tauri startet, registriert den globalen Hotkey, zeigt das Tray-Icon, und hält ein verstecktes Svelte-Fenster für Tests/Settings bereit. Alles passiert in diesem Prozess.

Frühere Iterationen hatten zusätzlich eine Browser-Extension mit Native-Messaging-Host. Mit dem Hotkey-Pattern entfällt das komplett — eine UX für alle Apps.

---

## Datei-Übersicht

### Rust-Backend (`src-tauri/src/`)

| Datei                          | Aufgabe                                                       |
|--------------------------------|---------------------------------------------------------------|
| `main.rs`                      | Entry-Point. Plugin-Registrierung, Tray-Menü, Hotkey-Registration, optionaler Watcher-Spawn. |
| **`hotkey.rs`**                | **Primäre UX**: Smart-Paste-Handler. Liest Clipboard, entscheidet Forward/Reverse, schreibt zurück, schickt synthetic Strg+V. |
| `settings.rs`                  | Persistente User-Settings (`settings.json`).                  |
| `detection.rs`                 | Detection-Pipeline (Layers L1, L2, L2b, L2c). Mit Unit-Tests. |
| `tokens.rs`                    | HMAC-basierte Token-Generierung `«T_<hash>»`.                 |
| `gazetteer.rs`                 | Statische DE-Namen + Städte.                                  |
| `secrets.rs`                   | Pro-Installation zufälliger Master-Secret (OS-Keychain, Datei-Fallback); leitet DB-Schlüssel ab. |
| `storage.rs`                   | SQLCipher-verschlüsselter Mapping-Store (WAL-Mode, Multi-Prozess-safe, Auto-Migration von Klartext-DBs). |
| `clipboard.rs`                 | Cross-Platform Read/Write + `ClipboardWatcher`-Trait.         |
| `clipboard/windows_impl.rs`    | Win-Watcher via `GetClipboardSequenceNumber`-Polling.         |
| `clipboard/macos_impl.rs`      | Mac-Watcher via `NSPasteboard.changeCount`-Polling.           |
| `foreground.rs`                | Foreground-App-Detektion (Win Bundle-Name / macOS Bundle-ID). |

### Frontend (`src/`)

| Datei              | Aufgabe                                                              |
|--------------------|----------------------------------------------------------------------|
| `main.ts`          | Svelte-Bootstrap.                                                    |
| `App.svelte`       | Status + Hotkey-Anzeige + Detection-Tester.                          |
| `styles.css`       | Globale Styles.                                                      |

### CI/Config (`.github/`, `src-tauri/`)

| Datei                         | Aufgabe                                                            |
|-------------------------------|--------------------------------------------------------------------|
| `.github/workflows/ci.yml`    | Test+Clippy+cargo-audit+cargo-deny+npm-audit auf Win+Mac.          |
| `.github/dependabot.yml`      | Wöchentliche Dep-Updates (Cargo + npm + Actions).                  |
| `src-tauri/deny.toml`         | cargo-deny License-Whitelist + Advisory-Ignores.                   |
| `src-tauri/capabilities/default.json` | Tauri-2-Permissions fürs Frontend.                         |

---

## Smart-Paste-Flow (primäre UX)

```
       User drückt Strg+B in beliebiger App
                ↓
    Tauri-Plugin global-shortcut
    feuert Handler im Tauri-Prozess
                ↓
            hotkey::handle(app)
                ↓
       1. clipboard::read_clipboard_text()
                ↓
       2. detection::detect(text)
                  ┌────────────────────┐
                  ↓                    ↓
           findings.is_empty()    findings vorhanden
                ↓                      ↓
        3a. storage::restore()   3b. apply_tokens_with_hint()
                ↓                      ↓
            restored ≠ text?       storage::record() pro Finding
                ↓                      ↓
       (ja)  Some(restored)         Some(pseudonymized)
       (nein) None
                ↓
       4. clipboard::write_clipboard_text() (wenn Some)
                ↓
       5. enigo: synthetic Strg+V (oder Cmd+V auf Mac)
                ↓
       6. Notification: "X PII durch Pseudonyme ersetzt"
```

Bemerkenswert:
- Eine einzige Hotkey-Geste deckt Forward + Reverse + Pass-Through ab — die App entscheidet anhand des Inhalts.
- Kein User-Confirm-Dialog. Der Hotkey **ist** der User-Konsent.
- Wenn Clipboard nichts Verwertbares hat (kein PII, keine bekannten Tokens) → reines Strg+V durchreichen.

---

## Detection-Layer im Detail

```
text  →  L1 Regex                     →  findings[..]
        (Email, IBAN, Tel, CC, Steuer-ID, Datum)

text  →  L2  Gazetteer-Lookup         →  findings[..]
        (DE-Vor-/Nachnamen, ~200)

text  →  L2b Salutations-Regex        →  findings[..]
        (Herr/Frau/Dr. + Großbuchstaben-Wort)

findings  ─mutiert→  L2c Compound-Expansion
                    (Person-Finding nimmt nächstes Großbuchstaben-Wort dazu)

text  →  L2  Städte-Gazetteer         →  findings[..]
text  →  L2  Org-Suffix-Pattern       →  findings[..]
        (GmbH/AG/SE/KG/e.V./eG)

findings  →  dedupe_and_sort  →  finale Findings
            (überlappende Treffer: längster gewinnt)
```

L3 (ONNX-NER) ist im Konzept dokumentiert, noch nicht implementiert.

---

## Token-Format

| Position | Bedeutung |
|----------|-----------|
| `«` `»` | Französische Anführungszeichen (U+00AB / U+00BB). Translationsresistent, kollisionsfrei mit Markdown/Templates/HTML. |
| `T` | Ein-Buchstaben-Entity-Type: `P L O E T B K S D`. |
| `_` | Trennzeichen. |
| `<hash>` | 6 Zeichen base32(HMAC-SHA256(master_secret, original)). ≈30 Bit Entropie. |

Beispiele: `«P_a4b3z2»`, `«E_xy7zk9»`.

Der `master_secret` ist **pro Installation zufällig** generiert und primär im **OS-Keychain** (macOS Keychain / Windows Credential Manager, via `keyring`-Crate) persistiert. Fällt der Keychain aus oder fehlt ein Backend, greift der Datei-Fallback `$DATA_DIR/secret.bin` (Permissions 0600 auf Unix); eine bestehende Datei wird beim Start automatisch in den Keychain migriert und danach sicher gelöscht. Damit kollidieren Tokens nicht zwischen verschiedenen Usern auf derselben Maschine oder zwischen verschiedenen Installationen.

Aus dem `master_secret` wird zusätzlich der **SQLCipher-DB-Schlüssel** abgeleitet (`secrets::db_key_hex`, domain-separiert). Die Mapping-DB (`storage.db`) ist damit **AES-256-verschlüsselt**; eine bestehende unverschlüsselte DB wird beim ersten Start transparent nach SQLCipher migriert.

---

## Auto-Detection (Power-User-Feature)

Default **aus**. Im Tray-Menü togglebar, erfordert App-Restart.

Wenn aktiviert: ein Polling-Thread checkt jede 250 ms parallel
1. Den Clipboard-Sequence-Counter (Win) bzw. `NSPasteboard.changeCount` (Mac)
2. Die Foreground-App via `GetForegroundWindow` bzw. `NSWorkspace.frontmostApplication`

Trigger-Logik: **wenn** (LLM-App im Vordergrund) **und** (Clipboard-Inhalt seit letzter Verarbeitung neu) → Detection + Auto-Replace + Notification.

LLM-App-Whitelist:
- Windows: `claude.exe`, `chatgpt.exe`, `copilot.exe`
- macOS Bundle-IDs: `com.anthropic.claudefordesktop`, `com.openai.chatgpt`, `com.microsoft.copilot`, `ai.perplexity.mac`

---

## Wo entwickelt man was?

| Ziel                                       | Hauptdatei(en)                          |
|--------------------------------------------|-----------------------------------------|
| Neuer struktureller Entity-Typ (z. B. USt-IdNr) | `detection.rs` — neues Regex + Collector |
| Mehr Namen/Städte ergänzen                 | `gazetteer.rs`                          |
| Neuer Anrede-Marker („Hr. Dipl.-Ing.")     | `RE_SALUTATION_NAME` in `detection.rs`  |
| Hotkey ändern (anderer Default)            | `settings.rs::Settings::default`        |
| Smart-Paste-Logik anpassen                 | `hotkey.rs::decide_action`              |
| Neuer Tauri-Command für Frontend           | `main.rs` + `App.svelte`                |
| Phase 1: Case-Manager mit Mehrfach-Cases   | neuer `case_manager.rs`, Storage erweitern |
| ~~Phase 2: SQLCipher-Verschlüsselung~~ (erledigt) | `storage.rs` (SQLCipher via `bundled-sqlcipher-vendored-openssl`, `migrate_plaintext_if_needed`) + `secrets.rs` (OS-Keychain via `keyring`, `db_key_hex`) |

---

## Wichtige Konventionen

- **`Finding.start`/`end` sind Byte-Indices** in UTF-8, keine Char-Indices.
- **Gazetteer-Aufrufer übergeben lowercase** — der Lookup ist case-sensitive.
- **Master-Secret aus `secrets.rs`** — niemals hartkodieren, alle Token-Operationen gehen über `crate::secrets::master_secret()`.
- **`#[cfg(target_os = ...)]`** für Plattform-Code: bedingte Kompilation, keine Runtime-Checks.

---

## Tray + Activation-Policy (Plattform-spezifisch)

Beide Plattformen sind Tray-residente Apps, aber das OS-Verhalten
unterscheidet sich genug, dass `main.rs::setup` an zwei Stellen
plattform-gated Code hat:

- **macOS** → `app.set_activation_policy(ActivationPolicy::Accessory)`.
  Ohne diesen Aufruf wäre die App ein „Regular App" mit Dock-Icon
  und Cmd+Tab-Eintrag — unpassend für eine reine Menubar-App. Mit
  Accessory: kein Dock-Icon, kein Cmd+Tab, lebt nur im Tray (wie
  1Password). Keine Auswirkung auf andere Plattformen.

- **macOS-Tray-Icon** → monochromes Template (`icons/tray-icon.png`,
  P-Silhouette auf transparentem Grund), via `tauri::include_image!`
  Compile-Time-eingebettet und mit `icon_as_template(true)` markiert.
  macOS rendert es dann in der Menubar-Akzentfarbe (Dark/Light-aware).
  **Windows/Linux** nutzen weiterhin das bunte `default_window_icon()`,
  weil dort der Menubar-Kontext fehlt und ein farbiges Icon besser
  erkennbar ist.

Regenerierung des Mac-Template-Icons aus dem App-Logo:

```bash
python3 scripts/gen-tray-icon.py
```

Das Script (siehe `scripts/gen-tray-icon.py` für Details) mappt
weiße Pixel des Quell-Logos auf schwarz mit gleicher Alpha-Intensität
und blaue Pixel auf transparent, downsampled auf 32×32. Das Result
liegt in `src-tauri/icons/tray-icon.png` und wird via
`tauri::include_image!` zur Compile-Zeit ins Binary eingebettet.

---

## Testen

```bash
cd src-tauri
cargo test                 # alle 42 Tests
cargo test detection::     # nur Detection-Tests
cargo test hotkey::        # Smart-Paste-Entscheidungslogik
cargo test settings::      # Settings-Persistenz-Roundtrip
```

UI-Smoketest:

```bash
cargo tauri dev            # Window + Tray + Hotkey aktiv
```

Hotkey-Test: in einer anderen App Text mit PII kopieren, dann **Strg+B** drücken. Notification erscheint, der Text ist pseudonymisiert eingefügt.
