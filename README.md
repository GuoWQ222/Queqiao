# Queqiao 鹊桥

Remote control for Windows, in Rust: log in, see your devices, and control any of them from another.
Named after the magpie bridge (鹊桥) of the Qixi legend, which joins two people across the Milky Way. Design and milestones: `docs\DESIGN.md`.

Target: any Windows 10 (1809+) or Windows 11 PC, x64 or ARM64, with any GPU or none. Every stage detects what the
machine has and falls back to a path that needs no GPU (`docs\DESIGN.md`, "Runs on any PC").

| Folder | Contents |
|---|---|
| `crates\queqiao-core` | Shared library: protocol, identity, transport, codec, input |
| `crates\queqiao-capture` | Screen capture (DXGI desktop duplication); `examples\capture_probe.rs` checks it |
| `crates\queqiao-codec` | BGRA -> NV12 on the GPU and the hardware H.264 encoder (any vendor, via Media Foundation); `examples\encode_probe.rs` checks it |
| `crates\queqiao-server` | Accounts, devices, presence, signaling, STUN/TURN relay |
| `crates\queqiao-service` | Windows service on each controlled PC |
| `crates\queqiao-agent` | Controlled side (user session) |
| `crates\queqiao-viewer` | Controlling side (viewer window) |
| `scripts` | Build and check scripts |
| `docs` | Design |
| `research` | Design-level notes on other products; no third-party code |

Build: `cargo build --workspace`. Full check (fmt, clippy, build, test, run each program):
`powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1`

Programs that capture the screen or show windows must run in the logged-on user's session, not over SSH:
`scripts\run_in_session.ps1 -Exe <full path> -Arguments '<arguments>'`.
