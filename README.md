# Queqiao 鹊桥

Remote control for Windows, in Rust: log in, see your devices, and control any of them from another.
Named after the magpie bridge (鹊桥) of the Qixi legend, which joins two people across the Milky Way. Design and milestones: `docs\DESIGN.md`.

Target: any Windows 10 (1809+) or Windows 11 PC, x64 or ARM64, with any GPU or none. Every stage detects what the
machine has and falls back to a path that needs no GPU (`docs\DESIGN.md`, "Runs on any PC").

| Folder | Contents |
|---|---|
| `crates\queqiao-core` | Shared: the viewer-agent protocol, the device-server messages, test switches, process set-up |
| `crates\queqiao-capture` | Screen capture (DXGI desktop duplication); `examples\capture_probe.rs` checks it |
| `crates\queqiao-codec` | BGRA -> NV12 (GPU video processor, CPU fallback) and the H.264 encoder: hardware of any vendor via Media Foundation, OpenH264 on the CPU as the fallback; `examples\encode_probe.rs` checks it |
| `crates\queqiao-display` | Viewer window and presenter (D3D11 swap chain; GPU video processor, CPU fallback), the login form and device list; `examples\loopback.rs` runs the whole local loop |
| `crates\queqiao-transport` | WebRTC through str0m (Windows crypto): ICE, DTLS-SRTP, H.264 over RTP, a data channel; the signaling interface, and direct signaling over TCP (M2) |
| `crates\queqiao-account` | Client side of queqiao-server: TLS WebSocket with a pinned certificate, login, device list, session signaling; the saved login (DPAPI); `examples\account_probe.rs` checks a server |
| `crates\queqiao-input` | Injects the viewer's mouse and keyboard on the controlled PC (SendInput, scancodes) |
| `crates\queqiao-server` | Accounts (Argon2id), devices, presence and signaling over TLS WebSockets, SQLite; STUN/TURN relay in M4 |
| `crates\queqiao-service` | Windows service on each controlled PC |
| `crates\queqiao-agent` | Controlled side (user session) |
| `crates\queqiao-viewer` | Controlling side (viewer window) |
| `scripts` | Build and check scripts |
| `docs` | Design |
| `research` | Design-level notes on other products; no third-party code |

Build requirements: Rust 1.85 or later, Visual Studio Build Tools with the C++ workload (OpenH264 is C++),
and [NASM](https://www.nasm.us) on the PATH (OpenH264's assembly; without it the software encoder is about
5x slower). The programs link the C runtime statically, so they need no Visual C++ Redistributable.

Build: `cargo build --workspace`. Full check (fmt, clippy, build, test, run each program):
`powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1`

Remote control with an account (M3):
1. On the server PC: `queqiao-server init --name <its IP>` prints the certificate fingerprint,
   `queqiao-server add-user <name> <password>` adds a user, `queqiao-server serve --name <its IP>` runs it: accounts
   on TCP 47100, STUN/TURN on UDP 3478, relayed addresses on UDP 47200-47399 (`--turn-port`, `--relay-ports`,
   `--public-ip`; `--relay-private` also relays to private addresses, for LAN-only servers). Its data, certificate
   and key are in `queqiao-server-data`. The firewall must let those ports in.
2. On each PC to be controlled, once: `queqiao-agent --server <host:47100> --pin <fingerprint> --user <name>
   --password <password>`; later starts need no options (the login is saved, DPAPI-encrypted, in
   `%APPDATA%\Queqiao`).
3. On the controlling PC: start `queqiao-viewer`, log in once, double-click a device.
`scripts\run_m3_local.ps1` runs all of it on one PC, with the account probe; `scripts\run_m4_local.ps1` adds a session
forced through the relay (`--force network:relay` or `QUEQIAO_FORCE=network:relay`).

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
