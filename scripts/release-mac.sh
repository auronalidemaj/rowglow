#!/usr/bin/env bash
# Rowglow: signed and notarized macOS release.
#
# Builds Rowglow.app and the .dmg, signs them with your Developer ID, has Apple
# notarize both and staples the tickets, so the app opens on any Mac without a
# Gatekeeper warning (even offline).
#
#   scripts/release-mac.sh --check                    what's set up and what's missing (builds nothing, no network)
#   scripts/release-mac.sh                            build for this Mac's chip (aarch64 on Apple Silicon)
#   scripts/release-mac.sh --target x86_64-apple-darwin      Intel build (needs rustup)
#   scripts/release-mac.sh --target universal-apple-darwin   one DMG for both chips (needs rustup)
#   scripts/release-mac.sh --notarize-dmg FILE.dmg    notarize + staple a DMG that's already built (CI uses this)
#   scripts/release-mac.sh --verify FILE.app|FILE.dmg check the signature, the ticket and Gatekeeper's verdict
#
# Settings come from the environment or scripts/release-mac.env (copy
# release-mac.env.example). One-time Apple setup: docs/MAC-SIGNING.md.
# Works with the bash 3.2 that ships with macOS.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DESKTOP="$ROOT/apps/desktop"
ENV_FILE="${ROWGLOW_RELEASE_ENV:-$ROOT/scripts/release-mac.env}"
# Keychain item holding the app-specific password (Apple ID notarization only).
KEYCHAIN_SERVICE="rowglow-notary"
SETTINGS="ROWGLOW_API_BASE APPLE_SIGNING_IDENTITY APPLE_TEAM_ID APPLE_ID APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH"

if [ -t 1 ]; then B=$'\033[1m' G=$'\033[32m' R=$'\033[31m' Y=$'\033[33m' D=$'\033[2m' N=$'\033[0m'; else B='' G='' R='' Y='' D='' N=''; fi
step() { printf '\n%s==> %s%s\n' "$B" "$*" "$N"; }
ok() { printf '  %s✓%s %s\n' "$G" "$N" "$*"; }
bad() { printf '  %s✗%s %s\n' "$R" "$N" "$*"; FAILED=$((FAILED + 1)); }
warn() { printf '  %s!%s %s\n' "$Y" "$N" "$*"; }
note() { printf '    %s%s%s\n' "$D" "$*" "$N"; }
die() { printf '%serror:%s %s\n' "$R" "$N" "$*" >&2; exit 1; }
FAILED=0

usage() { sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

# --- settings -----------------------------------------------------------------

# KEY=VALUE lines from the env file; variables already set in the environment win.
load_settings() {
  [ -f "$ENV_FILE" ] || return 0
  local line key value
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line#"${line%%[![:space:]]*}"}"
    case "$line" in '' | '#'*) continue ;; esac
    line="${line#export }"
    key="${line%%=*}"
    value="${line#*=}"
    value="${value%"${value##*[![:space:]]}"}"
    case "$value" in \"*\") value="${value#\"}" value="${value%\"}" ;; \'*\') value="${value#\'}" value="${value%\'}" ;; esac
    case "$key" in
      APPLE_PASSWORD | APPLE_CERTIFICATE_PASSWORD | *SECRET* | *TOKEN*)
        die "$(basename "$ENV_FILE") contains $key. Keep passwords out of files: see docs/MAC-SIGNING.md (the keychain item \"$KEYCHAIN_SERVICE\")." ;;
    esac
    case " $SETTINGS " in
      *" $key "*) [ -n "${!key:-}" ] || export "$key=$value" ;;
      *) warn "Ignoring unknown setting $key in $(basename "$ENV_FILE")" ;;
    esac
  done <"$ENV_FILE"
  if [ -n "${APPLE_API_KEY_PATH:-}" ]; then APPLE_API_KEY_PATH="${APPLE_API_KEY_PATH/#\~/$HOME}"; fi
}

