# Konzept: Schwärz-Bühne — Streichzeug als Clipboard-App

Stand 2026-07-04 · Status: beschlossen, nicht begonnen · Ansprechpartner: Maintainer

Dieses Dokument ist die **alleinige Quelle der Wahrheit** für das Feature.
Es ist so geschnitten, dass einzelne Arbeitspakete (WP-A …) von separaten
Agents umgesetzt werden können, ohne die gesamte Codebase oder dieses
Dokument komplett lesen zu müssen. **Jeder Agent liest: Abschnitt 1
(Überblick), Abschnitt 2 (Verträge) und sein eigenes WP — sonst nichts.**
Die Verträge in Abschnitt 2 sind verbindlich; wer davon abweichen muss,
bricht ab und meldet das, statt still zu ändern.

---

## 1. Überblick

### 1.1 Was gebaut wird

Heute ist Streichzeug ein unsichtbarer Paste-Helfer (Hotkey `Strg+Alt+B`
ersetzt PII beim Einfügen). Neu dazu kommt ein zweiter, sichtbarer Workflow —
die **Schwärz-Bühne**:

1. User markiert Text in einer beliebigen App (z. B. E-Mail in Outlook)
   und drückt den **Capture-Hotkey** (Default `Strg+Alt+Shift+B`).
2. Streichzeug holt die Markierung per synthetischem Strg+C, erkennt PII
   (bestehende Detection-Pipeline) und **bringt das eigene Fenster nach
   vorn** (Fokus-Übernahme ist hier erwünscht).
3. Im Fenster läuft die **Marker-Animation**: Der Originaltext ist zu sehen,
   ein Filzstift-Strich fährt gestaffelt über jede Fundstelle, darunter
   erscheint das Pseudonym (`«P_a4b»`) bzw. der Platzhalter („Person A" im
   Strict-Mode). Diese Animation ist der emotionale Kern des Features.
4. Das geschwärzte Ergebnis liegt sofort im System-Clipboard **und** als
   Eintrag in einer **Ablage** in der App (Liste, „Kopieren"-Button,
   Löschen) — bereit für den LLM-Chat, wann immer der User so weit ist.

Ausbaustufen (jede für sich shipbar):

- **Stufe 1 (MVP):** obiger Flow für Plain-Text. → WP-A bis WP-E
- **Stufe 2:** Formatierung bleibt erhalten (HTML-Clipboard aus
  Word/Outlook lesen, geschwärzt als HTML + Text zurückschreiben,
  Animation läuft über dem formatierten Dokument). → WP-F bis WP-H
- **Stufe 3:** Bilder/Scans — User gibt Bild herein (Clipboard/Drag&Drop),
  lokale OS-OCR (Apple Vision / Windows.Media.Ocr), Ausgabe als
  geschwärzter Text und geschwärztes Bild (schwarze Balken). → WP-I, WP-J

### 1.2 Bewusst verworfen (nicht wieder vorschlagen)

