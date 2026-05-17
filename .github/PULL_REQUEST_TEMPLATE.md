## Summary

<!-- Was macht dieser PR? 1-3 Sätze. -->

## Motivation

<!-- Warum dieser Change? Bug-Reference, Use-Case, Performance-Problem … -->

## Testing

- [ ] `cargo test --bin streichzeug --locked` grün
- [ ] `cargo clippy --bin streichzeug --locked -- -D warnings` grün
- [ ] `cargo fmt --check` grün
- [ ] Bei UI-Änderungen: `cargo tauri dev` lokal getestet
- [ ] Bei Detection-Änderungen: mindestens ein neuer Test-Case in `src-tauri/src/detection.rs`

## Checkliste

- [ ] Alle Commits sind mit `Signed-off-by:` versehen (DCO, siehe [`CONTRIBUTING.md`](../CONTRIBUTING.md))
- [ ] Keine API-Keys, Mail-Adressen, Personennamen oder andere personenbezogenen Daten im Code/Tests/Doku
- [ ] Falls neue Dependencies dazukommen: in `CONTRIBUTING.md`-Diskussion erwähnt oder vorab Issue eröffnet

## Screenshots / Demo

<!-- Optional, aber sehr hilfreich bei UI-Änderungen oder visuellen Bug-Fixes. -->
