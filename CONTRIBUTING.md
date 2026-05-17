# Contributing to Streichzeug

Vielen Dank, dass du beitragen willst.

Streichzeug ist ein lokales Privacy-Tool — Code-Qualität, Privacy-by-Design
und reproduzierbare Builds sind uns wichtiger als Feature-Velocity.

## Lizenz für Beiträge

Streichzeug ist dual-lizenziert unter **`MIT` ODER `Apache-2.0`** (Rust-
Ökosystem-Standard, siehe Lizenz-Sektion im [`README.md`](README.md)).

Wenn du einen PR einreichst, gilt automatisch der Rust-Standard-Wortlaut:

> Unless you explicitly state otherwise, any contribution intentionally
> submitted for inclusion in the work by you, as defined in the
> Apache-2.0 license, shall be dual licensed as above, without any
> additional terms or conditions.

Das heißt: dein Beitrag wird ebenfalls unter `MIT OR Apache-2.0`
lizenziert. Ein separates Contributor License Agreement (CLA) gibt es
nicht.

## Developer Certificate of Origin (DCO)

Zusätzlich verlangen wir DCO-Sign-Off auf jedem Commit. Damit
bestätigst du nach [developercertificate.org](https://developercertificate.org/),
dass du das Recht hast, den Code beizusteuern (z. B. weil du ihn selbst
geschrieben hast und kein Arbeitsvertrag im Wege steht).

Praktisch reicht ein `-s`-Flag bei `git commit`:

```bash
git commit -s -m "Fix: …"
```

Wer's automatisch will: in deiner globalen Git-Konfig

```bash
git config --global format.signoff true
```

Der DCO-Bot prüft jeden PR und blockiert Merge, falls Commits ohne
Sign-off dabei sind.

## Vor einem Pull Request

```bash
# Backend
cd src-tauri
cargo test --bin streichzeug --locked
cargo clippy --bin streichzeug --locked -- -D warnings
cargo fmt --check

# Frontend
cd ..
npm install
npm run check     # svelte-check
npm run build
```

Plus bei substantiellen Detection-Änderungen: bitte mindestens einen Unit-
Test in `src-tauri/src/detection.rs` für den neuen Fall hinzufügen.

## Was wir gerne sehen

- **Bug-Fixes** mit Test-Case der den Bug reproduziert
- **Neue Entity-Typen** für die Detection-Pipeline (z. B. internationale
  Telefonnummern, andere Länder-IBANs, Gesundheits-IDs)
- **Performance-Verbesserungen** mit Benchmark-Vorher/Nachher
- **Plattform-Support** — Linux ist offiziell nicht getestet, Patches
  willkommen
- **Internationalisierung** der UI (aktuell nur DE)
- **Bessere Doku** — README, ARCHITECTURE, Code-Kommentare
- **Reproducible-Build-Setup** (Verify-Build via Cargo + Nix oder
  ähnlich)

## Was wir nicht zusammenführen

- **Telemetrie** jeglicher Art. Die App ist offline-by-design und
  bleibt's.
- **Cloud-Sync** der Mapping-DB. Der lokale Sicherheitsfokus geht vor.
- **Auto-Update über externe Server**. Tauri-Updates kommen, sobald wir
  Code-Signing haben — bis dahin: GitHub-Releases manuell ziehen.
- **Hartkodierte API-Keys / Secrets** in irgendeiner Form.

## Code-Konventionen

- **Rust:** `cargo fmt` + `cargo clippy -- -D warnings`. Default-Stil
  von rustfmt, keine Custom-Rules.
- **Svelte/TS:** keine Custom-Linter, aber bitte konsistent zum
  bestehenden Stil.
- **Commits:** kurze Summary-Zeile (≤ 70 Zeichen), Body mit Begründung.
  Wir bevorzugen *warum* erklärende Commit-Messages über *was*-erklärende.
- **Kommentare:** Code-Comments erklären **warum**, nicht **was**.
  Identifier-Namen erklären das *was* schon.
- **PR-Größe:** lieber mehrere kleine PRs als ein großer.

## Tests laufen lassen

```bash
cd src-tauri
cargo test                                  # alle Unit-Tests
cargo test detection::                      # nur Detection
cargo test --features ner ner::             # NER-Tests (braucht Modell)
```

Frontend-Tests gibt's aktuell nicht — Svelte-Component-Tests sind auf
der Wunschliste.

## Diskussion vor großen Änderungen

Bei größeren Änderungen (neues Modul, Architektur-Wechsel, neue
Dependency) bitte vorher ein [GitHub-Issue](https://github.com/dzieciol-dev/streichzeug/issues/new)
mit dem Vorschlag eröffnen — sonst riskierst du, an etwas zu arbeiten,
was wir aus Gründen nicht aufnehmen wollen.

## Verhaltenskodex

Siehe [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md). Sei nett.
