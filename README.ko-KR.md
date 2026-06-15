# Tokcat Windows

Tokcat Windows는
[`handlecusion/tokcat`](https://github.com/handlecusion/tokcat)의 Windows tray
app 포트입니다. 이 repo는 Windows 빌드, GitHub Release installer artifact,
updater metadata, Windows 구현 메모를 소유합니다.

기준 문서: [`docs/windows-port.md`](docs/windows-port.md)

## 상태

Windows x64가 1차 지원 대상입니다.

현재 migration 상태:

- Tauri bundle target: NSIS installer
- Release runner: `windows-latest`
- Updater manifest endpoint:
  `https://github.com/handlecusion/tokcat-window/releases/latest/download/latest.json`
- Updater platform key: `windows-x86_64`
- Windows Authenticode signing: 아직 미설정

Unsigned installer는 기술적으로 실행될 수 있지만, Authenticode signing을
설정하고 검증하기 전까지 SmartScreen 친화 배포라고 말하지 않습니다.

## 기능

Tokcat은 로컬 AI coding tool usage log를 읽고 Windows tray dashboard에서 token
usage를 보여줍니다.

지원 reader:

- Claude Code
- OpenAI Codex CLI
- Cursor compatibility cache
- OpenCode
- Gemini CLI
- GitHub Copilot CLI
- Amp
- Droid
- Hermes

Tokcat은 token history를 Tokcat server로 보내지 않고, Tokcat account, analytics,
telemetry, cloud sync가 없습니다. Network access는 이 repo의 GitHub Release
updater manifest 확인과 로컬 credential이 있을 때 Codex/Claude quota 조회로
제한됩니다.

## 설치

최신 Windows installer:

```text
https://github.com/handlecusion/tokcat-window/releases/latest
```

`Tokcat_*_x64-setup.exe` asset을 사용합니다. `.sig`와 `latest.json`은 Tauri
updater용입니다.

## 개발

필요:

- Node.js 20+
- Rust stable
- Windows WebView2 runtime
- macOS/Linux에서 Windows installer를 cross-build할 경우 `cargo-xwin`, `lld`,
  NSIS `makensis`

의존성 설치:

```sh
npm ci
```

Frontend-only dev server:

```sh
npm run dev
```

Tauri dev app:

```sh
npm run tauri:dev
```

Windows host에서 build:

```sh
npm run tauri:build
```

macOS/Linux에서 Windows x64 cross-build:

```sh
npm run tauri:build:windows
```

Windows artifact 예상 위치:

```text
src-tauri/target/release/bundle/nsis/
```

Cross-build artifact 예상 위치:

```text
src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/
```

## Release

`vX.Y.Z` tag push 시 `windows-latest`에서 Windows installer를 빌드하고, Tauri
updater artifact와 `latest.json`을 생성한 뒤 `handlecusion/tokcat-window`
GitHub Release로 publish합니다.

필수 GitHub secrets:

- `TAURI_SIGNING_PRIVATE_KEY`
- key에 password가 있으면 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

이 repo는 Homebrew tap을 업데이트하지 않습니다.
