# Tokcat Windows Port

## Decision

Windows support lives in separate repo:

- macOS repo: `handlecusion/tokcat`
- Windows repo: `handlecusion/tokcat-window`
- local path: `/Users/ys/Code/tokcat-window`

Reason: current Tokcat repo is not neutral cross-platform source. It has macOS
release assumptions, Homebrew tap automation, DMG cleanup, macOS private API,
`NSStatusBar` tray optimization, and macOS popover/window behavior. Windows
needs separate installer, signing, updater endpoint, tray behavior, startup
integration, and QA matrix. Keeping repo separate prevents release workflow
coupling while Windows support is still uncertain.

## Non-goal for first phase

Do not make upstream `handlecusion/tokcat` build every target in one workflow.
That can happen later if Windows port becomes stable and source divergence stays
small. First phase proves Windows distribution by itself.

## Current source baseline

This repo starts as a copy of `handlecusion/tokcat` at commit:

```text
e6943a0fe82d452c534ed20fb60374d83810b357
```

Keep upstream remote so UI and log-reader changes can be merged deliberately.
Do not pull upstream blindly after Windows-specific code starts landing.

Expected remotes:

```text
origin   git@github.com:handlecusion/tokcat-window.git
upstream git@github.com:handlecusion/tokcat.git
```

## Target support

| Platform | Status target | Distribution | Updater key |
| --- | --- | --- | --- |
| Windows x64 | primary Windows target | GitHub Release installer | `windows-x86_64` |
| Windows arm64 | not first phase | none | none |
| macOS arm64 | upstream repo owns it | DMG + Homebrew | `darwin-aarch64` |
| macOS x64 | upstream decision, not here | upstream only | `darwin-x86_64` if added |

## Release ownership

Windows repo owns:

- Windows GitHub Actions workflow
- Windows installer artifact upload
- Windows `latest.json`
- Windows updater endpoint
- Windows code-signing setup
- Windows install/uninstall docs

macOS repo owns:

- DMG build
- DMG `.VolumeIcon.icns` cleanup
- Homebrew cask bump
- macOS updater manifest
- macOS Gatekeeper/Homebrew notes

## Tauri 2 distribution facts

Checked against current Tauri 2 docs before writing this plan.

Relevant rules:

- Windows release workflow should run on `windows-latest`.
- Tauri updater static JSON uses platform keys in `OS-ARCH` form.
- Windows x64 updater key is `windows-x86_64`.
- `bundle.createUpdaterArtifacts` must be enabled for updater signatures.
- Windows build can produce NSIS and/or MSI installer artifacts.
- Tauri v2 updater can reuse Windows installer artifacts and emits matching
  `.sig` files.
- `latest.json` must include valid `url` and signature content for every
  platform key it declares. Bad or incomplete entries can break validation
  before version comparison.

Useful docs:

- `https://v2.tauri.app/distribute/pipelines/github/`
- `https://v2.tauri.app/plugin/updater/`
- `https://v2.tauri.app/distribute/sign/windows/`

## Recommended Windows release shape

Use separate tag flow in this repo:

```text
push tag v<VERSION> -> GitHub Actions on windows-latest -> build installer ->
generate latest.json -> create GitHub Release
```

Recommended first artifact:

- NSIS `.exe` installer
- matching `.exe.sig`
- `latest.json`

MSI can wait unless there is a specific enterprise/install-management need.
NSIS is enough to prove direct user install + Tauri updater first.

Do not update Homebrew tap from this repo.

## Required GitHub secrets

