# Developing Queqiao

How the repository is laid out, how to build and check it, how the packages are made and how it is tested. For what
Queqiao does and how to use it, see the [README](../README.md); for the design, [DESIGN.md](DESIGN.md) (Windows),
[LINUX.md](LINUX.md) and [MOBILE.md](MOBILE.md); for the feature list against UU Remote's, [PARITY.md](PARITY.md).

## Repository layout

| Folder | Contents |
|---|---|
| `crates/queqiao-core` | Shared: the viewer-agent protocol, the device-server messages, the texts in English and Chinese, test switches, process set-up |
| `crates/queqiao-capture` | Screen capture: DXGI desktop duplication on Windows; MIT-SHM on X11, the ScreenCast portal or wlroots on Wayland |
| `crates/queqiao-codec` | Colour conversion and the video encoders and decoders: NVIDIA NVENC/NVDEC, Intel oneVPL, AMD AMF, Media Foundation, OpenH264 as the fallback on the CPU; H.264, H.265 (4:2:0 and 4:4:4, 8 and 10 bits), AV1 |
| `crates/queqiao-display` | Everything on screen: the session window and presenter, the main window, the login window, the control center, the file transfer window, dialogs, the tray icon, banner and consent prompt, the privacy screen (Win32 on Windows, egui and X11/Wayland on Linux) |
| `crates/queqiao-transport` | WebRTC through str0m: ICE (with our own TURN client), DTLS-SRTP, video and Opus over RTP, a control channel and streams with flow control; the signalling interface and the server's name lookup |
| `crates/queqiao-audio` | Sound: WASAPI (Windows) or PulseAudio (Linux) capture and playback, Opus, the speakers' mute |
| `crates/queqiao-files` | File and folder transfer (offers, pieces, SHA-256, safe names), browsing the other side, the transfer list, clipboard images |
| `crates/queqiao-terminal` | Remote command lines: ConPTY or a pseudo terminal on the controlled side, a terminal window per command line on the controlling side |
| `crates/queqiao-input` | Injects the viewer's mouse and keyboard on the controlled side (SendInput; XTest, wlroots virtual devices or the RemoteDesktop portal); the clipboard |
| `crates/queqiao-portal` | The Wayland desktop portal session shared by capture and input (GNOME, KDE) |
| `crates/queqiao-account` | Client side of queqiao-server: TLS WebSocket with a pinned certificate, login (and sign-up), device list, session signalling; the saved login (DPAPI on Windows, the Secret Service on Linux) |
| `crates/queqiao-server` | Accounts (Argon2id), devices, presence and signalling over TLS WebSockets, SQLite; the STUN/TURN relay; relay servers that join it (choosing the relay per session) and a standby's live copy of the accounts |
| `crates/queqiao-service` | The optional service on a controlled computer: keeps the agent running for whoever is signed in, and one for the sign-in, lock, Ctrl+Alt+Del and UAC screens; applies updates (Windows); systemd with KMS, uinput and PAM (Linux) |
| `crates/queqiao-local` | The private channels between the service, its desktop helper and the agent |
| `crates/queqiao-setup` | The Windows installer (one signed file carrying the signed programs), uninstall, updates, and the release tools (`pack`, `release-key`, `release`) |
| `crates/queqiao-update` | The update feed: a manifest signed with the release key (Ed25519), downloads |
| `crates/queqiao-mobile` | The core of the Android and iOS apps (account, sessions, input), called through a small C interface |
| `crates/queqiao-agent` | The controlled side (the person's session) |
| `crates/queqiao-viewer` | The controlling side (the session window) |
| `mobile/android`, `mobile/ios` | The phone apps: Jetpack Compose and SwiftUI on the shared core |
| `packaging/linux` | What goes into the .deb and .rpm (files.txt), the systemd units, PAM files, desktop entries |
| `scripts` | Build, package and test scripts |
| `docs` | Design notes; `docs/images` the README's pictures |
| `research` | Design-level notes on other products and the tools that made them; no third-party code or text |

## Building

**Windows:** Rust 1.85 or later, Visual Studio Build Tools with the C++ workload and its CMake (libopus is built from
source; `scripts\check.ps1` finds Visual Studio's CMake by itself), and [NASM](https://www.nasm.us) on the PATH
(OpenH264's assembly; without it the software encoder is about 5x slower). The programs link the C runtime
statically, so they need no Visual C++ Redistributable.

**Linux:** Rust, a C toolchain with clang, CMake, NASM and pkg-config, and the X11, Wayland, PipeWire, PulseAudio,
D-Bus and OpenSSL development packages (on Ubuntu: `tools/linux/cc4_setup.sh` lists them).

```sh
cargo build --workspace
```

Full check (formatting, clippy with warnings as errors, build, tests, every program's `--version`):

```sh
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1   # Windows
scripts/check.sh                                                        # Linux
```

The phone apps are built by GitHub Actions (`.github/workflows/android.yml`, `ios.yml`): `scripts/build_mobile_core.sh`
and `scripts/build_android.sh` for Android, `scripts/build_ios_core.sh` and Xcode for iOS. Their test results are
pushed to the branches `ci-results-android` and `ci-results-ios`.

## Packages

- **Windows:** `scripts\package.ps1 -Ship [-Server <host:port> -Pin <sha256>]` makes a shipping build (without the
  tests' options), signs the programs (a test certificate for now), packs them into
  `target\package\queqiao-setup-<version>.exe` and signs that. `-Server`/`-Pin` build in the account server the login
  window offers; they are given at build time, not kept in this repository. The installer installs for the person
  running it into `%LOCALAPPDATA%\Programs\Queqiao` (Start menu, Settings > Apps, start with Windows), and the service
  if chosen (one UAC prompt): `--quiet [--autostart] [--service]`, `--uninstall [--quiet]`.
- **Linux:** `scripts/package_linux.sh [--server <host:port> --pin <sha256>]` makes a .deb and an .rpm.
- **Updates (Windows):** `queqiao-setup release-key --output <file>` makes the release key once (encrypted, never in
  this repository; its public half goes into `crates/queqiao-update`); `queqiao-setup release --key <file> --installer
  <file> --version <x.y.z> --output <folder>` writes the signed feed. On Linux, updates come with the system's packages.

## The server

`queqiao-server init --name <its address>` makes the data folder, certificate and key and prints the certificate's
fingerprint; `queqiao-server add-user <name>` adds a user (the password on standard input) and `remove-user`,
`remove-device` take things away; `queqiao-server serve --name <its address>` runs it: accounts on TCP 47100, STUN/TURN
on UDP 3478, relayed addresses on UDP 47200-47399 (`--listen`, `--turn-port`, `--relay-ports`, `--public-ip`;
`--relay-private` also relays to private addresses, for servers on a LAN only; `--allow-signup` lets people make
accounts from the login window, at most 3 an hour from one address; `--capacity-mbps` is what the server may send in
all, 100 by default).

### Several relays and a standby

One server holds the accounts (the account server); the relayed traffic, the only heavy traffic, is spread over relay
servers that join it:

```sh
# On the account server: let relay "shanghai" join (prints its key once)
queqiao-server add-relay shanghai --data <folder> | ssh shanghai 'queqiao-server relay-key --data <its folder>'

# On the relay server: STUN/TURN only, linked to the account server (its certificate pinned)
queqiao-server relay --central <account server>:47100 --pin <its fingerprint> --public-ip <this server's address> \
    --capacity-mbps 100 --data <its folder> [--standby]
```

- Each device measures its round trip to every relay after logging in. Each session gets the relay nearest to both of
  its devices that has room, with the next two as backups: a relay that does not answer while the session is set up
  is left for the next one. Relays report their load every 2 s, and one nearly full shares its capacity fairly
  between its sessions. `remove-relay <name>` takes one out at once; `relays` lists them.
- A relay checks the relay passwords itself (the account server makes them with that relay's key) and is told of every
  password that was revoked (a logout, a removed device, a new account password).
- `--standby` also keeps a live copy of the accounts in its folder: a full copy first, then each change as it is made
  (a change log written by triggers, so the command-line tools' changes are in it too). The programs are told of the
  standby and keep it with their login. To make it the account server: stop its relay (and the old account server, if
  it still runs) and run `queqiao-server serve` on its folder. Saved logins work there as before; relays given it as a
  second `--central`/`--pin` link to it.

## Tests

Programs that capture the screen or show windows must run in the signed-in person's session, not over SSH:
`scripts\run_in_session.ps1 -Exe <full path> -Arguments '<arguments>'`.

| Script | What it runs |
|---|---|
| `scripts\run_m2_local.ps1`, `scripts/run_m2_linux.sh` | A viewer and an agent by IP address, with the input test (`queqiao-viewer --listen <ip:port>`, `queqiao-agent --connect <ip:port>`) |
| `scripts\run_m3_local.ps1`, `scripts/run_m3_linux.sh` | The account mode with a server of its own: sessions, consent, the login window and device list, the main window, codecs |
| `scripts\run_m4_local.ps1` | Adds a session forced through the relay (`--force network:relay`), helping (a code, a link, a fixed code), two sessions at once, a new account |
| `scripts\run_m5_service.ps1`, `scripts\run_m5_logon.ps1` | The service and the sign-in screen (on a test PC) |
| `scripts\run_m7_setup.ps1`, `run_m7_update.ps1`, `run_m7_service_update.ps1`, `run_m7_agent_login.ps1` | The installer, updates, the login from the tray |
| `scripts/run_login_linux.sh` | The one login form: a new name makes the account, a wrong password is refused, the right one logs in |
| `scripts/run_takeover_linux.sh`, `scripts\run_takeover_windows.ps1` | One device controls a computer at a time: busy, Take Control, Disconnect |
| `scripts/run_relays_linux.sh` | Two relay servers joined to an account server: a wrong key and an unknown relay refused, both ends of a session on the same relay, the next one when it does not answer, a relay taken out |
| `scripts/run_standby_linux.sh` | A standby's copy of the accounts: a full copy, then a new user as a change; the standby made the account server, and logins falling back to it |
| `scripts/run_android_e2e.sh`, `scripts/run_ios_simulator_smoke.sh` | The phone apps against a server and an agent (in CI) |

Checks of one machine's capture and encoders (release builds in `target\release\examples`): `scripts\run_probes.ps1`,
`scripts\run_codec_probes.ps1` (encode/decode round trips, also over SSH), `scripts\run_loopbacks.ps1` (the whole loop
in a window, with each stage's delay). Fallback paths can be forced with `QUEQIAO_FORCE`, for example
`encode:software`, `convert:cpu`, `adapter:warp` (probes take `force=...`).
