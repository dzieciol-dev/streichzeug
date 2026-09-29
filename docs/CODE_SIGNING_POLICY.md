# Code-Signing-Policy

Diese Policy beschreibt, welche Streichzeug-Dateien signiert werden, wer
Signaturen freigibt und welche Zusagen für signierte Programme gelten.

## Windows

Der Windows-Installer ist derzeit nicht signiert. Geplant ist eine
Signatur mit einem auf den Entwickler ausgestellten Code-Signing-
Zertifikat; diese Policy wird dann um Aussteller und Ablauf ergänzt.

## macOS

Die macOS-Bundles (DMG) werden mit einer Apple Developer ID signiert und
von Apple notarisiert.

## Was signiert wird

- Ausschließlich Release-Artefakte, die aus einem Tag dieses
  Repositories gebaut werden (macOS: GitHub-Actions-Pipeline
  `.github/workflows/release.yml`). Entwicklungs-Builds werden nicht
  signiert.
- Produktname und Version in den Datei-Metadaten entsprechen dem Release
  (`Streichzeug`, Version aus `tauri.conf.json`).
- Fremdbibliotheken, die zur Laufzeit nachgeladen werden (ONNX Runtime,
  NER-Modell), werden nicht mitsigniert. Sie kommen unverändert aus der
  Originalquelle und werden vor der Nutzung gegen im Programm hinterlegte
  SHA-256-Werte geprüft.

## Rollen

| Rolle | Wer |
|---|---|
| Committer und Reviewer | [@dzieciol-dev](https://github.com/dzieciol-dev) |
| Approver (Freigabe jedes Releases) | [@dzieciol-dev](https://github.com/dzieciol-dev) |

Externe Beiträge werden vor dem Merge von einem Reviewer geprüft. Jede
Signaturanfrage wird einzeln von einem Approver freigegeben. Alle
Teammitglieder nutzen Zwei-Faktor-Authentifizierung für GitHub.

## Datenschutz

Streichzeug überträgt keine Daten an Dritte, ohne dass der Nutzer es
ausdrücklich auslöst. Erkennung und Pseudonymisierung laufen vollständig
lokal. Die einzige Netzverbindung ist der optionale, vom Nutzer
gestartete Download des Erkennungsmodells (Hugging Face) und der ONNX
Runtime (GitHub); dabei werden keine Inhalte aus der Zwischenablage
übertragen. Es gibt keine Telemetrie.

## Lizenz

Streichzeug ist Open Source unter `MIT` oder `Apache-2.0` (nach Wahl)
und enthält keine proprietären Bestandteile.