- **Screenshot des aktiven Fensters + OCR** („Screenshot-Theater"):
  bräuchte Screen-Recording-Permission — für ein Privacy-Tool untragbar.
- **Nicht-fokussierendes interaktives HUD:** Windows-Fokus-Restore-Tanz,
  NSPanel-Key-Handling — zu komplex fürs Ziel.
- **Schwärzen „im" fremden App-Fenster:** fremdes Rendering ist nicht
  manipulierbar; Koordinaten-Overlays sind pro App unterschiedlich kaputt.

### 1.3 Rahmenbedingungen (gelten für alle WPs)

- **Kein Netz.** Die App stellt keinerlei Outbound-Verbindungen her. Keine
  CDN-Assets, keine Remote-Fonts, keine Remote-Bilder (Stufe 2: Tracking-
  Pixel in HTML-Mails!). CSP in `src-tauri/tauri.conf.json` bleibt strikt.
- **Strict-Mode-Semantik:** Im Strict-Mode wird **nie** ein Mapping
  gespeichert. Die Ablage speichert grundsätzlich **nur die geschwärzte
  Fassung** — nie Originaltext.
- **Bestehender Hotkey-Flow bleibt unangetastet.** `hotkey.rs::handle`
  wird nicht umgebaut; die Bühne ist ein paralleler Pfad.
- **Sprache:** UI-Texte deutsch, „personenbezogene Daten" statt „PII"
  (Beta-Learning). Code-Kommentare deutsch, Stil der Nachbarschaft.
- **Qualitäts-Gate pro WP:** `cd src-tauri && cargo test && cargo clippy
  -- -D warnings && cargo fmt --check`; Frontend: `npm run build` grün.
- Tauri 2, Svelte 5 im **Legacy-Modus** (keine Runes), Rust-Backend.
  Frontend-Struktur: `src/App.svelte` (eine Datei, Karten-Layout),
  `src/Onboarding.svelte`, `src/styles.css`.

---

## 2. Verträge (verbindlich für alle WPs)

### 2.1 Settings-Erweiterung (`src-tauri/src/settings.rs`)

Drei neue Felder im bestehenden `Settings`-Struct, alle mit
`#[serde(default…)]` (alte settings.json muss weiter laden — es gibt
Tests, die das prüfen; analog `enable_ner`):

```rust
/// Hotkey für „Markierung schwärzen & ablegen" (Capture → Bühne).
/// Default: CmdOrCtrl+Alt+Shift+B. Leerer String = Feature deaktiviert.
#[serde(default = "default_stage_hotkey")]
pub stage_hotkey: String,          // default_stage_hotkey() -> "CmdOrCtrl+Alt+Shift+B".into()

/// Animations-Stil der Bühne: "slow" | "normal" | "fast" | "off".
/// Wird nur vom Frontend interpretiert; Backend reicht durch.
/// (Historischer Wert "full" wird vom Frontend als "normal" behandelt.)
#[serde(default = "default_stage_animation")]
pub stage_animation: String,       // default_stage_animation() -> "normal".into()

/// Ablage-Einträge bei App-Quit löschen (Session-only-Ablage).
#[serde(default)]
pub stash_clear_on_quit: bool,
```

### 2.2 Event `stage://job` (Backend → Frontend, Main-Window)

Emittiert vom Capture-Flow, nachdem Detection + Ablage-Eintrag + Clipboard-
Write abgeschlossen sind. Das Frontend animiert nur noch — es trifft keine
Entscheidungen und ruft für den Job keine weiteren Commands auf.

```jsonc
{
  "job_id": "c3f9…",              // case_id des Forwards, bzw. UUID im Strict-Mode
  "mode": "reversible",           // "reversible" | "strict"
  "stash_id": 42,                 // ID des bereits angelegten Ablage-Eintrags; null wenn 0 Findings
  "finding_count": 3,
  "truncated": false,             // true, wenn Anzeige-Segmente gekürzt wurden (Cap s. u.)
  "segments": [                   // Anzeige-Reihenfolge = Array-Reihenfolge
    { "kind": "text", "content": "Sehr geehrter " },
    { "kind": "finding", "original": "Herr Müller", "replacement": "«P_a4b»",
      "entity_type": "person", "confidence": 0.93 },
    { "kind": "text", "content": ", Ihre IBAN " }
  ]
}
```

**Warum Segmente statt Offsets:** `Finding.start/end` sind Byte-Offsets in
UTF-8; JavaScript rechnet in UTF-16-Code-Units. Das Backend schneidet den
Text deshalb selbst in Segmente — im Frontend gibt es **keine**
Offset-Arithmetik. Segment-Bau: Text an den Finding-Grenzen splitten
(Findings sind nach `start` sortiert und überlappungsfrei — das garantiert
die bestehende Detection).

**Anzeige-Cap:** `segments` deckt maximal die ersten **8 000 Zeichen**
Originaltext ab (danach `truncated: true` und ein abschließendes
Text-Segment `"… [gekürzt]"`). Ablage/Clipboard enthalten immer den
vollständigen geschwärzten Text; der Cap betrifft nur die Anzeige.

### 2.3 Neue Tauri-Commands (in `src-tauri/src/main.rs` registriert)

```rust
stash_list() -> Vec<StashMeta>
// StashMeta { id: i64, created_at: String /* ISO-8601 UTC */, mode: String,
//             title: String, entity_counts: HashMap<String, u32>, char_len: usize }

stash_get_text(id: i64) -> Result<String, String>   // geschwärzter Volltext
stash_copy(id: i64) -> Result<(), String>           // schreibt Volltext ins System-Clipboard
stash_delete(id: i64) -> Result<(), String>
stash_clear() -> usize                              // löscht alle, liefert Anzahl
```

`title` = erste 60 Zeichen des **geschwärzten** Texts, Whitespace
kollabiert. `entity_counts` = Map `entity_type → Anzahl` (snake_case-Werte
wie in `detection::EntityType::as_str`, z. B. `"person"`, `"iban"`).

### 2.4 Ablage-Tabelle (`src-tauri/src/storage.rs`, gleiche SQLCipher-DB)

```sql
CREATE TABLE IF NOT EXISTS stash (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at    DATETIME DEFAULT CURRENT_TIMESTAMP,
    mode          TEXT NOT NULL,            -- 'reversible' | 'strict'
    title         TEXT NOT NULL,
    redacted_text TEXT NOT NULL,
    entity_counts TEXT NOT NULL             -- JSON-Objekt {"person":2,"iban":1}
);
```

Es wird **nur geschwärzter Text** gespeichert — kein Original, dadurch
Strict-Mode-kompatibel und ohne eigenen Retention-Zwang (im reversiblen
Modus hängt die Rück-Übersetzbarkeit an der `mappings`-Tabelle, die der
bestehenden Retention unterliegt). Löschung: manuell, `stash_clear`,
optional bei Quit (`stash_clear_on_quit`).

### 2.5 Capture-Verhalten (Backend-Flow, neues Modul `src-tauri/src/stage.rs`)

1. Clipboard-Inhalt VORHER lesen und merken (`prev`).
2. Synthetisches **Strg+C / Cmd+C** via enigo — gleiche Robustheits-Sequenz
   wie `hotkey.rs::send_paste` (alle Modifier explizit releasen, jedes
   enigo-Result loggen). **Vorher auf das PHYSISCHE Loslassen der
   Hotkey-Modifier warten** (macOS: `NSEvent.modifierFlags`-Polling, max.
   1 s) — noch gedrückte Tasten kombinieren sich auf HID-Ebene mit dem
   injizierten Event, die Ziel-App sieht dann `Cmd+Option+Shift+C` statt
   `Cmd+C` und kopiert nichts (Beta-Befund 2026-07-05; die synthetischen
   Release-Events neutralisieren physisch gehaltene Tasten NICHT).
3. **Auf Änderung pollen, mit Retry:** bis zu 3 Copy-Versuche à 450 ms
   Poll-Budget (30-ms-Intervall). Änderung = Text unterscheidet sich von
   `prev` (oder `prev` war leer und jetzt ist Text da).
4. **Fallback statt Fehler:** Ändert sich nichts (nichts markiert,
   Terminal-App, Copy-Verbot), wird der **vorhandene** Clipboard-Inhalt
   verwendet — damit funktioniert auch der Flow „erst normal kopieren,
   dann Capture-Hotkey". Ist das Clipboard auch leer/nicht-Text →
   Fenster zeigen mit Fehler-State (Event mit `segments: []`,
   `finding_count: 0`, `stash_id: null`).
5. Größen-Cap wie im Hotkey-Pfad: > 10 MB → Fehler-State, keine Detection.
6. Detection wie in `hotkey.rs::decide_action`, aber **nur Forward** (die
   Bühne macht kein Reverse): Strict-Mode → `detection::detect_strict` +
   `apply_strict_with_hint`, kein Mapping. Sonst → frische `case_id` via
   `secrets::new_case_id()`, `detection::detect_with_case`,
   `storage::record` pro Finding, `apply_tokens_with_hint`.
7. Geschwärzten Text **sofort** ins Clipboard schreiben (nicht auf die
   Animation warten — Animation ist reine Anzeige, Clipboard muss auch
   stimmen, wenn der User sofort weiterarbeitet). Kein Restore von `prev`
   (konsistent zur dokumentierten No-Restore-Philosophie in `hotkey.rs`).
8. Ablage-Eintrag anlegen (bei ≥ 1 Finding), Event `stage://job` an das
   Main-Window emitten, Fenster zeigen: `show()` + `unminimize()` +
   `set_focus()`.
9. 0 Findings: kein Ablage-Eintrag, Clipboard unverändert lassen, Event
   trotzdem senden (`finding_count: 0`) — die Bühne zeigt „Keine
   personenbezogenen Daten gefunden".

**Einstiege ohne Hotkey** (alle laufen ab Schritt 5, ohne synthetisches
Copy — ein Klick nimmt der Quell-App bereits den Fokus, die Markierung
wäre nicht mehr abholbar):
- **Button im Fenster** „Zwischenablage schwärzen" (Command
  `stage_clipboard`) — Dock-Klick holt das Fenster, ein Klick schwärzt.
