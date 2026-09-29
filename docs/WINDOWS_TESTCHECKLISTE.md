# Windows-Testcheckliste

Manuelle Prüfung eines Releases auf einem echten Windows-PC (Issue #21).
Dauer: ca. 30–45 Minuten. Ergebnis je Punkt abhaken oder mit kurzer
Notiz versehen; bei Fehlern das Log beilegen (Punkt 0.3).

**Getestete Version:** ______ **Windows-Version:** ______ (Einstellungen →
System → Info) **Tastatur:** Deutsch / andere: ______

## Testtext

Für alle Tests diesen fiktiven Text verwenden (in Editor oder Word
einfügen):

```
Sehr geehrte Frau Schäfer,

vielen Dank für Ihre Nachricht vom 12.03.2026. Wie besprochen leitet
Herr Dr. Max Mustermann-Berg, Leiter Kommunikation der Beispiel GmbH in
Köln, die Unterlagen an Anna und Thomas weiter.

Rückfragen gern an max.mustermann@example.de oder 0221 1234567.
Bankverbindung: DE89 3704 0044 0532 0130 00

Mit freundlichen Grüßen
Gabriele Kroll
```

---

## 0. Installation und Start

- [ ] **0.1** MSI aus den GitHub-Releases laden, Doppelklick.
  SmartScreen-Warnung → „Weitere Informationen" → „Trotzdem ausführen".
  Notieren: Kam die Warnung? Welcher Wortlaut?
- [ ] **0.2** Installation läuft durch, App startet, Icon erscheint unten
  rechts im Infobereich (ggf. unter „^").
- [ ] **0.3** Tray-Menü → „Log-Ordner öffnen" öffnet
  `%LOCALAPPDATA%\de.streichzeug.app\logs\`. Dort liegt `app.log` mit
  einer Startzeile inkl. Versionsnummer.
- [ ] **0.4** Einrichtungsassistent erscheint beim ersten Start und lässt
  sich bis zum Ende durchklicken.
- [ ] **0.5** Anmeldeinformationsverwaltung (Windows-Suche:
  „Anmeldeinformationsverwaltung") → „Windows-Anmeldeinformationen":
  Es gibt einen Eintrag mit `de.streichzeug.app`.

## 1. Smart-Paste (Hauptfunktion)

- [ ] **1.1** Testtext markieren, Strg+C. In einem leeren Editor-Fenster
  **Strg+Alt+B** drücken. Eingefügt wird der Text mit Pseudonymen
  (`«P_…»`, `«E_…»` …). Notieren, was **nicht** ersetzt wurde.
- [ ] **1.2** Den eingefügten Text mit Pseudonymen markieren, Strg+C,
  woanders Strg+Alt+B: Die Originale kommen zurück (Rückübersetzung).
- [ ] **1.3** Strict-Modus (Tab „Erkennung") einschalten, 1.1 wiederholen:
  lesbare Platzhalter (`«Person A»` …). Danach wieder zurückstellen.
- [ ] **1.4** Text ohne personenbezogene Daten kopieren, Strg+Alt+B: normales
  Einfügen, nichts verändert.
- [ ] **1.5** Strg+Alt+B in Word, Outlook, Browser (z. B. Eingabefeld von
  ChatGPT/Claude) und Teams ausprobieren. Notieren, wo es **nicht** geht
  oder wo Strg+Alt+B etwas anderes auslöst (Strg+Alt = AltGr auf deutscher
  Tastatur).

## 2. Erweiterte Erkennung (KI-Modell)

- [ ] **2.1** Tab „Erkennung" → aktivieren. Download (~145 MB) läuft durch,
  danach Status „geladen/bereit". Ein erfolgreicher Download bestätigt
  zugleich den im Programm hinterlegten Prüfwert des Windows-Pakets.
- [ ] **2.2** Test 1.1 wiederholen: Vornamen (Anna, Thomas) und Personen
  werden jetzt ersetzt. Notieren, was noch stehen bleibt.
- [ ] **2.3** App beenden und neu starten: Erweiterte Erkennung ist weiter
  aktiv, ohne erneuten Download.

## 3. Schwärz-Bühne

- [ ] **3.1** Testtext in Word/Editor **markieren** (nicht kopieren),
  **Strg+Alt+Shift+B**: Die Bühne öffnet sich mit geschwärztem Text.
  Kritisch: Wurde die Markierung zuverlässig übernommen? 5× wiederholen,
  auch zügig nach dem Markieren (Timing des automatischen Strg+C).
- [ ] **3.2** Text kopieren, Tray-Menü → „Zwischenablage schwärzen": Bühne
  öffnet sich mit dem Text.
- [ ] **3.3** Screenshot mit Namen erstellen (Win+Shift+S), dann Tray →
  „Zwischenablage schwärzen": Bild mit schwarzen Balken über den Namen.
  Notieren, was die Texterkennung übersieht.
- [ ] **3.4** Bilddatei (PNG/JPG mit Text) ins App-Fenster ziehen: wie 3.3.
- [ ] **3.5** Tab „Ablage": Die Einträge aus 3.1–3.4 sind da, „Nochmal
  kopieren" funktioniert.

## 4. Einstellungen und Abschluss

- [ ] **4.1** Hotkey im Tab „Einstellungen" auf die Alternative umstellen,
  funktioniert sofort; danach zurückstellen.
- [ ] **4.2** Windows neu starten: Startet Streichzeug automatisch mit
  (erwartet/gewünscht? notieren).
- [ ] **4.3** Deinstallation über „Apps & Features": App verschwindet, Icon
  weg. (Daten unter `%APPDATA%` und `%LOCALAPPDATA%\de.streichzeug.app`
  bleiben bewusst liegen.)

## Ergebnis

- Blocker (App für Teilnehmende nicht nutzbar): ______
- Störend, aber umgehbar: ______
- Log beigelegt: ja / nein
