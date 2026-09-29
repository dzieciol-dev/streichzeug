# Backlog

Gesammelte, noch nicht umgesetzte Punkte. Größere Themen mit eigener
Diskussion stehen zusätzlich als GitHub-Issue (Nummer in Klammern).

Stand: 2026-09-29

---

## Erkennung

Befund aus dem Praxistest mit v0.6.2 (realer Mail-Text): Die Grunderkennung
(Regex + Namenslisten) lässt viele Vornamen stehen. Mit Erweiterter
Erkennung (NER-Modell) wurden alle Personen erkannt — stehen blieb nur eine
Funktionsbezeichnung (Muster „Leiter Kommunikation").

- [ ] **Modell als empfohlener Standard.** Onboarding-Schritt „Erweiterte
  Erkennung" als Hauptweg darstellen (Überspringen als Ausnahme). Solange
  das Modell fehlt, deutlicher Hinweis in der App („Nur Grunderkennung
  aktiv — Vornamen werden oft übersehen") mit Knopf zum Nachladen.
  Kein automatischer Download ohne Zustimmung (Netzverbindung nur mit
  Anlass). Modell weiterhin NICHT ins Bundle: Runtime-Download ist eine
  bewusste Entscheidung (Commit 97b0efc — kein Re-Distributor unter
  AFL-3.0, Installer 4 MB statt 101 MB).
- [ ] **Neue Kategorie „Funktion".** Regelbasierte Erkennung von
  Funktionsbezeichnungen (Leiter/in, Geschäftsführer/in, Vorstand,
  Vorsitzende/r, Referent/in, Präsident/in, Pressesprecher/in, Direktor/in …)
  inkl. Zusatz („… Marktentwicklung und Kommunikation", „… für Recht"),
  Platzhalter `«Funktion A»`. Abschaltbar, weil die Funktion für manche
  Aufgaben inhaltlich gebraucht wird. Hintergrund: Das NER-Modell kennt nur
  PER/ORG/LOC/DATE; Funktion + Organisation identifiziert im Verbandskontext
  aber genau eine Person.
- [ ] **Größere Namenslisten für die Grunderkennung.** Aktuell 222 Vornamen,
  73 Nachnamen, 134 Städte (`gazetteer.rs`). Mehrere tausend Vornamen aus
  offenen Daten (kommunale Vornamens-Statistiken, Lizenz prüfen) plus
  Ausnahmeliste für Namen, die zugleich normale Wörter sind (Mark, Rose,
  Frank, Will …). Braucht ein Testkorpus gegen Fehltreffer.

## Windows

- [ ] **Windows-Code-Signing.** MSI ist unsigniert → SmartScreen-Warnung;
  Installation braucht zudem Adminrechte (auf verwalteten PCs → IT).
  SignPath Foundation hat den Antrag vom 2026-09-29 am selben Tag abgelehnt
  (zu wenig öffentliche Sichtbarkeit: Stars, Forks, Erwähnungen; erneute
  Bewerbung ausdrücklich möglich). Plan (Entscheidung 2026-09-29):
  1. Vorerst unsigniert; Installation für Workshop-Teilnehmende über die IT.
  2. Windows-Test (#21) abwarten.
  3. Bei Bedarf Certum-Open-Source-Zertifikat (ab ca. 69 € inkl.
     Kryptokarte + Leser; Signatur „Open Source Developer, <Name>") und
     lokal am Windows-PC signieren (Tauri `bundle.windows.signCommand`),
     danach `docs/CODE_SIGNING_POLICY.md` ergänzen.
  4. SignPath-Neubewerbung, sobald das Projekt sichtbarer ist.
- [ ] **Manuelle Verifikation auf Windows (#21)** — Ablauf:
  `docs/WINDOWS_TESTCHECKLISTE.md`. Smart-Paste
  (Strg+Alt+B), Schwärz-Bühne (Strg+Alt+Shift+B, Button, Tray, Drag&Drop),
  synthetisches Strg+C (nur fester 150-ms-Puffer), Bild-OCR, NER-Download
  inkl. ZIP-Entpacken und gepinntem Hash des Windows-ORT-Archivs (nur gegen
  den GitHub-Download geprüft, nicht gegen eine laufende Installation),
  Mapping-DB-Schlüssel im Windows-Anmeldeinformationsspeicher.
- [ ] **Widget-Pendant auf Windows (#21).** Schwebender Knopf ist auf
  Windows deaktiviert; nicht-aktivierendes Fenster via
  `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` (`widget.rs`).
- [ ] **Hotkey vs. AltGr.** Strg+Alt entspricht auf deutscher Tastatur AltGr.
  Prüfen, ob Strg+Alt+B in gängigen Apps kollidiert.
- [ ] **OCR-Sprache.** `OcrEngine::TryCreateFromUserProfileLanguages` nutzt
  die Sprachen des Benutzerprofils; fehlt das deutsche OCR-Sprachpaket,
  sinkt die Qualität. Prüfen und ggf. Hinweis in der Bühne.
- [ ] **Optional: winget-Paket** als Gegenstück zum Homebrew-Tap.

### Windows-Signing — Optionen (Stand 2026-09-29)

| Weg | Kosten | Voraussetzungen / Haken |
|---|---|---|
| SignPath Foundation | kostenlos | Abgelehnt 2026-09-29 (Sichtbarkeit); Neubewerbung später möglich |
| Certum Open Source Code Signing | ab ca. 69 € (Satz mit Karte + Leser; Verlängerungspreis prüfen) | Für einzelne OSS-Entwickler; Schlüssel auf Kryptokarte → Signieren lokal am Windows-PC, nicht in der CI |
| Azure Artifact Signing | Monatsabo | Einzelentwickler nur USA/Kanada; EU nur als Organisation mit Registerdaten — für ein Einzelunternehmen ohne HR-Eintrag unsicher |
| Klassisches OV-Zertifikat | ca. 200–400 €/Jahr | Schlüssel auf Hardware-Token/HSM |

Für alle Wege gilt: SmartScreen baut Reputation erst über Downloads auf;
auch signierte Installer können anfangs noch warnen.

## Sonstiges

- [ ] **PDF-Schwärzung (#25).** Echte Entfernung des Texts unter dem Balken
  bei PDFs mit Textebene; gescannte PDFs über OCR.
- [ ] **ONNX-Runtime einbetten und mitsignieren** (macOS). Dann kann das
  Entitlement `disable-library-validation` entfallen. Lizenzfrage wie beim
  Modell vorher klären (ORT: MIT).
- [ ] **`ort` von 2.0.0-rc.10 anheben** (Dependabot #7). API-Drift-Risiko,
  deshalb eigener Schritt mit NER-Diagnose-Tests.