- **Text-Drag&Drop ins Fenster** (Command `stage_text`): markierten Text
  aus beliebiger App hereinziehen — der Text kommt im HTML5-Drop-Event
  mit, ohne Clipboard. Dafür ist `dragDropEnabled: false` am Main-Window
  gesetzt (Tauri würde Drops sonst abfangen); Stufe 3 (Datei-Drops) muss
  Dateien deshalb ebenfalls über HTML5 (`dataTransfer.files`) entgegennehmen.
- **Tray-Menü** „Zwischenablage schwärzen" (kann auf macOS ausgeblendet
  sein, wenn die Menüleiste voll ist — deshalb ist der Fenster-Button der
  primäre Klick-Einstieg).
- **Schwebendes Widget** (macOS, opt-in via `show_widget`): kleines
  Always-on-top-Panel mit Marker-Button (`src-tauri/src/widget.rs`,
  `src/Widget.svelte`, Fenster-Label `widget`). Läuft als
  **nicht-aktivierendes NSPanel** (Laufzeit-`object_setClass` auf NSPanel
  + `nonactivatingPanel`-Mask + `becomesKeyOnlyIfNeeded`, Mechanik dem
  Apache-2.0-Plugin tauri-nspanel nachempfunden, bewusst OHNE die
  Dependency) — Klick nimmt der Quell-App den Fokus nicht, darum kann er
  den **vollen Capture-Flow inkl. synthetischem Cmd+C** auslösen (Command
  `stage_capture`). Transparenz via setOpaque/clearColor/KVC statt
  tauri-Feature `macos-private-api`. Position wird bei Moved gemerkt und
  am Exit persistiert (`widget_position`). Windows-Pendant
  (`WS_EX_NOACTIVATE`) ist offener Ausbau.
