# Security Policy

## Reporting a Vulnerability

Bitte **keine öffentlichen GitHub-Issues** für Sicherheitslücken anlegen.

Sicherheits-Findings bitte über
[GitHub Private Security Advisories](https://github.com/lurning/streichzeug/security/advisories/new)
melden. Das ist der vertrauliche Kanal für Coordinated Disclosure auf
GitHub — wir bekommen die Meldung, du behältst die Kontrolle über die
Offenlegung.

Wir bestätigen den Eingang spätestens **innerhalb von 5 Werktagen** und
melden eine geplante Patch-Timeline innerhalb von 14 Tagen.

## Was als Sicherheitslücke zählt

- Möglichkeiten, an die Klartext-PII oder den Master-Secret eines anderen
  Users zu kommen
- Pfad-Injektionen, SQL-Injektionen, Buffer-Overflows in unserem Rust-Code
- Supply-Chain-Probleme mit unseren direkten Dependencies (Indirekte:
  bitte trotzdem melden)
- Umgehungen der Pseudonymisierung (z. B. False Negatives, die wir
  übersehen haben)

## Was wir bewusst akzeptieren

Gegen User-Level-Malware mit Memory-Dump-Capability schützen wir uns
nicht — wer einen Memory-Dump der laufenden App ziehen kann, hat ohnehin
Lese-Zugriff auf alles, was im Prozess steht. Defense-in-Depth
(Mapping-DB-Encryption, SecureZeroMemory) ist auf der Roadmap, aktuell
aber noch nicht implementiert.

Siehe README → „Bekannte Schwächen" für die vollständige Liste der
bewusst akzeptierten Restrisiken.

## Scope

Sicherheitsrelevant:
- Code in diesem Repo (`src-tauri/`, `src/`, `scripts/`)
- Binaries, die wir signiert veröffentlichen (sobald wir das tun)

Nicht in Scope:
- Vulnerabilities in Tauri selbst — bitte an
  https://github.com/tauri-apps/tauri reporten
- Vulnerabilities in upstream Crates / npm-Packages — bitte direkt an
  die Maintainer
- Brute-Force auf den HMAC-Master-Secret bei trivialen Eingaben — das
  ist eine inhärente Eigenschaft deterministischer Tokens

## Disclosure-Policy

- **Privater Bug-Report** über GitHub Security Advisories
- Wir koordinieren mit dir einen **Disclosure-Termin**
- Standard: 90 Tage nach Patch-Verfügbarkeit kannst du öffentlich machen,
  oder früher in Absprache
- Wir nennen dich im CHANGELOG.md und in den Release-Notes (oder anonym,
  wenn du willst)

## Belohnung

Aktuell **keine monetäre Bug-Bounty** — das Projekt ist Open-Source-
Beta. Wir sagen dir Danke, nennen dich (falls gewünscht), und nehmen
deine Test-Cases in die Suite auf.
