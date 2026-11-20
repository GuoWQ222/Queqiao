# RemoteControl

Our own remote-control software for Windows, in Rust. Design and milestones: `docs\DESIGN.md`.

| Folder | Contents |
|---|---|
| `crates\rc-core` | Shared library: protocol, identity, transport, capture, codec, input |
| `crates\rc-server` | Accounts, devices, presence, signaling, STUN/TURN relay |
| `crates\rc-service` | Windows service on each controlled PC |
| `crates\rc-agent` | Controlled side (user session) |
| `crates\rc-viewer` | Controlling side (viewer window) |
| `scripts` | Build and check scripts |
| `docs` | Design |
| `research` | Design-level notes on other products; no third-party code |

Build: `cargo build --workspace`. Full check (fmt, clippy, build, test, run each program):
`powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check.ps1`