# "name<TAB>sha1" for every valid code-signing identity in the keychains.
identities() {
  security find-identity -v -p codesigning 2>/dev/null |
    sed -n 's/^ *[0-9][0-9]*) \([0-9A-F]\{40\}\) "\(.*\)"$/\2	\1/p'
}

# Sets IDENTITY (the full certificate name) or explains what's missing.
resolve_identity() {
  local all devid
  all="$(identities)"
  devid="$(printf '%s\n' "$all" | grep '^Developer ID Application: ' || true)"
  IDENTITY=""
  if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
    IDENTITY="$(printf '%s\n' "$all" | awk -F'\t' -v want="$APPLE_SIGNING_IDENTITY" '$1 == want || $2 == want { print $1; exit }')"
    if [ -z "$IDENTITY" ]; then
      bad "APPLE_SIGNING_IDENTITY isn't a certificate in your keychain: $APPLE_SIGNING_IDENTITY"
      note "List yours with: security find-identity -v -p codesigning"
      return 1
    fi
  else
    case "$(printf '%s' "$devid" | grep -c . || true)" in
      1) IDENTITY="$(printf '%s\n' "$devid" | cut -f1)" ;;
      0) ;;
      *)
        bad "Several Developer ID Application certificates found; set APPLE_SIGNING_IDENTITY to one of:"
        printf '%s\n' "$devid" | cut -f1 | while IFS= read -r n; do note "$n"; done
        return 1
        ;;
    esac
  fi
  case "$IDENTITY" in
    "Developer ID Application: "*)
      ok "Signing certificate: $IDENTITY"
      ;;
    "")
      bad "No \"Developer ID Application\" certificate in your keychain"
      if [ -n "$all" ]; then
        note "You have: $(printf '%s\n' "$all" | cut -f1 | sed 's/:.*//' | sort -u | paste -sd, - | sed 's/,/, /g')."
        note "Those only sign apps for your own Macs or the App Store; they can't be notarized for download."
      fi
      note "Create one (Account Holder only): Xcode → Settings → Accounts → Manage Certificates → + → Developer ID Application"
      return 1
      ;;
    *)
      bad "\"${IDENTITY%%:*}\" certificates can't be notarized for download; use a \"Developer ID Application\" one"
      IDENTITY=""
      return 1
      ;;
  esac
  # The team ID is the part in parentheses at the end of the certificate name.
  if [ -z "${APPLE_TEAM_ID:-}" ]; then
    APPLE_TEAM_ID="$(printf '%s' "$IDENTITY" | sed -n 's/.*(\([A-Z0-9]\{10\}\))$/\1/p')"
  fi
}

