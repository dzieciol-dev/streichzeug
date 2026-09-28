#!/usr/bin/env bash
# Richtet die GitHub-Secrets für macOS-Signing + Notarisierung ein
# (siehe .github/workflows/release.yml, Schritt „Configure macOS signing").
#
# Fragt alles einzeln ab. Passwörter werden verdeckt eingelesen und per
# stdin an `gh secret set` übergeben — sie erscheinen weder auf dem
# Bildschirm noch in Kommandozeilen-Argumenten oder der Shell-History.
# Der globale gh-Account wird NICHT umgestellt (Token nur für diesen Lauf).
#
# Aufruf:  bash scripts/setup-macos-signing.sh
set -euo pipefail

REPO="dzieciol-dev/streichzeug"
GH_USER="dzieciol-dev"
DEFAULT_TEAM_ID="8GN3ZSH2WZ"

bold() { printf '\n\033[1m%s\033[0m\n' "$1"; }
info() { printf '  %s\n' "$1"; }
fail() { printf '\n\033[31mAbbruch:\033[0m %s\n' "$1" >&2; exit 1; }
pause() { read -r -p "  Weiter mit Enter … " _; }

# ---------------------------------------------------------------- 0. Vorab
bold "Streichzeug – macOS-Signing einrichten"
info "Ziel-Repo: $REPO"
info "Das Skript setzt 6 GitHub-Secrets. Abbrechen jederzeit mit Ctrl+C."

command -v gh >/dev/null || fail "GitHub-CLI (gh) fehlt."
command -v openssl >/dev/null || fail "openssl fehlt."
GH_TOKEN="$(gh auth token --user "$GH_USER" 2>/dev/null)" \
  || fail "Kein gh-Login für $GH_USER gefunden (gh auth login)."
export GH_TOKEN
gh repo view "$REPO" >/dev/null 2>&1 || fail "Kein Zugriff auf $REPO mit $GH_USER."
info "GitHub-Zugriff als $GH_USER: ok"

# ------------------------------------------------ 1. Zertifikat vorhanden?
bold "Schritt 1 von 4: Developer-ID-Zertifikat"
has_devid() {
  local ids
  ids="$(security find-identity -v -p codesigning 2>/dev/null || true)"
  [[ "$ids" == *"Developer ID Application"* ]]
}
while ! has_devid; do
  info "Im Schlüsselbund ist noch kein „Developer ID Application“-Zertifikat."
  info "So legst du es an:"
  info "  1. Xcode öffnen → Menü Xcode → Settings → Accounts"
  info "  2. Dein Team auswählen → „Manage Certificates …“"
  info "  3. Unten links „+“ → „Developer ID Application“"
  read -r -p "  Xcode jetzt öffnen? [j/N] " ans
  [[ "$ans" =~ ^[jJyY]$ ]] && open -a Xcode || true
  info "Wenn das Zertifikat angelegt ist:"
  pause
done
info "Zertifikat im Schlüsselbund gefunden:"
security find-identity -v -p codesigning | grep "Developer ID Application" \
  | sed -E 's/^ *[0-9]+\) [0-9A-F]{40} /    /'

# ------------------------------------------------------ 2. .p12 exportieren
bold "Schritt 2 von 4: Zertifikat als .p12-Datei"
info "So exportierst du es:"
info "  1. App „Schlüsselbundverwaltung“ öffnen → links „Anmeldung“ → oben „Meine Zertifikate“"
info "  2. Rechtsklick auf „Developer ID Application: …“ → „… exportieren“"
info "  3. Format „Privater Informationsaustausch (.p12)“, ein Passwort vergeben"
info "  4. Datei + Passwort danach in 1Password ablegen"
read -r -p "  Schlüsselbundverwaltung jetzt öffnen? [j/N] " ans
[[ "$ans" =~ ^[jJyY]$ ]] && open -a "Keychain Access" || true

