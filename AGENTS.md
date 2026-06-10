# AGENTS.md

Project-specific instructions for Codex working on Tokcat Windows.

## What this is

Tokcat Windows is a separate working repository for bringing Tokcat to Windows.
It starts from `handlecusion/tokcat` source, but release ownership is separate.

## Repository boundary

- `origin` must point to `handlecusion/tokcat-window`.
- `upstream` should point to `handlecusion/tokcat`.
- Do not push Windows release experiments to `handlecusion/tokcat`.
- Keep macOS release/Homebrew logic in the upstream macOS repo unless a shared
  source change truly belongs there.

## Current porting intent

The goal is not "one repo builds every OS" yet. The goal is a Windows-focused
port with its own release workflow, installer assets, updater endpoint, signing
story, and implementation notes.

Prefer small porting checkpoints:

1. Build copied source on Windows.
2. Remove or gate macOS-only behavior.
3. Produce unsigned Windows installer artifacts.
4. Produce updater artifacts and `latest.json`.
5. Add Windows code signing only after unsigned release flow is proven.

## Distribution rules

- No Homebrew tap for Windows.
- Use GitHub Releases from `handlecusion/tokcat-window`.
- Use a separate Tauri updater endpoint:
  `https://github.com/handlecusion/tokcat-window/releases/latest/download/latest.json`.
- Keep Tauri updater signing separate from Windows Authenticode signing.
- Do not claim SmartScreen-friendly distribution until Windows code signing is
  configured and tested.

## Documentation

`docs/windows-port.md` is the source of truth for current Windows support
strategy. Update it when release workflow, installer choice, updater shape, or
major implementation assumptions change.