(Ein macOS-Services-Menü-Eintrag „auf Markierung anwenden" bleibt als
späterer nativer Ausbau denkbar — Services liefert Markierungen ohne
synthetische Tastendrücke.)

### 2.6 Frontend-Komponenten-Vertrag

`src/MarkerText.svelte` — die wiederverwendbare Animations-Komponente
(sie wandert in Stufe 2/3 unverändert weiter):

- Props: `segments` (Array wie in 2.2), `animation` (`"slow" | "normal" |
  "fast" | "off"`; Legacy-Wert `"full"` wird als `"normal"` behandelt).
- Verhalten: Text-Segmente normal rendern; Finding-Segmente zeigen erst
  `original`, dann fährt ein Marker-Strich darüber (Balken in fertiger Form,
  per `clip-path`-Wipe von links aufgedeckt — NICHT per `scaleX` gestreckt,
  sonst zerrt die Kanten-Textur bei breiten Fundstellen; leichte Rotation
  ≈ −0,5°, ungleichmäßig gerundete Enden, Textur mit festen Pixelmaßen),
  anschließend Crossfade zu `replacement`.
  Staffelung pro Finding, Wellen-Parallelisierung ab ~10 Findings,
  **Gesamtbudgets:** `"slow"` ≤ 3,5 s · `"normal"` ≤ 2,2 s · `"fast"`
  ≤ 0,7 s · `"off"`: sofort Endzustand.
  Endzustand: deckender Balken bleibt stehen, `replacement` in Weiß darauf
  (Original bleibt unsichtbar im Fluss → konstante Breite, kein Reflow).
  Alle Balken sind einheitlich schwarz und IMMER voll deckend — keine
  Confidence-Differenzierung in der Optik (Maintainer-Entscheidung, 2026-07-05:
  gestrichelte/gestreifte/hellere Varianten lasen sich als Löcher bzw.
  Unruhe und brachen die Marker-Metapher). `confidence` bleibt im Payload
  und als `title`-Attribut erhalten.
  `prefers-reduced-motion` → wie `"off"`.
