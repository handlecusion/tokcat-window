#!/usr/bin/env bash
# Build and publish a Tokcat Windows release.
#
# Usage:
#   scripts/release.sh <version> "<release notes>"
#
# Prerequisites:
#   - gh CLI authenticated for handlecusion/tokcat-window
#   - Updater private key at ~/.tauri/tokcat.key or
#     TAURI_SIGNING_PRIVATE_KEY_PATH
#   - package.json / Cargo.toml / tauri.conf.json already bumped to <version>
#     and committed on main.
#   - On non-Windows hosts, cargo-xwin available for cross-compiling.

set -euo pipefail

VERSION="${1:-}"
NOTES="${2:-}"

if [[ -z "$VERSION" || -z "$NOTES" ]]; then
  echo "usage: $0 <version> <notes>" >&2
  exit 1
fi

KEY_PATH="${TAURI_SIGNING_PRIVATE_KEY_PATH:-$HOME/.tauri/tokcat.key}"
if [[ ! -f "$KEY_PATH" ]]; then
  echo "signing key not found at $KEY_PATH" >&2
  exit 1
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

PKG_VER="$(node -p "require('./package.json').version")"
CARGO_VER="$(grep -E '^version = ' src-tauri/Cargo.toml | head -1 | sed -E 's/version = "([^"]+)"/\1/')"
TAURI_VER="$(node -p "require('./src-tauri/tauri.conf.json').version")"
if [[ "$PKG_VER" != "$VERSION" || "$CARGO_VER" != "$VERSION" || "$TAURI_VER" != "$VERSION" ]]; then
  echo "version mismatch: requested=$VERSION package.json=$PKG_VER Cargo.toml=$CARGO_VER tauri.conf.json=$TAURI_VER" >&2
  exit 1
fi

TAG="v$VERSION"
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    BUILD_CMD=(npx tauri build)
    BUNDLE_DIR="src-tauri/target/release/bundle"
    ;;
  *)
    BUILD_CMD=(npx tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc)
    BUNDLE_DIR="src-tauri/target/x86_64-pc-windows-msvc/release/bundle"
    ;;
esac

echo "==> Building Windows release with updater artifacts"
TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_PATH")" \
  TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" \
  "${BUILD_CMD[@]}"

NSIS_DIR="$BUNDLE_DIR/nsis"
mapfile -t INSTALLERS < <(find "$NSIS_DIR" -maxdepth 1 -type f -name '*.exe' | sort)
if [[ "${#INSTALLERS[@]}" -ne 1 ]]; then
  echo "expected exactly one NSIS installer in $NSIS_DIR, found ${#INSTALLERS[@]}" >&2
  printf '%s\n' "${INSTALLERS[@]}" >&2
  exit 1
fi

INSTALLER="${INSTALLERS[0]}"
SIG_FILE="${INSTALLER}.sig"
if [[ ! -f "$SIG_FILE" ]]; then
  echo "expected updater signature missing: $SIG_FILE" >&2
  exit 1
fi

INSTALLER_NAME="$(basename "$INSTALLER")"
SIGNATURE="$(cat "$SIG_FILE")"
PUB_DATE="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
DOWNLOAD_BASE="https://github.com/handlecusion/tokcat-window/releases/download/$TAG"
LATEST_JSON="$BUNDLE_DIR/latest.json"

node - "$VERSION" "$NOTES" "$PUB_DATE" "$SIGNATURE" "$DOWNLOAD_BASE/$INSTALLER_NAME" "$LATEST_JSON" <<'NODE'
const fs = require('fs')
const [version, notes, pubDate, signature, url, out] = process.argv.slice(2)
fs.writeFileSync(out, JSON.stringify({
  version,
  notes,
  pub_date: pubDate,
  platforms: {
    'windows-x86_64': { signature, url },
  },
}, null, 2))
NODE

echo "==> latest.json"
cat "$LATEST_JSON"

if ! git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "==> Tagging $TAG"
  git tag "$TAG"
  git push origin "$TAG"
fi

echo "==> Creating GitHub release"
gh release create "$TAG" \
  "$INSTALLER" \
  "$SIG_FILE" \
  "$LATEST_JSON" \
  --title "Tokcat Windows $VERSION" \
  --notes "$NOTES"

echo "==> Done: https://github.com/handlecusion/tokcat-window/releases/tag/$TAG"