Minimum for updater:

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` if key has password

Later for Windows Authenticode signing, choose one path:

- PFX certificate path:
  - `WINDOWS_CERTIFICATE`
  - `WINDOWS_CERTIFICATE_PASSWORD`
- Azure Trusted Signing path:
  - Azure auth secrets
  - Trusted Signing endpoint/account/profile values

Updater signing and Authenticode signing solve different problems:

- Tauri updater signing: app verifies downloaded update integrity.
- Authenticode signing: Windows trust/SmartScreen reputation path.

Unsigned Windows installer may work technically, but user trust and SmartScreen
warnings remain unresolved.

## Config changes expected

`src-tauri/tauri.conf.json`:

- Change updater endpoint to `handlecusion/tokcat-window`.
- Add Windows bundle target, likely `nsis` first.
- Keep `createUpdaterArtifacts` enabled.
- Add Windows signing config only after signing method chosen.
- Re-check window config fields that are macOS-only or ignored on Windows:
  `transparent`, `decorations`, `skipTaskbar`, `visibleOnAllWorkspaces`,
  `windowEffects`.

`src-tauri/Cargo.toml`:

- Remove `macos-private-api` feature for Windows build path if it blocks build.
- Keep macOS-only dependencies under `cfg(target_os = "macos")`.
- Add Windows-specific deps only when implementation needs native Windows API.
- Re-evaluate vendored `tray-icon` patch. It exists for macOS performance; if it
  complicates Windows build, remove patch in this repo or isolate it.

`.github/workflows/release.yml`:

- Replace macOS runner with `windows-latest`.
- Remove DMG cleanup.
- Remove Homebrew tap job.
- Upload Windows installer, signature, and `latest.json`.
- Validate package/Cargo/Tauri version match tag.

README:

- Mark this repo as Windows port repo.
- Do not keep macOS-only install instructions as primary Windows path.

## Implementation risks

### Tray behavior

Current app is designed as macOS menu bar popover. Windows equivalent is system
tray plus app window. The UX should be validated, not assumed identical.

Risk areas:

- left click vs right click behavior
- taskbar visibility
- popover placement near tray icon
- multi-monitor placement
- always-on-top behavior
- transparent/frosted window support
- tray animation CPU cost

### Native macOS code

Source has macOS-only native tray path using AppKit/Objective-C APIs. It is
`cfg(target_os = "macos")`, but surrounding assumptions can still leak into
shared code.

First Windows build should identify whether failures are:

- Rust compile failures
- Tauri config validation failures
- missing icon formats
- plugin support differences
- runtime window/tray behavior

### Autostart

macOS uses LaunchAgent through Tauri autostart plugin. Windows startup behavior
must be tested separately. Do not claim parity until enable/disable survives:

- normal install
- app update
- uninstall
- user account without admin rights

### Local log paths

Many supported AI coding tools use different config/log paths across OSes.
Windows port must verify each client path instead of assuming Unix home layout.

Minimum first pass:

- Claude Code
- Codex CLI
- Cursor
- Gemini CLI
- Copilot CLI

Other clients can stay "unknown on Windows" until real paths are confirmed.

### Updater

Windows updater endpoint must not reuse macOS `latest.json`. If shared endpoint
ever returns both macOS and Windows platforms, every listed platform entry must
have valid URL and signature content.

Simplest first phase: Windows repo publishes Windows-only `latest.json`.

## Rollout plan

### Phase 0: repo setup

- Create `handlecusion/tokcat-window`.
- Set `origin` to Windows repo and `upstream` to macOS repo.
- Add this document and Windows repo agent guide.

Exit: repo exists and documents ownership clearly.

### Phase 1: build discovery

- Run install/build on a Windows machine or `windows-latest`.
- Capture first build blockers.
- Fix only build blockers, not UX polish.

Exit: `npx tauri build` produces a Windows installer artifact.

### Phase 2: installer release

- Add tag-driven release workflow.
- Upload NSIS artifact.
- Generate `latest.json` with `windows-x86_64`.
- Keep Authenticode optional for this phase, but document warning.

Exit: clean GitHub Release can install app on Windows x64.

### Phase 3: updater

- Verify installed app checks `tokcat-window` endpoint.
- Verify update from version N to N+1.
- Verify signature failure blocks bad update.

Exit: Windows app can self-update from GitHub Release.

### Phase 4: trust and UX

- Add Windows code signing.
- Resolve SmartScreen/reputation path as much as available.
- Tune tray click/window placement/autostart.
- Confirm local log paths for supported clients.

Exit: user-facing beta acceptable.

## Open questions

- Keep product name `Tokcat`, or use installer/display name `Tokcat Windows`
  during beta?
- Same updater signing key as macOS, or separate key per repo/platform?
- NSIS only, or NSIS + MSI from first public release?
- Public repo from day one, or private until first successful Windows install?
- Support Windows 10, Windows 11, or Windows 11 only?
- Does Windows port keep all 3D/dashboard UI immediately, or ship reduced
  feature set until native behavior is stable?

## Recommended default answers

Unless project owner decides otherwise:

- Product name: `Tokcat`
- Repo name: `tokcat-window`
- First platform: Windows x64 only
- Installer: NSIS only
- Updater key: reuse existing Tauri updater key for speed, rotate later only if
  repo/platform separation becomes security requirement
- Visibility: public, because upstream Tokcat source is already public
- Windows support label: beta until signed installer + updater + tray behavior
  are verified on real Windows
