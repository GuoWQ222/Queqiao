# Queqiao 鹊桥

Remote control for Windows, in Rust: log in, see your devices, and control any of them from another.
Named after the magpie bridge (鹊桥) of the Qixi legend, which joins two people across the Milky Way. Design and milestones: `docs\DESIGN.md`.

Target: any Windows 10 (1809+) or Windows 11 PC, x64 or ARM64, with any GPU or none. Every stage detects what the
machine has and falls back to a path that needs no GPU (`docs\DESIGN.md`, "Runs on any PC").

What it does (the full list, against UU Remote's functions: `docs\PARITY.md`):
- **Connecting:** an account with your devices (made from the login form where the server allows it), direct or
  through the relay (STUN/TURN), signed signals between device keys, consent on the controlled PC, a tray icon and a
  session banner with End; several sessions at once; helping someone of another account with their help ID and a code
  (one-use or fixed; an invitation to copy, a queqiao:// link); Wake-on-LAN through another device on the same network,
  and a check of this PC's set-up for it; device names of your own and favourites.
- **Installing and updating:** one signed installer, for the person using the PC without an administrator, and the
  Queqiao service if wanted (the sign-in, lock, Ctrl+Alt+Del and UAC screens); updates signed with a release key,
  installed between sessions, and put back if the new version does not start.
- **Picture and sound:** hardware H.264 on any GPU (software fallback), adaptive bitrate, frame-rate, quality,
  resolution and bitrate settings, several screens, full screen, the controlled PC's sound (Opus), its speakers
  muted while you hear it, a privacy screen that darkens the controlled PC while you see it.
- **Input:** mouse and keyboard by scancode, Windows shortcuts (Win, Alt+Tab), game mouse (relative movement, its
  speed), the controlled PC's pointer shape on the local pointer, hotkeys of your own and a boss key.
- **Files and clipboard:** text, images and files both ways through the clipboard; drop files or folders on the
  session window; a transfer list with pause, resume and cancel.
- **Tools:** remote command lines (PowerShell in a pseudo console; detach, show again, rename), lock, restart,
  Ctrl+Alt+Del (with the service).
- **The controlled PC's person decides:** whether the account's devices may control it, asking first, clipboard,
  files, command lines, starting with Windows, locking after a session, updates, and a password for these settings;
  the privacy screen also turns its keyboard and mouse off (Ctrl+Alt+Del gives them back).
- **Languages:** Simplified Chinese and English, from Windows' display language.

| Folder | Contents |
|---|---|
| `crates\queqiao-core` | Shared: the viewer-agent protocol, the device-server messages, test switches, process set-up |
| `crates\queqiao-capture` | Screen capture (DXGI desktop duplication); `examples\capture_probe.rs` checks it |
| `crates\queqiao-codec` | BGRA -> NV12 (GPU video processor, CPU fallback) and the H.264 encoder: hardware of any vendor via Media Foundation, OpenH264 on the CPU as the fallback; `examples\encode_probe.rs` checks it |
| `crates\queqiao-display` | Viewer window and presenter (D3D11 swap chain; GPU video processor, CPU fallback), the login form, device list and dialogs, the tray icon, banner and consent prompt, the privacy screen, the shortcut hook; `examples\loopback.rs` runs the whole local loop |
| `crates\queqiao-transport` | WebRTC through str0m (Windows crypto): ICE (with our own TURN client), DTLS-SRTP, H.264 and Opus over RTP, a control channel and streams with flow control (files, command lines, clipboard images); the signaling interface, and direct signaling over TCP (M2) |
| `crates\queqiao-audio` | Sound: WASAPI loopback capture and playback, Opus (libopus), the speakers' mute |
| `crates\queqiao-files` | File and folder transfer (offers, pieces, SHA-256, safe names), the transfer list, clipboard images (compressed) |
| `crates\queqiao-terminal` | Remote command lines: ConPTY on the controlled PC, a console window per command line on the controlling PC |
| `crates\queqiao-account` | Client side of queqiao-server: TLS WebSocket with a pinned certificate, login, device list, session signaling; the saved login (DPAPI); `examples\account_probe.rs` checks a server |
| `crates\queqiao-input` | Injects the viewer's mouse and keyboard on the controlled PC (SendInput, scancodes); the clipboard (text, images, files) |
| `crates\queqiao-server` | Accounts (Argon2id), devices, presence and signaling over TLS WebSockets, SQLite; STUN/TURN relay in M4 |
| `crates\queqiao-service` | Windows service on each controlled PC (optional): keeps the agent running for whoever is signed in, and an agent for the sign-in screen; its SYSTEM desktop helper shows and works the sign-in, lock, Ctrl+Alt+Del and UAC screens; applies updates to its own copy |
| `crates\queqiao-local` | The private channels between the service, its desktop helper and the agent (anonymous pipes handed over to the child) |
| `crates\queqiao-setup` | The installer (one signed file carrying the signed programs), uninstall, updates (`--update`, `--service-update`), and the release tools (`pack`, `release-key`, `release`) |
| `crates\queqiao-update` | The update feed: a manifest signed with the release key (Ed25519), WinHTTP downloads |
| `crates\queqiao-agent` | Controlled side (user session) |
| `crates\queqiao-viewer` | Controlling side (viewer window) |
| `scripts` | Build and check scripts |
| `docs` | Design |
| `research` | Design-level notes on other products; no third-party code |

Build requirements: Rust 1.85 or later, Visual Studio Build Tools with the C++ workload (OpenH264 is C++) and its
CMake (libopus is built from source; `scripts\check.ps1` finds Visual Studio's CMake by itself), and
[NASM](https://www.nasm.us) on the PATH (OpenH264's assembly; without it the software encoder is about 5x slower).
The programs link the C runtime statically, so they need no Visual C++ Redistributable.

Build: `cargo build --workspace`. Full check (fmt, clippy, build, test, run each program):
`powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1`

The installer: `powershell -File scripts\package.ps1` after a release build signs the programs (a test certificate for
now), packs them into `target\package\queqiao-setup-<version>.exe` and signs that. It installs for the person running
it into `%LOCALAPPDATA%\Programs\Queqiao` (Start menu, Settings > Apps, "Start with Windows"), and the service if
chosen (one UAC prompt); `--quiet [--autostart] [--service]`, `--uninstall [--quiet]`. Updates: `queqiao-setup
release-key --output <file>` makes the release key once (DPAPI-encrypted, never in this repository; its public half
goes into `crates\queqiao-update`), `queqiao-setup release --key <file> --installer <file> --version <x.y.z> --output
<folder>` writes the signed feed to publish as a GitHub release's files.

Remote control with an account (M3):
1. On the server PC: `queqiao-server init --name <its IP>` prints the certificate fingerprint,
   `queqiao-server add-user <name> <password>` adds a user (or `serve --allow-signup` lets people make accounts from
   the login form), `queqiao-server serve --name <its IP>` runs it: accounts
   on TCP 47100, STUN/TURN on UDP 3478, relayed addresses on UDP 47200-47399 (`--turn-port`, `--relay-ports`,
   `--public-ip`; `--relay-private` also relays to private addresses, for LAN-only servers). Its data, certificate
   and key are in `queqiao-server-data`. The firewall must let those ports in.
2. On each PC to be controlled: install it (above), then "Log in..." in the tray (server, fingerprint, user name,
   password; or "Create an account"). Without the installer, once: `queqiao-agent --server <host:47100> --pin
   <fingerprint> --user <name> --password <password>`. Later starts need nothing (the login is saved, DPAPI-encrypted,
   in `%APPDATA%\Queqiao`); the agent stays in the tray while the server cannot be reached and tries again.
3. On the controlling PC: start `queqiao-viewer`, log in once, double-click a device. The session window's menu
   (title bar, or Alt+Space) has everything for the session: sound, picture settings, screens, full screen
   (Ctrl+Alt+Enter), game mouse (Ctrl+Alt+M), command lines, transfers, lock, restart. The controlled PC's tray icon
   has its person's settings and "Let someone help me..." (help ID and code for someone of another account; they use
   "Help someone..." in their device list).
`scripts\run_m3_local.ps1` runs all of it on one PC, with the account probe; `scripts\run_m4_local.ps1` adds a session
forced through the relay (`--force network:relay` or `QUEQIAO_FORCE=network:relay`), helping (with a code, a link and a
fixed code), two sessions at once and a new account. The service: `scripts\run_m5_service.ps1`,
`scripts\run_m5_logon.ps1` (the sign-in screen, on a test PC); the installer and updates: `scripts\run_m7_setup.ps1`,
`scripts\run_m7_update.ps1`, `scripts\run_m7_service_update.ps1`, `scripts\run_m7_agent_login.ps1`.

Remote control by IP address (M2, for tests):
`queqiao-viewer --listen <ip:port>` on the controlling PC and `queqiao-agent --connect <ip:port>` on the controlled
PC (or the other way round: either side may listen). The listening side needs an inbound firewall rule for its
program. `scripts\run_m2_local.ps1` runs both on one PC with an input test.

Programs that capture the screen or show windows must run in the logged-on user's session, not over SSH:
`scripts\run_in_session.ps1 -Exe <full path> -Arguments '<arguments>'`. Checks on a machine (release builds
in `target\release\examples`, or copy the .exe files and scripts into one folder):
`scripts\run_probes.ps1` (capture and encoders), `scripts\run_codec_probes.ps1` (encode/decode round trips,
also over SSH), `scripts\run_loopbacks.ps1` (the whole loop in a window, with the delay of each stage). Fallback paths can be forced with `QUEQIAO_FORCE`, for
example `encode:software`, `convert:cpu`, `adapter:warp` (probes take `force=...`).
