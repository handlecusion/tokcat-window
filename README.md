# Tokcat Windows

Tokcat Windows is the Windows tray-app port of
[`handlecusion/tokcat`](https://github.com/handlecusion/tokcat). This repo owns
Windows builds, GitHub Release installer artifacts, updater metadata, and
Windows-specific implementation notes.

The canonical Windows support document is
[`docs/windows-port.md`](docs/windows-port.md).

## Status

Windows x64 is the primary target.

Current migration state:

- Tauri bundle target: NSIS installer
- Release runner: `windows-latest`
- Updater manifest endpoint:
  `https://github.com/handlecusion/tokcat-window/releases/latest/download/latest.json`
- Updater platform key: `windows-x86_64`
- Windows Authenticode signing: not configured yet

Unsigned installers can run, but Windows SmartScreen reputation is not solved
until Authenticode signing is configured and tested.

## What Tokcat Does

Tokcat reads local AI coding tool usage logs and shows token usage in a native
Windows tray dashboard.

Supported readers include:

- Claude Code
- OpenAI Codex CLI
- Cursor compatibility cache
- OpenCode
- Gemini CLI
- GitHub Copilot CLI
- Amp
- Droid
- Hermes

Tokcat does not send token history to Tokcat servers and has no Tokcat account,
analytics, telemetry, or cloud sync. Network access is limited to update checks
against this repo's GitHub Release updater manifest and direct Codex/Claude
quota lookups when local credentials are available.

## Install

Download the latest Windows installer from:

```text
https://github.com/handlecusion/tokcat-window/releases/latest
```

Use the `Tokcat_*_x64-setup.exe` asset. Matching `.sig` and `latest.json` assets
are for the Tauri updater.

## Development

Requirements:

- Node.js 20+
- Rust stable
- Windows WebView2 runtime
- On non-Windows hosts, `cargo-xwin`, `lld`, and NSIS `makensis` for
  cross-compiling Windows installer builds

Install dependencies:

```sh
npm ci
```

Run frontend-only dev server:

```sh
npm run dev
```

Run Tauri dev app:

```sh
npm run tauri:dev
```

Build on Windows:

```sh
npm run tauri:build
```

Cross-build Windows x64 from macOS/Linux:

```sh
npm run tauri:build:windows
```

Windows artifacts are expected under:

```text
src-tauri/target/release/bundle/nsis/
```

For cross-builds, artifacts are expected under:

```text
src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/
```

## Release

Pushing a `vX.Y.Z` tag builds the Windows installer on `windows-latest`, creates
Tauri updater artifacts, generates `latest.json`, and publishes a GitHub
Release in `handlecusion/tokcat-window`.

Required GitHub secrets:

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` if the key is password-protected

No Homebrew tap is updated from this repo.

Local fallback:

```sh
scripts/release.sh 0.1.30 "Release notes"
```

## Repository Boundary

- `origin`: `handlecusion/tokcat-window`
- `upstream`: `handlecusion/tokcat`

Do not publish Windows experiments to the upstream macOS repo.
