# Contributing to Tokcat Windows

## Development requirements

- **Node.js** 20 or later
- **Rust** stable (install via [rustup](https://rustup.rs))
- **Windows WebView2 runtime** — pre-installed on Windows 11; on Windows 10 install from [microsoft.com](https://developer.microsoft.com/microsoft-edge/webview2/)

## Running locally

Install dependencies:

```sh
npm ci
```

Frontend-only dev server (no Rust required):

```sh
npm run dev
```

Full Tauri dev app:

```sh
npm run tauri:dev
```

## Building

Windows native build:

```sh
npm run tauri:build
```

NSIS installer and updater artifacts will appear under:

```
src-tauri/target/release/bundle/nsis/
```

Cross-compile from macOS or Linux (requires `cargo-xwin`, `lld`, and NSIS `makensis`):

```sh
npm run tauri:build:windows
```

Cross-build artifacts appear under:

```
src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/
```

## Release process

Releases are published automatically when a `vX.Y.Z` tag is pushed. Before tagging, bump the version in all three files to keep them consistent:

- `package.json` → `version`
- `src-tauri/Cargo.toml` → `[package] version`
- `src-tauri/tauri.conf.json` → `version`

The release workflow validates that all three match the pushed tag and fails early if they diverge.

### Signing key setup

The Tauri updater requires a signing key pair. To generate one:

```sh
npx tauri signer generate -w ~/.tauri/tokcat.key
```

This writes the private key to `~/.tauri/tokcat.key` and prints the public key.

Add these two secrets to the repository (**Settings → Secrets and variables → Actions**):

| Secret | Value |
|--------|-------|
| `TAURI_SIGNING_PRIVATE_KEY` | Contents of `~/.tauri/tokcat.key` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Password you chose (leave empty if none) |

The matching public key must be placed in `src-tauri/tauri.conf.json` under `plugins.updater.pubkey`.

### Triggering a release

Push a version tag:

```sh
git tag v0.1.31
git push origin v0.1.31
```

Alternatively, use **Actions → Release → Run workflow** to manually trigger a build without pushing a tag (useful for testing the pipeline).

## Pull requests

- Open PRs against `main`.
- CI runs TypeScript typecheck, Vite build, and `cargo check` on every push.
- Windows-specific changes only — do not include macOS release automation or Homebrew tap updates (those live in the upstream [`handlecusion/tokcat`](https://github.com/handlecusion/tokcat) repo).
