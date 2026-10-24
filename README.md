# FURSOY Mail

**Experimental Desktop Mail Client & OTP Heuristics Engine (Rust · Tauri 2 · React)**

> [!IMPORTANT]
> **Project Status: Active Work-in-Progress / Experimental (Not Daily-Driver Ready)**  
> FURSOY Mail is an active personal exploration of local-first desktop mail architectures, asynchronous IMAP syncing in Rust, and notification-driven OTP heuristics.  
> **While tagged release builds are available for testing, the client is currently in active development, contains known edge-case bugs, and is NOT ready or recommended for everyday, primary email use.**

FURSOY Mail explores a lightweight, local-first alternative to heavy webmail interfaces: detecting likely verification codes in incoming streams and providing instant copy actions directly from native desktop notifications.

[Download Experimental Preview (Testing Only)](https://github.com/FURSOY/fursoy-mail/releases/latest)

## Highlights

- Fast desktop notifications for new messages
- OTP and verification-code detection with one-click copy
- Notification modes for all mail, OTP-only, or no notifications
- Multiple accounts with account-aware notifications
- Local mail cache and search
- Inbox, archive, sent, spam, and trash views
- Compose, reply, reply all, forward, archive, trash, and restore actions
- Attachment viewing, download, and sending
- `mailto:` links open directly in the FURSOY Mail compose window
- Remote-image controls for email privacy
- Quiet hours, configurable sync intervals, and fullscreen pause
- Theme colors, interface density, and English/Turkish interface options
- Signed in-app update packages distributed through GitHub Releases

## Platform and account support

FURSOY Mail currently supports:

- Windows
- Any mail account reachable over IMAP and SMTP

Setup starts from an email address. Gmail and Outlook sign in through the
browser and connect with OAuth; Yahoo and iCloud use app passwords; other
domains fall back to manual server settings.

Other operating systems are not currently supported.

## Installation

1. Open the [latest release](https://github.com/FURSOY/fursoy-mail/releases/latest) (experimental testing builds).
2. Download the Windows file ending in `x64-setup.exe`.
3. Run the installer and launch FURSOY Mail.
4. Connect a Google account and approve the permissions required by the mail features.

Node.js, Rust, and other developer tools are not required to install or use the released application.

The installer uses Tauri's WebView2 download bootstrapper. An internet connection may be required during installation when Microsoft Edge WebView2 Runtime is not already available on the computer.

Windows SmartScreen may show an unknown-publisher warning while the project does not use a commercial Windows code-signing certificate.

## Privacy

FURSOY Mail connects directly to Google services and does not operate a server that receives or stores mailbox data.

- Mail data is processed and cached locally on the user's device.
- OAuth tokens are stored in the operating system credential store.
- The app does not collect crash reports, diagnostics, usage statistics, analytics, or telemetry.
- Remote email images may contact their original host when allowed by the user's setting.
- Update checks connect to this project's GitHub Releases distribution.

See the full [Privacy Policy](PRIVACY.md) for details.

## Security

Do not disclose suspected vulnerabilities or sensitive user data in a public issue. Read the [Security Policy](SECURITY.md) and report vulnerabilities through [GitHub Private Vulnerability Reporting](https://github.com/FURSOY/FURSOY-Mail/security/advisories/new) or [support@fursoy.com](mailto:support@fursoy.com).

## License

FURSOY Mail is free software licensed under the [GNU General Public License version 3 only](LICENSE) (`GPL-3.0-only`). You may use, study, modify, and redistribute it under the terms of that license. Distributed modified versions must remain under GPLv3 and make their corresponding source code available.

Copyright (C) 2026 FURSOY.

Licenses and copyright notices for bundled dependencies are available in the [npm](THIRD_PARTY_LICENSES_NPM.html) and [Rust](THIRD_PARTY_LICENSES_RUST.html) third-party license documents.

## Google permissions

FURSOY Mail requests:

- `gmail.modify` to read, send, and organize mail
- basic Google profile and email identity access to identify connected accounts

The app does not request the separate `gmail.send` scope because sending is already included in `gmail.modify`.

<details>
<summary><strong>Development and packaging</strong></summary>

### Requirements

- Node.js 22 or newer
- Rust stable toolchain
- Microsoft C++ Build Tools
- Tauri 2 system prerequisites for Windows

### Setup

Install frontend dependencies:

```powershell
npm ci
```

Create `src-tauri/.env` for local Google OAuth builds:

```env
GOOGLE_CLIENT_ID=your-google-client-id
GOOGLE_CLIENT_SECRET=your-google-client-secret
```

Never commit OAuth credentials or `.env` files.

Start the development application:

```powershell
npm run tauri dev
```

### Verification

Run frontend tests:

```powershell
npm test
```

Run Rust tests:

```powershell
npm run test:rust
```

Run the frontend production build:

```powershell
npm run build
```

### Windows packaging

Create an NSIS installer:

```powershell
npm run dist:windows
```

Create the installer and local updater manifest:

```powershell
npm run release:windows
```

Generated NSIS files are placed under:

```text
src-tauri/target/release/bundle/nsis/
```

Do not distribute the raw executable from `src-tauri/target/release/`. Use the NSIS `-setup.exe` artifact so installation and WebView2 prerequisite handling follow the configured release path.

Tagged releases are built and published by `.github/workflows/release.yml`. The workflow reads Google OAuth and Tauri updater signing values from GitHub repository secrets.

</details>