# Sets NOTARY_ARGS (for notarytool) and exports what the Tauri bundler reads.
# $1 = "quiet" to skip reading the password from the keychain (--check).
resolve_notary() {
  NOTARY_ARGS=()
  if [ -n "${APPLE_API_KEY:-}${APPLE_API_ISSUER:-}" ]; then
    [ -n "${APPLE_API_KEY:-}" ] || { bad "APPLE_API_ISSUER is set but APPLE_API_KEY (the key ID) isn't"; return 1; }
    [ -n "${APPLE_API_ISSUER:-}" ] || { bad "APPLE_API_KEY is set but APPLE_API_ISSUER (the issuer ID) isn't"; return 1; }
    APPLE_API_KEY_PATH="${APPLE_API_KEY_PATH:-$HOME/.appstoreconnect/private_keys/AuthKey_${APPLE_API_KEY}.p8}"
    if [ ! -f "$APPLE_API_KEY_PATH" ]; then
      bad "API key file not found: $APPLE_API_KEY_PATH"
      note "Put the AuthKey_${APPLE_API_KEY}.p8 you downloaded from App Store Connect there, or set APPLE_API_KEY_PATH."
      return 1
    fi
    export APPLE_API_KEY APPLE_API_ISSUER APPLE_API_KEY_PATH
    unset APPLE_ID APPLE_PASSWORD
    NOTARY_ARGS=(--key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER")
    ok "Notarization: App Store Connect API key $APPLE_API_KEY"
  elif [ -n "${APPLE_ID:-}" ]; then
    [ -n "${APPLE_TEAM_ID:-}" ] || { bad "APPLE_ID is set but APPLE_TEAM_ID isn't (it's on developer.apple.com → Membership)"; return 1; }
    if [ -z "${APPLE_PASSWORD:-}" ]; then
      if ! security find-generic-password -s "$KEYCHAIN_SERVICE" -a "$APPLE_ID" >/dev/null 2>&1; then
        bad "No app-specific password stored for $APPLE_ID"
        note "Create one at account.apple.com → Sign-In and Security → App-Specific Passwords, then run:"
        note "security add-generic-password -s $KEYCHAIN_SERVICE -a $APPLE_ID -w"
        note "(it asks for the password, so it never lands in your shell history)"
        return 1
      fi
      if [ "${1:-}" != quiet ]; then
        APPLE_PASSWORD="$(security find-generic-password -s "$KEYCHAIN_SERVICE" -a "$APPLE_ID" -w)"
      fi
    fi
    export APPLE_ID APPLE_TEAM_ID
    [ -z "${APPLE_PASSWORD:-}" ] || export APPLE_PASSWORD
    NOTARY_ARGS=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "${APPLE_PASSWORD:-}")
    ok "Notarization: Apple ID $APPLE_ID (team $APPLE_TEAM_ID)"
  else
    bad "No notarization credentials"
    note "Set APPLE_API_KEY + APPLE_API_ISSUER (recommended) or APPLE_ID + an app-specific password; see docs/MAC-SIGNING.md"
    return 1
  fi
}

resolve_api_base() {
  case "${ROWGLOW_API_BASE:-}" in
    https://?*) ok "License/update server: $ROWGLOW_API_BASE" ;;
    "")
      bad "ROWGLOW_API_BASE isn't set"
      note "Your website's URL (https://…). It's built into the app for license activation and update checks."
      return 1
      ;;
    *) bad "ROWGLOW_API_BASE must start with https:// (got $ROWGLOW_API_BASE)"; return 1 ;;
  esac
}

host_target() { case "$(uname -m)" in arm64) echo aarch64-apple-darwin ;; *) echo x86_64-apple-darwin ;; esac; }

# The Rust targets a build needs, and whether they're installed.
check_target() {
  local target="$1" need missing="" t
  case "$target" in
    universal-apple-darwin) need="aarch64-apple-darwin x86_64-apple-darwin" ;;
    aarch64-apple-darwin | x86_64-apple-darwin) need="$target" ;;
    *) bad "Unknown target $target (aarch64-apple-darwin, x86_64-apple-darwin or universal-apple-darwin)"; return 1 ;;
  esac
  if [ "$need" = "$(host_target)" ]; then
    ok "Rust target: $target (this Mac)"
    return 0
  fi
  if ! command -v rustup >/dev/null 2>&1; then
    bad "Building $target needs rustup (Homebrew's Rust only builds for this Mac's chip)"
    note "brew uninstall rust && brew install rustup && rustup-init, then: rustup target add $need"
    return 1
  fi
  for t in $need; do
    rustup target list --installed 2>/dev/null | grep -qx "$t" || missing="$missing $t"
  done
  if [ -n "$missing" ]; then
    bad "Missing Rust targets:$missing"
    note "rustup target add$missing"
    return 1
  fi
  ok "Rust target: $target"
}

