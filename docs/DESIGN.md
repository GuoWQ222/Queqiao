# RemoteControl: design (draft 2, 2026-11-20)

Our own remote-control software for Windows, written in Rust. Users log in; every device on an account can see the
others and control them. Original code; UU Remote was studied only for its architecture
(`research\uu\FINDINGS.md`).

## Goals

- Log in with an account; see your devices with online status; connect to any of them with one click.
- Smooth, low-latency desktop streaming (target: < 50 ms glass-to-glass on LAN, 60 fps at 1080p-4K).
- Works across NAT: direct P2P when possible, relay otherwise; end-to-end encrypted either way.
- Works at the Windows login screen and on UAC prompts; Ctrl+Alt+Del; remote restart.
- Later: clipboard, file transfer, multi-monitor, audio, gamepads, virtual display.

## Components

| Component | Runs on | Job |
|---|---|---|
| `rc-server` | server (the PC for testing) | Accounts, devices, presence, session tokens, WebSocket signaling (offer/answer/candidates), STUN + TURN relay. HTTPS API + WebSocket + UDP. SQLite. |
| `rc-service` | each controlled PC, Windows service (LocalSystem) | Starts and guards `rc-agent` in the active console session with a SYSTEM token (so it can reach the login/UAC desktop); restarts it on session change; Ctrl+Alt+Del (SendSAS); remote restart. |
| `rc-agent` | controlled PC, user session | Keeps the presence connection; on a session: capture -> encode -> send, receive input -> inject, clipboard, files. Tray icon + banner whenever someone is connected. |
| `rc-viewer` | controlling PC | Login, device list, viewer window: receive -> decode -> present; local input capture (Raw Input, cursor lock) -> send. |
| `rc-core` crates | shared | Protocol, crypto/identity, transport, capture, codec, input. |

The controlled and controlling roles live in one installed app: every device can be both.

## Media pipeline (controlled side)

1. Capture: DXGI Desktop Duplication on a D3D11 device (frames stay on the GPU; dirty rects + cursor shape from the
   API). GDI fallback. Follow the input desktop (login screen / UAC) by re-creating duplication after desktop switches.
2. Convert: BGRA -> NV12 on the GPU (D3D11 video processor or a compute shader).
3. Encode: H.264 first (all GPUs decode it), H.265/AV1 later. Hardware path through Media Foundation hardware MFTs
   (NVIDIA/AMD/Intel, one code path); direct NVENC later for the lowest latency. Low-latency settings: no B-frames,
   infinite GOP with on-demand IDR, intra refresh, CBR/VBV of about one frame.
4. Packetise: RTP (H.264 packetisation mode 1) through `str0m`; NACK/RTX for lost packets.
5. Rate control: the bandwidth estimate from `str0m` drives encoder bitrate (reconfigure without a new keyframe)
   and frame rate; the viewer asks for an IDR (PLI) when a frame can't be recovered.

Encoder probing runs in a separate child process (a driver crash must not kill the agent); results are cached per GPU.

## Viewer side

Receive -> reassemble/FEC -> decode with D3D11VA (Media Foundation decoder) -> present with a D3D11 flip-model
swapchain (no copies to the CPU), cursor drawn locally from the sent shape for lower perceived latency.
Input: Raw Input for mouse and keyboard (scancodes), optional cursor lock for games; sent on a reliable stream.

## Transport (revised after the UU decompile)

UU builds on WebRTC's standard pieces (`research\uu\STREAMER_FINDINGS.md`). We do the same, in Rust:

- **WebRTC via `str0m`** (pure-Rust, sans-IO WebRTC): ICE with STUN for hole punching, DTLS-SRTP encryption,
  RTP/RTCP with transport-wide congestion control and a GCC-style bandwidth estimator, NACK/RTX, SCTP data
  channels. Sans-IO means we own the sockets and threads, which suits a low-latency engine.
- **Relay: TURN.** `rc-server` includes a TURN server (`turn` crate); agents get relay candidates from it, so ICE
  falls back to the relay automatically when direct UDP fails. Later: TURN over TCP/TLS for UDP-blocked networks.
- **Data channels:** `control` (reliable, ordered: session control, clipboard), `input` (unordered, low latency),
  `files` (reliable, flow-controlled).
- **Identity and encryption:** each device has its own DTLS certificate (key protected with DPAPI). The account
  server publishes each device's certificate fingerprint to the other devices on the account; the signed SDP carries
  it and peers check it. The TURN relay only forwards encrypted packets.
- **Signaling:** our own WebSocket protocol on `rc-server` (JSON or protobuf messages: offer, answer, candidate,
  restart-ice, leave), authorised with the account token. Compressed SDP is optional.
- **Desktop tuning (from what UU ships):** high start bitrate (about 8 Mbps), no encoder frame dropping, render
  immediately (no jitter-buffer delay), no probing bursts, don't treat random loss as congestion, keyframes at most
  every ~600 ms on loss, reset the estimate when the path changes, keep gathering ICE candidates after connecting.
- **Later, if needed:** FEC for lossy links (Reed-Solomon over RTP), a QUIC data channel for file transfer.

Alternatives considered: our own QUIC transport (`quinn`): full control, but hole punching, relay and real-time
rate control would all be ours to build and debug; Google libwebrtc via LiveKit's Rust bindings: the most complete
congestion control and FEC, but a large prebuilt C++ dependency and hard to feed with our own GPU textures.

## Accounts and security

- Passwords hashed with Argon2id; short-lived access tokens + refresh tokens; per-device revocation.
- A device joins an account after login; new devices are shown to the account's other devices.
- Rule: only devices on the same account can control each other (checked by the server when it issues a session
  token, and again by the controlled agent against its pinned list).
- The controlled PC always shows that a session is active (tray icon + banner) and can end it.

## Milestones

| # | Result | Check |
|---|---|---|
| M0 | Toolchain, Cargo workspace, empty crates build | `cargo build` passes |
| M1 | Local loop on one PC: capture -> encode -> decode -> window | latency and fps measured on the PC |
| M2 | LAN: agent + viewer by IP, video + mouse/keyboard | control the PC from a second process / PC |
| M3 | `rc-server`: accounts, devices, presence; device list in the viewer | log in, see devices, connect by click |
| M4 | NAT traversal + relay | connect across networks, forced-relay test |
| M5 | `rc-service`: login screen, UAC, Ctrl+Alt+Del, restart | control at the lock screen |
| M6 | Clipboard, file transfer, multi-monitor, audio | each feature end to end |
| M7 | Adaptive bitrate, direct NVENC, installer, auto-update | quality under packet loss / low bandwidth |