- Event: `on:done` wenn die Animation durch ist.
- Muss mit Mock-Daten standalone laufen (keine Tauri-API-Aufrufe in der
  Komponente selbst).

### 2.7 Capability

`src-tauri/capabilities/default.json` gilt fürs `main`-Window und deckt
`core:default` (Events) bereits ab. **Es wird kein zweites Fenster
angelegt** — die Bühne ist eine View im bestehenden Main-Window.

---

## 3. Arbeitspakete Stufe 1 (MVP)

Reihenfolge/Abhängigkeiten:

```
WP-A (Backend Capture)  ─┐
WP-B (Backend Ablage)   ─┼─→ WP-D (Frontend-Integration) ─→ WP-E (Integration/Docs)
WP-C (Animation)        ─┘
```

WP-A, WP-B, WP-C sind **parallel** startbar (Verträge in Abschnitt 2
entkoppeln sie). WP-D braucht C zwingend und A/B zum End-to-End-Testen.

---

### WP-A — Backend: Capture-Flow + Hotkey-Registrierung

**Lesen:** Abschnitt 1 + 2 · `src-tauri/src/hotkey.rs` (Vorbild für
enigo-Sequenz, Größen-Cap, decide_action-Pfade) · `src-tauri/src/settings.rs`
· `src-tauri/src/clipboard.rs` (nur die Cross-Platform-Helpers ab
„Cross-Platform Helpers") · in `src-tauri/src/main.rs` die Regionen
Hotkey-Registrierung/`setup()` (ca. Zeilen 229–560) und Command-Registrierung.
**Nicht lesen:** detection.rs-Interna, ner.rs, storage.rs-Interna,
Frontend.

**Liefern:**
1. `src-tauri/src/stage.rs` — Capture-Flow exakt nach Vertrag 2.5.
   Entscheidungslogik (Fallback, Caps, Forward-Pfad) als reine, testbare
   Funktionen ausgelagert (Vorbild: `decide_action` in hotkey.rs); die
   enigo-/Clipboard-Seite dünn halten. Segment-Bau (2.2) inklusive —
   Unit-Tests mit Umlauten/Emoji (UTF-8-Grenzen!), 0 Findings,
   Truncation-Cap.
2. Settings-Felder aus 2.1 inkl. Tests (Default-Werte, Legacy-JSON lädt).
3. Registrierung des zweiten Hotkeys in `main.rs` analog zum bestehenden
   (gleicher `ShortcutState::Pressed`-Guard). Kollision beachten: ist
   `stage_hotkey` == `hotkey` oder ungültig → loggen, Feature aus, App
   startet trotzdem.
4. Fenster-Vorholen (`show/unminimize/set_focus`) + Event-Emission.
   Ablage-Schreiben über die von WP-B bereitgestellte Funktion
   `storage::stash_insert(mode, title, redacted_text, entity_counts) -> i64`
   — bis WP-B gemerged ist, hinter `todo!()`-freiem Stub (Trait nicht
   nötig, einfach Funktionssignatur als Platzhalter, Merge-Konflikt ist
   trivial).

**Akzeptanz:** Unit-Tests für Segment-Bau + Entscheidungslogik grün;
manuell: Markierung in TextEdit/Editor → Hotkey → Fenster kommt, Event
im Devtools-Log sichtbar, Clipboard enthält geschwärzten Text.

---

### WP-B — Backend: Ablage (Storage + Commands)

**Lesen:** Abschnitt 1 + 2 (v. a. 2.3, 2.4) · `src-tauri/src/storage.rs`
(Schema-Init, CONN-Pattern, Test-Aufbau) · in `src-tauri/src/main.rs` nur
den Command-Block (ca. Zeilen 45–226) und den
`invoke_handler`-Aufruf. **Nicht lesen:** hotkey.rs, detection.rs, ner.rs,
Frontend.

**Liefern:**
1. `stash`-Tabelle in `init_schema` (2.4) — `CREATE TABLE IF NOT EXISTS`,
   keine Migration nötig.
2. `storage::stash_insert(...) -> i64` sowie Funktionen hinter den
   Commands aus 2.3; Commands in `main.rs` registrieren. `stash_copy`
   nutzt `clipboard::write_clipboard_text`.
3. `stash_clear_on_quit`: im bestehenden Exit-/Cleanup-Pfad (dort, wo
   Session-only-Mappings gelöscht werden) die Ablage leeren, wenn das
   Setting an ist.
4. Unit-Tests: insert→list→get→delete-Roundtrip, title-Kürzung (60
   Zeichen, Whitespace kollabiert, Umlaute), entity_counts-JSON-Roundtrip,
   clear.

**Akzeptanz:** Tests grün, Commands per `invoke` aufrufbar (kurzer
Smoke über die bestehende UI-Konsole genügt).

---

### WP-C — Frontend: Marker-Animations-Komponente

**Lesen:** Abschnitt 1 + 2 (v. a. 2.2, 2.6) · `src/styles.css` ·
den `<style>`-Block von `src/App.svelte` (Design-Sprache: Karten, Farben,
Radii). **Nicht lesen:** Rust-Code. **Kein** Tauri-API-Import in der
Komponente.

**Liefern:**
1. `src/MarkerText.svelte` exakt nach Vertrag 2.6. Svelte 5 Legacy-Modus
   (keine Runes — `export let`, `createEventDispatcher`).
2. Marker-Optik: Filzstift-Metapher ernst nehmen — satter dunkler Strich,
   minimale Rotation, raue Kanten, Strich „zieht" von links (scaleX-
   Animation), danach Crossfade zum Ersatztext in `<code>`-Optik analog
   bestehender Token-Darstellung. Entity-Typ als `title`-Attribut.