check_tools() {
  local missing=""
  for t in codesign security spctl hdiutil ditto plutil; do command -v "$t" >/dev/null 2>&1 || missing="$missing $t"; done
  xcrun --find notarytool >/dev/null 2>&1 || missing="$missing notarytool"
  xcrun --find stapler >/dev/null 2>&1 || missing="$missing stapler"
  if [ -n "$missing" ]; then
    bad "Missing Xcode tools:$missing"
    note "Install them with: xcode-select --install"
    return 1
  fi
  ok "Xcode tools: codesign, notarytool $(xcrun notarytool --version 2>/dev/null | head -1), stapler"
}

# --- notarize -----------------------------------------------------------------

# Submits a file, waits for Apple's verdict and prints the log when it's rejected.
notarize() {
  local file="$1" out err status id
  printf '  Uploading %s to Apple (usually 1–10 minutes)…\n' "$(basename "$file")"
  # JSON on stdout; progress and warnings on stderr, kept apart so the JSON parses.
  err="$(mktemp /tmp/rowglow-notary.XXXXXX)"
  out="$(xcrun notarytool submit "$file" "${NOTARY_ARGS[@]}" --wait --output-format json 2>"$err")" || true
  status="$(printf '%s' "$out" | plutil -extract status raw -o - - 2>/dev/null || true)"
  id="$(printf '%s' "$out" | plutil -extract id raw -o - - 2>/dev/null || true)"
  if [ "$status" = "Accepted" ]; then
    rm -f "$err"
    ok "Notarized (submission $id)"
    return 0
  fi
  bad "Apple didn't accept $(basename "$file"): ${status:-no answer}"
  { printf '%s\n' "$out"; cat "$err"; } | grep . | sed 's/^/    /' | tail -20
  rm -f "$err"
  if [ -n "$id" ]; then
    note "Apple's log:"
    xcrun notarytool log "$id" "${NOTARY_ARGS[@]}" 2>&1 | sed 's/^/    /' | head -60 || true
  fi
  return 1
}

dmg_signer() { codesign -dvv "$1" 2>&1 | sed -n 's/^Authority=//p' | head -1; }

# Signs the DMG if the build didn't, then notarizes and staples it.
notarize_dmg() {
  local dmg="$1" signer
  [ -f "$dmg" ] || die "No such DMG: $dmg"
  signer="$(dmg_signer "$dmg")"
  case "$signer" in
    "Developer ID Application: "*) ok "DMG signed by $signer" ;;
    *)
      [ -n "${IDENTITY:-}" ] || resolve_identity || die "The DMG isn't signed and there's no Developer ID certificate to sign it with"
      codesign --force --sign "$IDENTITY" --timestamp "$dmg"
      ok "DMG signed by $IDENTITY"
      ;;
  esac
  notarize "$dmg" || die "Notarization failed"
  xcrun stapler staple -q "$dmg" || die "Couldn't staple the ticket to $dmg"
  ok "Ticket stapled to $(basename "$dmg")"
}

# --- verify -------------------------------------------------------------------

verify_app() {
  local app="$1" info
  if codesign --verify --deep --strict "$app" 2>/dev/null; then ok "Signature intact"; else bad "Signature is broken or missing"; fi
  info="$(codesign -dvv "$app" 2>&1 || true)"
  case "$(printf '%s\n' "$info" | sed -n 's/^Authority=//p' | head -1)" in
    "Developer ID Application: "*) ok "Signed by $(printf '%s\n' "$info" | sed -n 's/^Authority=//p' | head -1)" ;;
    "") bad "Not signed with a certificate (ad-hoc or unsigned)" ;;
    *) bad "Not a Developer ID signature: $(printf '%s\n' "$info" | sed -n 's/^Authority=//p' | head -1)" ;;
  esac
  if printf '%s\n' "$info" | grep -q 'flags=.*runtime'; then ok "Hardened runtime on"; else bad "Hardened runtime off (notarization requires it)"; fi
  if printf '%s\n' "$info" | grep -q '^Timestamp='; then ok "Secure timestamp present"; else bad "No secure timestamp"; fi
  if xcrun stapler validate -q "$app" >/dev/null 2>&1; then ok "Notarization ticket stapled"; else bad "No notarization ticket stapled"; fi
  gatekeeper "$app" execute
}

