# Queqiao 鹊桥

Remote control for Windows, in Rust: log in, see your devices, and control any of them from another.
Named after the magpie bridge (鹊桥) of the Qixi legend, which joins two people across the Milky Way. Design and milestones: `docs\DESIGN.md`.

Target: any Windows 10 (1809+) or Windows 11 PC, x64 or ARM64, with any GPU or none. Every stage detects what the
machine has and falls back to a path that needs no GPU (`docs\DESIGN.md`, "Runs on any PC").

| Folder | Contents |
|---|---|
| `crates\queqiao-core` | Shared library: protocol, identity, transport, codec, input |
| `crates\queqiao-capture` | Screen capture (DXGI desktop duplication); `examples\capture_probe.rs` checks it |
| `crates\queqiao-codec` | BGRA -> NV12 (GPU video processor, CPU fallback) and the H.264 encoder: hardware of any vendor via Media Foundation, OpenH264 on the CPU as the fallback; `examples\encode_probe.rs` checks it |
| `crates\queqiao-display` | Viewer window and presenter (D3D11 swap chain; GPU video processor, CPU fallback); `examples\loopback.rs` runs the whole local loop |
| `crates\queqiao-transport` | WebRTC through str0m (Windows crypto): ICE, DTLS-SRTP, H.264 over RTP, a data channel; direct signaling over TCP (M2) |
| `crates\queqiao-input` | Injects the viewer's mouse and keyboard on the controlled PC (SendInput, scancodes) |
| `crates\queqiao-server` | Accounts, devices, presence, signaling, STUN/TURN relay |
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

Remote control by IP address (M2; queqiao-server will find devices by account in M3):
`queqiao-viewer --listen <ip:port>` on the controlling PC and `queqiao-agent --connect <ip:port>` on the controlled
PC (or the other way round: either side may listen). The listening side needs an inbound firewall rule for its
program. `scripts\run_m2_local.ps1` runs both on one PC with an input test.

Programs that capture the screen or show windows must run in the logged-on user's session, not over SSH:
`scripts\run_in_session.ps1 -Exe <full path> -Arguments '<arguments>'`. Checks on a machine (release builds
in `target\release\examples`, or copy the .exe files and scripts into one folder):
`scripts\run_probes.ps1` (capture and encoders), `scripts\run_codec_probes.ps1` (encode/decode round trips,
also over SSH), `scripts\run_loopbacks.ps1` (the whole loop in a window, with the delay of each stage). Fallback paths can be forced with `QUEQIAO_FORCE`, for
example `encode:software`, `convert:cpu`, `adapter:warp` (probes take `force=...`).