3. Demo-Harness `src/MarkerTextDemo.svelte` mit 3 Mock-Payloads
   (kurzer Text/3 Findings · langer Text/15 Findings · 0 Findings),
   Buttons zum Abspielen je Animations-Stil. Erreichbar über einen
   Querystring-Schalter in `src/main.ts` (`?demo=marker`), der statt
   `App.svelte` den Harness mountet — kein Eingriff in App.svelte nötig.
4. `prefers-reduced-motion`, Tastatur-los, keine Interaktion — reine
   Anzeige. Performance: 15 Findings ohne Jank (nur `transform`/`opacity`
   animieren, keine Layout-Properties).

**Akzeptanz:** `npm run build` grün; Demo-Harness zeigt alle drei
Payloads/Stile; Gesamtdauer-Budgets eingehalten.

---

### WP-D — Frontend: Bühne + Ablage + View-Integration

**Lesen:** Abschnitt 1 + 2 · `src/App.svelte` komplett ·
`src/MarkerText.svelte` (nur Props/Events, aus WP-C) · `src/main.ts`.
**Nicht lesen:** Rust-Code außer den Command-Signaturen in 2.3.

**Liefern:**
1. Leichtes View-Konzept in `App.svelte`: `view: "status" | "stage" |
  "stash"` (kein Router). Kopfzeile mit Umschaltern „Status ·
  Ablage"; die Bühne öffnet sich nur event-getrieben.