while :; do
  read -r -p "  Pfad zur .p12-Datei (Datei ins Fenster ziehen, dann Enter): " P12
  P12="${P12%"${P12##*[![:space:]]}"}"   # trailing spaces from drag&drop
  P12="${P12//\\ / }"                     # escaped spaces
  P12="${P12#\'}"; P12="${P12%\'}"
  [[ -f "$P12" ]] && break
  info "Datei nicht gefunden: $P12"
done

while :; do
  read -r -s -p "  Passwort der .p12-Datei (Eingabe unsichtbar): " P12_PASSWORD; echo
  export P12_PASSWORD
  # Keychain-Exporte nutzen oft ältere Verschlüsselung → notfalls -legacy.
  SUBJECT="$(openssl pkcs12 -in "$P12" -passin env:P12_PASSWORD -nokeys -clcerts 2>/dev/null \
             | openssl x509 -noout -subject -nameopt utf8,sep_multiline 2>/dev/null \
          || openssl pkcs12 -legacy -in "$P12" -passin env:P12_PASSWORD -nokeys -clcerts 2>/dev/null \
             | openssl x509 -noout -subject -nameopt utf8,sep_multiline 2>/dev/null || true)"
  [[ -n "$SUBJECT" ]] && break
  info "Passwort passt nicht (oder Datei ist keine .p12). Nochmal."
done

SIGNING_IDENTITY="$(printf '%s\n' "$SUBJECT" | sed -n 's/^ *CN=//p' | head -1)"
TEAM_ID="$(printf '%s\n' "$SUBJECT" | sed -n 's/^ *OU=//p' | head -1)"
[[ "$SIGNING_IDENTITY" == "Developer ID Application:"* ]] \
  || fail "Die .p12 enthält „$SIGNING_IDENTITY“, kein Developer-ID-Application-Zertifikat."
TEAM_ID="${TEAM_ID:-$DEFAULT_TEAM_ID}"
info "Erkannt: $SIGNING_IDENTITY"
info "Team-ID: $TEAM_ID"

# ------------------------------------------------ 3. Notarisierung (Apple)
bold "Schritt 3 von 4: Zugang für die Notarisierung"
read -r -p "  Apple-ID (E-Mail deines Developer-Kontos): " APPLE_ID
[[ "$APPLE_ID" == *@* ]] || fail "Das sieht nicht nach einer E-Mail-Adresse aus."

info "Jetzt ein app-spezifisches Passwort anlegen:"
info "  appleid.apple.com → Anmeldung und Sicherheit → App-spezifische Passwörter → „+“"
info "  Name z. B. „Streichzeug Notarisierung“. Es hat die Form xxxx-xxxx-xxxx-xxxx."
read -r -p "  Seite jetzt im Browser öffnen? [j/N] " ans
[[ "$ans" =~ ^[jJyY]$ ]] && open "https://account.apple.com/account/manage" || true
while :; do
  read -r -s -p "  App-spezifisches Passwort (Eingabe unsichtbar): " APPLE_PASSWORD; echo
  [[ "$APPLE_PASSWORD" =~ ^[a-z]{4}-[a-z]{4}-[a-z]{4}-[a-z]{4}$ ]] && break
  info "Format passt nicht (erwartet xxxx-xxxx-xxxx-xxxx, Kleinbuchstaben). Nochmal."
done

# ------------------------------------------------------ 4. Secrets setzen
bold "Schritt 4 von 4: Secrets in GitHub eintragen"
info "Gesetzt werden: APPLE_CERTIFICATE, APPLE_CERTIFICATE_PASSWORD,"
info "APPLE_SIGNING_IDENTITY, APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID"
read -r -p "  Jetzt eintragen? [j/N] " ans
[[ "$ans" =~ ^[jJyY]$ ]] || fail "Nichts eingetragen."

set_secret() { printf '%s' "$2" | gh secret set "$1" -R "$REPO" >/dev/null && info "✓ $1"; }
base64 -i "$P12" | tr -d '\n' | gh secret set APPLE_CERTIFICATE -R "$REPO" >/dev/null \
  && info "✓ APPLE_CERTIFICATE"
set_secret APPLE_CERTIFICATE_PASSWORD "$P12_PASSWORD"
set_secret APPLE_SIGNING_IDENTITY "$SIGNING_IDENTITY"
set_secret APPLE_ID "$APPLE_ID"
set_secret APPLE_PASSWORD "$APPLE_PASSWORD"
set_secret APPLE_TEAM_ID "$TEAM_ID"
unset P12_PASSWORD APPLE_PASSWORD

bold "Fertig."
info "Secrets im Repo (nur Namen):"
gh secret list -R "$REPO" | cut -f1 | sed 's/^/    /'
info "Die .p12-Datei kannst du löschen, sobald sie in 1Password liegt:"
info "    $P12"