gatekeeper() {
  local out reason
  if [ "$2" = open ]; then
    out="$(spctl --assess --type open --context context:primary-signature -vv "$1" 2>&1 || true)"
  else
    out="$(spctl --assess --type execute -vv "$1" 2>&1 || true)"
  fi
  reason="$(printf '%s\n' "$out" | sed -n 's/^source=//p' | head -1)"
  case "$out" in
    *": accepted"*"source=Notarized Developer ID"*) ok "Gatekeeper: accepted (Notarized Developer ID)" ;;
    *": accepted"*) warn "Gatekeeper: accepted${reason:+ ($reason)}, but not as a notarized download" ;;
    *) bad "Gatekeeper: rejected${reason:+ ($reason)}; a downloaded copy would be blocked" ;;
  esac
}

verify_dmg() {
  local dmg="$1" signer mnt app
  if codesign --verify --strict "$dmg" 2>/dev/null; then
    signer="$(dmg_signer "$dmg")"
    case "$signer" in
      "Developer ID Application: "*) ok "DMG signed by $signer" ;;
      *) bad "DMG isn't Developer ID signed (${signer:-ad-hoc})" ;;
    esac
  else
    bad "DMG isn't signed"
  fi
  if xcrun stapler validate -q "$dmg" >/dev/null 2>&1; then ok "Notarization ticket stapled to the DMG"; else bad "No notarization ticket stapled to the DMG"; fi
  gatekeeper "$dmg" open
  mnt="$(mktemp -d /tmp/rowglow-verify.XXXXXX)"
  if hdiutil attach -nobrowse -readonly -noverify -mountpoint "$mnt" "$dmg" >/dev/null 2>&1; then
    app="$(find "$mnt" -maxdepth 1 -name '*.app' | head -1)"
    if [ -n "$app" ]; then
      printf '  %sInside: %s%s\n' "$D" "$(basename "$app")" "$N"
      verify_app "$app"
    else
      bad "No .app inside the DMG"
    fi
    [ -L "$mnt/Applications" ] && ok "Applications shortcut present" || warn "No Applications shortcut in the DMG"
    hdiutil detach -quiet "$mnt" || hdiutil detach -force -quiet "$mnt" || true
  else
    bad "Couldn't open the DMG"
  fi
  rmdir "$mnt" 2>/dev/null || true
}

verify() {
  local path="${1%/}"
  [ -e "$path" ] || die "No such file: $path"
  step "Verifying $(basename "$path")"
  case "$path" in
    *.app) verify_app "$path" ;;
    *.dmg) verify_dmg "$path" ;;
    *) die "Expected a .app or a .dmg" ;;
  esac
  if [ "$FAILED" -eq 0 ]; then
    printf '\n%sReady to ship:%s it opens on any Mac without warnings.\n' "$G" "$N"
  else
    printf '\n%s%d check(s) failed.%s\n' "$R" "$FAILED" "$N"
    return 1
  fi
}

# --- main ---------------------------------------------------------------------

MODE=build
TARGET=""
FILE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --check) MODE=check ;;
    --verify) MODE=verify; FILE="${2:-}"; [ -n "$FILE" ] || usage 1; shift ;;
    --notarize-dmg) MODE=notarize-dmg; FILE="${2:-}"; [ -n "$FILE" ] || usage 1; shift ;;
    --target) TARGET="${2:-}"; [ -n "$TARGET" ] || usage 1; shift ;;
    --target=*) TARGET="${1#--target=}" ;;
    -h | --help) usage 0 ;;
    *) printf 'Unknown option: %s\n\n' "$1" >&2; usage 1 ;;
  esac
  shift