2. `src/StageView.svelte`: lauscht via `listen("stage://job", …)` (Import
   aus `@tauri-apps/api/event`, Listener in App.svelte registrieren und
   Payload als Prop reichen — nur ein Listener-Ort). Rendert
   `MarkerText` mit `settings.stage_animation`; nach `on:done`:
   Erfolgszeile „N Stellen geschwärzt — liegt im Clipboard und in der
   Ablage", Buttons „Nochmal kopieren" (`stash_copy`), „Zur Ablage",
   „Schließen" (zurück zur vorherigen View). States: 0 Findings
   („Keine personenbezogenen Daten gefunden — Clipboard unverändert"),
   Fehler-State (leer/zu groß), `truncated`-Hinweis.
3. `src/StashView.svelte`: Liste via `stash_list` (neueste zuerst) —
   Titel, Datum (lokalisiert), Chips pro Entity-Typ mit Anzahl
   (Badge-Optik wie `status-badge`), Aktionen Kopieren/Löschen je
   Eintrag, „Alle löschen" (danger-Button, wie „Jetzt alle Mappings
   löschen"). Leerer Zustand mit Ein-Satz-Erklärung des Capture-Hotkeys
   (aus `settings.stage_hotkey` via bestehendem `prettyHotkey`).
4. Settings-Karte ergänzen: Animations-Stil (3 Radios), Capture-Hotkey
   (kuratierte Optionen analog `HOTKEY_OPTIONS`; Default + 1 Alternative
   `CmdOrCtrl+Alt+G`; Hinweis „App-Neustart nötig"),
   `stash_clear_on_quit`-Checkbox. Speichern über bestehendes
   `update_settings`-Muster inkl. `saveError`-Handling.

**Akzeptanz:** `npm run build` grün; End-to-End mit WP-A/B: Capture →
Fenster → Animation → Ablage-Eintrag sichtbar, Kopieren/Löschen
funktioniert; alle drei States der Bühne erreichbar.

---

### WP-E — Integration, Tests, Doku

**Lesen:** Abschnitt 1 + 2 · Diffs/Ergebnisse von WP-A–D · `README.md` ·
`CHANGELOG.md` · `MAC_SETUP.md` (nur „UX-Erwartung"-Abschnitt).

**Liefern:**
1. End-to-End-Verifikation auf der Dev-Plattform (macOS): beide Hotkeys
   parallel registriert, Capture aus TextEdit/Mail/Browser, Strict-Mode-
   Durchlauf (kein Mapping entsteht — `get_storage_status` vorher/nachher),
   Session-only + `stash_clear_on_quit`.
2. Randfälle gezielt: nichts markiert (Fallback auf Clipboard), leeres
   Clipboard, 10-MB-Cap, Terminal als Quelle (Strg+C-Konflikt →
   dokumentierte Empfehlung: dort erst kopieren, dann Hotkey).
3. `CHANGELOG.md` (Unreleased) + `README.md` (Bedienung: zweiter Workflow,
   Ablage, Screenshot-Platzhalter) + kurzer Abschnitt in `MAC_SETUP.md`
   falls Accessibility-Implikationen (synthetisches Cmd+C nutzt dieselbe
   bestehende Permission — verifizieren und dokumentieren).
4. Offene Punkte als GitHub-Issue-Entwürfe (Text reicht): Windows-
   Verifikation, Stufe 2, Stufe 3.

---

## 4. Arbeitspakete Stufe 2 (Formatierung) — nach MVP-Merge

Verträge-Erweiterung (verbindlich, sobald Stufe 2 startet):

- **2.2+** Job-Payload erhält `content_kind: "plain" | "html"`. Bei
  `"html"`: statt `segments` das Feld `annotated_html` — sanitisiertes
  HTML, in dem jede Fundstelle bereits als
  `<span data-sz-finding data-original="…" data-replacement="…"
  data-entity-type="…" data-confidence="…">Original</span>` markiert ist.
  MarkerText animiert dann diese Spans (Komponente erweitert, Vertrag 2.6
  bleibt für plain unverändert).
- **2.4+** `stash` erhält Spalten `content_kind TEXT NOT NULL DEFAULT
  'plain'` und `redacted_html TEXT NULL` (Migration: `ALTER TABLE`,
  idempotent absichern). `stash_copy` schreibt bei `html` beide Flavors.

### WP-F — Backend: Rich-Clipboard-IO (plattformspezifisch)

HTML-Flavor lesen/schreiben in den bestehenden Plattform-Modulen:
`src-tauri/src/clipboard/macos_impl.rs` (`public.html` via NSPasteboard)
und `windows_impl.rs` (`CF_HTML` inkl. Header-Offsets `StartHTML/EndHTML…`
— das Format ist fummelig, Fixture-Tests mit echten Word/Outlook-Payloads).
Neue Helpers `read_clipboard_html() -> Option<String>` und
`write_clipboard_html(html, text_fallback) -> Result<(), String>`
(schreibt beide Flavors atomar). Capture-Flow (stage.rs) bevorzugt HTML,
fällt auf Text zurück.

### WP-G — Backend: HTML-Sanitizing + Finding-Mapping (reine Logik)

Neues Modul `src-tauri/src/richtext.rs`, komplett ohne OS-Abhängigkeit —
das testbarste Paket:
1. **Sanitize** (ammonia-Crate): Allowlist üblicher Struktur-/Format-Tags,
   Inline-Styles auf sichere Subset-Properties filtern, `script/iframe/
   form` raus, **alle Remote-Referenzen entfernen** (img src http(s),
   `background:url(…)` — No-Outbound-Versprechen). `data:`-Images bis
   256 KB behalten, sonst durch Platzhalter ersetzen.
2. **Mapping:** DOM parsen (kuchiki o. ä.), Textknoten in Dokumentordnung
   konkatenieren (Block-Grenzen als `\n`), Detection auf dem Plaintext,
   Findings zurück auf Textknoten-Ranges mappen. Finding über
   Knotengrenzen (z. B. `Max <b>Müller</b>`): Ersetzung in den ersten
   Knoten, Rest-Anteile leeren. Output: (a) `annotated_html` für die
   Anzeige, (b) `redacted_html`, (c) `redacted_text` (Plaintext-Ableitung).
3. Golden-Tests: Word-typisches HTML, geschachtelte Formatierung,
   Finding über Tag-Grenze, Tracking-Pixel fliegt raus, data-URI bleibt.

### WP-H — Frontend: Rich-Rendering + Animation über HTML

`MarkerText.svelte` erweitern: bei `content_kind: "html"` das
`annotated_html` rendern (`{@html}` ist okay — Input ist backend-
sanitisiert, CSP blockt Remote als zweite Verteidigungslinie) und die
`data-sz-finding`-Spans mit derselben Marker-Animation überziehen
(nach Mount per `querySelectorAll`, Animation via injizierten Klassen).
StashView zeigt `content_kind`-Badge; Kopieren-Button-Text „Mit
Formatierung kopieren".

---

## 5. Arbeitspakete Stufe 3 (Bilder/OCR) — nach Stufe 2

Verträge-Erweiterung: `content_kind: "image"`; Job-Payload bekommt
`image_path` (temporäre PNG-Kopie im App-Datenverzeichnis) +
`boxes: [{x,y,w,h, entity_type, replacement}]` (normierte Koordinaten
0–1); `stash` speichert Pfad des **geschwärzten** PNG (Original-Bild wird
nach Verarbeitung gelöscht).

### WP-I — Backend: OCR-Adapter + Bild-Pipeline

- Trait `OcrEngine { fn recognize(&self, png: &[u8]) -> Result<Vec<OcrWord>, String> }`
  mit `OcrWord { text: String, bbox: Rect }`.
- macOS: Vision (`VNRecognizeTextRequest`, `recognitionLanguages =
  ["de-DE","en-US"]`) via objc2-Bindings. Windows: `Windows.Media.Ocr`
  via windows-Crate. Beide lokal, kein Feature-Flag nötig (System-APIs,
  kein Modell-Download).
- Pipeline: Bild aus Clipboard/Drop → OCR → Wortfolge zu Text (Zeilen-
  Heuristik der jeweiligen API nutzen) → bestehende Detection → Findings
  auf Wort-Boxen mappen (mehrwortige Entities = Box-Union pro Zeile) →
  geschwärztes PNG rendern (schwarze Balken, image-Crate; Re-Encode
  strippt EXIF) → geschwärzter Text zusätzlich wie gehabt.
- **Ehrlichkeits-Pflicht:** Payload-Flag `ocr_based: true`; was OCR nicht
  erkennt, bleibt sichtbar — das UI muss warnen (WP-J).

### WP-J — Frontend: Bild-Bühne

Bild anzeigen, schwarze Balken animiert einblenden (gleiche Marker-Sprache,
Balken statt Strich), darunter der geschwärzte Text. Deutlicher
Warnhinweis: „Automatisch geschwärzt — bitte visuell prüfen, bevor du das
Bild teilst." Drag&Drop-Zone in der Bühne (`tauri://drag-drop`-Events)
als zweiter Eingang neben dem Capture-Hotkey.

---

## 6. Offene Entscheidungen (bei Bedarf den Maintainer fragen, nicht raten)

1. **Capture-Hotkey-Default** `CmdOrCtrl+Alt+Shift+B` — Kollisionscheck
   in Office/Browsern steht aus (WP-E verifiziert; Alternative
   `CmdOrCtrl+Alt+G`).
2. **Ablage-Retention:** aktuell „manuell + optional bei Quit". Falls
   Beta-Feedback nach Auto-Ablauf verlangt → eigenes Setting, nicht an
   Mapping-Retention koppeln.
3. **Onboarding:** neuer Wizard-Step für den zweiten Hotkey ist bewusst
   NICHT Teil von Stufe 1 (Wizard-Änderung = eigener PR nach Beta-Feedback).
