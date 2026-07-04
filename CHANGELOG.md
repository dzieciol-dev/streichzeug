# Changelog

Versionierung folgt [SemVer](https://semver.org/) (Major.Minor.Patch).

---

## [Unreleased]

**Sicherheit / Härtung:**

- **Mapping-DB mit SQLCipher (AES-256) verschlüsselt.** Der Schlüssel wird
  aus dem Master-Secret abgeleitet, das nun im OS-Keychain (macOS Keychain /
  Windows Credential Manager) mit Datei-Fallback liegt. Bestehende
  unverschlüsselte DBs/Secrets werden beim ersten Start transparent migriert.
- **ONNX-Runtime-Library wird per SHA-256 verifiziert** (Manifest), bevor sie
  geladen wird; Installationen ohne Lib-Hash fordern sauber einen Re-Download.
- Frische RUSTSEC-Advisories adressiert: `anyhow` 1.0.103, `quinn-proto`
  0.11.15 (echte Fixes); quick-xml-DoS (transitiv über Tauri, im
  Bedrohungsmodell nicht erreichbar) dokumentiert ignoriert.
- **npm-Security-Upgrade**: vite 6, `vite-plugin-svelte` 5, svelte 5
  (Legacy-Modus, keine Runes-Migration) → 0 npm-Vulnerabilities.

**UX:**

- **macOS: Dock-Icon** statt reiner Menüleisten-App. Rotes X versteckt nur
  das Fenster (App läuft weiter); Dock-Klick bzw. erneutes Öffnen
  (`RunEvent::Reopen`) holt es zurück. Vorbereitung für fenster-zentrierte
  Features.
- **Schlüsselbund-Zugriff wird im Onboarding angekündigt** und erst beim
  Abschluss ausgelöst — kein unvermittelter macOS-Dialog beim ersten Start.
- Sichtbare Fehlermeldungen statt stiller Logs bei fehlgeschlagenem Paste
  (inkl. Hinweis auf macOS-Bedienungshilfen), ephemerem Secret-Fallback und
  Settings-Speicherfehlern.

**Detection:**

- Steuer-ID-Erkennung verlangt jetzt ein Kontextwort plus Strukturregel —
  deutlich weniger False-Positives auf beliebige 11-stellige Zahlen.

---

## 0.5.1 — Beta-Feedback-Fixes

**Bugfixes aus dem ersten Windows-Beta-Test:**

- **Win-Auto-Extract der ORT-Runtime real implementiert.** `extract_ort_lib`
  bailte unter Windows mit „Win-Auto-Extract noch nicht implementiert" und
  ließ Beta-Tester die `onnxruntime.dll` händisch aus dem heruntergeladenen
  `ort_archive.bin` ziehen. Jetzt: `zip`-Crate als optionale Dep im
  `ner`-Feature, Win-Branch entpackt analog zu macOS/Linux. Onboarding-Flow
  „Modell jetzt laden" läuft unter Win bis zur fertig geladenen NER-Engine
  durch.

**UX-Verbesserungen:**

- „PII" durch „personenbezogene Daten" ersetzt im Onboarding-Wizard
  (Step 2 „Verarbeitungsmodus") und in der Haupt-UI (Bullet-Liste +
  beide Mode-Beschreibungen).
- Release-Body zeigt jetzt einen **Win-SmartScreen-Hinweis**
  parallel zum macOS-xattr-Block — Beta-Tester sehen vor dem Download,
  wie sie die „Computer wurde geschützt"-Warnung legitim umgehen.

---

## 0.5.0 — Initial public release

Erste öffentliche Version von **Streichzeug**, einem lokalen Desktop-Tool
für PII-Erkennung und -Pseudonymisierung in der Zwischenablage.

**Funktionsumfang:**

- Globaler Hotkey **Strg+Alt+B** (Cmd+Option+B auf macOS) mit
  Smart-Paste — Forward (Klartext → Pseudonyme) und Reverse
  (Pseudonyme → Klartext) anhand des Clipboard-Inhalts.
- Detection-Pipeline mit drei Layern:
  - **L1 Regex**: E-Mail, IBAN (Mod-97), Kreditkarte (Luhn),
    DE-Telefon, Steuer-ID (ISO-7064), USt-IdNr., BIC, Datum,
    Straße + PLZ, URLs.
  - **L2 Gazetteer**: kuratierte Listen für DE-Vor-/Nachnamen
    (~200 Einträge) und DACH-Städte (~120 Einträge), kombiniert mit
    Anrede-Kontext-Regex (Herr/Frau/Dr./Prof./Mr./Mrs.) und
    Compound-Expansion (Vorname + unbekannter Nachname).
  - **L3 NER** (optional, Feature-Flag `ner`): lokales DistilBERT-
    Modell für statistische Erkennung in unstrukturierten Texten.
    ~145 MB, ONNX-Runtime 1.22.x, läuft komplett offline.
- **Zwei Verarbeitungsmodi**:
  - *Reversibel*: HMAC-SHA256-Tokens `«T_<hash>»`, pro-Installation
    zufälliger Master-Secret, lokale Mapping-DB für Reverse.
  - *Strict*: lesbare Platzhalter `«Person A»`, keine Mapping-Tabelle,
    Daten beim LLM sind anonym (ErwGr. 26 DSGVO).
- **Pro-Forward-Cases**: derselbe Klartext in zwei separaten Forwards
  produziert unterschiedliche Tokens (verhindert Cross-Session-
  Frequency-Analyse durch LLM-Log-Sammler).
- **Konfigurierbare Retention** (15 min / 1 h / 8 h / 24 h /
  Session-only) — Mappings werden automatisch nach Ablauf gelöscht.
- **macOS-spezifisch**: Accessory-App-Policy (kein Dock-Icon,
  Menubar-only), Template-Tray-Icon (monochrom, Dark/Light-aware).

**Bekannte Schwächen** (siehe README → „Bekannte Schwächen"):

- Mapping-DB plain SQLite ohne Encryption-at-Rest.
- Master-Secret als File auf der Platte (statt OS-Keychain).
- Kein SecureZeroMemory für Klartext-Mappings im RAM.
- Keine Code-Signing-Pipeline für Distributions-Bundles.

**Technologie-Stack**:

- Rust + Tauri 2.x (Backend + Tray + Hotkey)
- Svelte + TypeScript + Vite (Frontend)
- SQLite (Mapping-Store, WAL-Mode)
- ONNX Runtime 1.22.x + tokenizers (optionale NER-Layer)
- HMAC-SHA256 (Token-Generation)

---

## Konventionen für Bug-Reports

Bei Problemen bitte ein [GitHub-Issue](https://github.com/dzieciol-dev/streichzeug/issues)
eröffnen mit:

- **Version** (aus dem Header-Badge im App-Fenster)
- Im UI auf „Log in Zwischenablage kopieren" klicken und das Log-Snippet
  mitschicken (PII ist im Log immer redacted)
- Kurze Beschreibung des Vorgangs (welche App war im Fokus, welcher
  Text wurde kopiert, was wurde gepastet)