done
[ "$(uname -s)" = Darwin ] || die "Mac releases are built on a Mac"
TARGET="${TARGET:-$(host_target)}"
load_settings

case "$MODE" in
  verify)
    verify "$FILE"
    exit $?
    ;;
  notarize-dmg)
    step "Notarizing $(basename "$FILE")"
    check_tools || exit 1
    resolve_notary || exit 1
    notarize_dmg "$FILE"
    FAILED=0
    verify "$FILE"
    exit $?
    ;;
  check)
    printf '%sRowglow Mac release check%s %s(settings: %s)%s\n' "$B" "$N" "$D" "$([ -f "$ENV_FILE" ] && echo "${ENV_FILE#"$ROOT/"}" || echo "environment only")" "$N"
    step "Tools"
    check_tools || true
    command -v pnpm >/dev/null 2>&1 && ok "pnpm $(pnpm --version)" || bad "pnpm isn't installed"
    command -v cargo >/dev/null 2>&1 && ok "$(cargo --version | cut -d' ' -f1-2)" || bad "Rust (cargo) isn't installed"
    check_target "$TARGET" || true
    step "Signing"
    resolve_identity || true
    step "Notarization"
    resolve_notary quiet || true
    step "App settings"
    resolve_api_base || true
    if [ "$FAILED" -eq 0 ]; then
      printf '\n%sAll set.%s Run scripts/release-mac.sh to build, sign and notarize.\n' "$G" "$N"
    else
      printf '\n%s%d thing(s) to set up%s (steps: docs/MAC-SIGNING.md).\n' "$R" "$FAILED" "$N"
      exit 1
    fi
    exit 0
    ;;
esac

# Build: check everything first so a long build never fails at the end.
VERSION="$(plutil -extract version raw -o - "$DESKTOP/src-tauri/tauri.conf.json")"
step "Checking the setup"
check_tools || true
check_target "$TARGET" || true
resolve_identity || true
resolve_notary || true
resolve_api_base || true
[ "$FAILED" -eq 0 ] || die "Fix the items above first (scripts/release-mac.sh --check shows them again)."

BUNDLE="$DESKTOP/src-tauri/target/$TARGET/release/bundle"
step "Building Rowglow $VERSION for $TARGET (signed, notarized by the Tauri bundler)"
[ -d "$DESKTOP/node_modules" ] || (cd "$DESKTOP" && pnpm install --frozen-lockfile)
export APPLE_SIGNING_IDENTITY="$IDENTITY" ROWGLOW_API_BASE
# A leftover APPLE_CERTIFICATE would make the bundler import a different certificate.
unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD
started="$(date +%s)"
(cd "$DESKTOP" && pnpm tauri build --target "$TARGET")

APP="$BUNDLE/macos/Rowglow.app"
DMG="$(ls -t "$BUNDLE/dmg/Rowglow_${VERSION}_"*.dmg 2>/dev/null | head -1 || true)"
[ -d "$APP" ] || die "The build didn't produce $APP"
[ -n "$DMG" ] || die "The build didn't produce a DMG in $BUNDLE/dmg"
[ "$(stat -f %m "$DMG")" -ge "$started" ] || die "$DMG is older than this build"
xcrun stapler validate -q "$APP" >/dev/null 2>&1 ||
  die "The app wasn't notarized (look for \"skipping app notarization\" in the build output above)"
ok "App notarized and stapled by the bundler"

step "Notarizing the DMG"
notarize_dmg "$DMG"

FAILED=0
verify "$DMG"
printf '\n  %s\n  %s\n  SHA-256 %s · %s\n' "$DMG" "$APP" \
  "$(shasum -a 256 "$DMG" | cut -d' ' -f1)" "$(du -h "$DMG" | cut -f1 | tr -d ' ')"
printf '\nNext: upload the DMG (for example to a GitHub release) and publish its URL on your update server.\n'
