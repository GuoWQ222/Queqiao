# Queqiao: design (draft 4, 2026-11-20)

Our own remote-control software for Windows, written in Rust. Users log in; every device on an account can see the
others and control them. Original code; UU Remote was studied only for its architecture
(`research\uu\FINDINGS.md`).

## Goals

- Log in with an account; see your devices with online status; connect to any of them with one click.
- Smooth, low-latency desktop streaming (target: < 50 ms glass-to-glass on LAN, 60 fps at 1080p-4K).
- Works across NAT: direct P2P when possible, relay otherwise; end-to-end encrypted either way.
- Works at the Windows login screen and on UAC prompts; Ctrl+Alt+Del; remote restart.
- Later: clipboard, file transfer, multi-monitor, audio, gamepads, virtual display.
- Runs on any Windows PC it is installed on (see the next section). It will be published.

## Runs on any PC

Queqiao is for general release, so nothing in it may depend on the machine it was developed on.

1. **Detect, never assume.** GPU vendor and model, number of GPUs, screens (count, size, refresh rate, rotation,
   DPI, HDR), CPU architecture and Windows edition are all found at runtime. Hardware probing runs in a child
   process (a driver crash must not take down the agent) and the result is cached per machine, keyed by GPU and
   driver version.
2. **Every stage has a fallback chain** that ends in a path needing no GPU at all:

   | Stage | First choice | Fallbacks, in order |
   |---|---|---|
   | Capture | DXGI Desktop Duplication on the GPU that drives the screen | Windows.Graphics.Capture, then GDI |
   | Convert | D3D11 video processor (BGRA -> NV12) | CPU (bilinear scaling, BT.709, one thread per band of rows) |
   | Encode | Hardware H.264 through Media Foundation: whichever vendor's encoder Windows offers (NVIDIA, AMD, Intel, Qualcomm) | OpenH264 on the CPU, compiled in (as UU Remote does) |
   | Decode | Media Foundation hardware decoding (D3D11VA) | OpenH264's decoder on the CPU |
   | Present | D3D11 flip-model swap chain | D3D11 on WARP (Microsoft's software adapter), then GDI |
   | Network | direct UDP (ICE) | TURN over UDP, then TURN over TCP/TLS on port 443 |

3. **Sizes and rates come from the screen and the encoder.** Encode at the screen's size when the encoder accepts it,
   otherwise scaled to fit common encoder limits (4096, 4K, 1440p, 1080p, 720p boxes; aspect kept; even sizes for
   NV12). Frame rate up to the screen's refresh rate (default cap 60). Start bitrate from size x rate (about 0.1 bit
   per pixel per frame, 2-50 Mbit/s), then the network's bandwidth estimate takes over.
4. **Codecs are negotiated.** Both ends state what they can encode and decode (in the SDP). H.264 is the common
   ground; H.265 and AV1 only when both ends have hardware for them.
5. **Platforms:** Windows 10 (1809 and later) and Windows 11, on x64 and ARM64. The programs link the C runtime
   statically (no Visual C++ Redistributable needed). Windows "N" editions ship without Media Foundation unless
   the Media Feature Pack is installed; OpenH264 does not need it, but the programs still import Media Foundation
   at start-up today, so it must become a delayed (runtime) load before release.
   The agent runs at normal priority and opts out of Windows power throttling (EcoQoS): a windowless process is
   otherwise put on slow efficiency cores. Measured on a 12th-gen Intel laptop: OpenH264 2.7x slower throttled.
   When nothing changes on screen, or the display is off (lid closed, monitor asleep), desktop duplication delivers
   no frames at all: the agent must still produce a first picture (force a repaint, wake the display, or capture
   by another path) and, for machines without a monitor, a virtual display (UU Remote installs an IddCx one).
6. **Vendor SDKs are optional extras.** Direct NVENC, AMF or oneVPL paths may be added later behind the same
   interface for lower latency, never as a requirement and never as the only path.
7. **Every path is tested.** Each fallback can be forced with a setting (for example
   `QUEQIAO_FORCE=encode:software`), so all paths run on any test machine; the test PC also runs on WARP to stand
   in for a machine without a GPU. A release is checked on several real machines (NVIDIA, AMD, Intel, a virtual
   machine, an ARM64 laptop).
8. **Licences (to confirm before release).** OpenH264's code is BSD-licensed (x264 was ruled out: GPL). H.264 itself
   is patent-licensed: the hardware encoders and decoders that come with Windows or the GPU driver carry their own
   licence, but an OpenH264 compiled into Queqiao makes the H.264 patent licence the publisher's (the patent pool
   has a free tier for small volumes). The alternative is Cisco's prebuilt OpenH264, which Cisco's licence covers
   when each PC downloads it from Cisco. **Decided by the user (2026-11-20): OpenH264 is built in, as UU
   Remote does; the publisher takes the H.264 patent licence.**
10. **Encoders: Media Foundation and direct vendor SDKs, compared** (user decision 2026-11-20): direct NVENC,
   AMF and oneVPL paths are built beside Media Foundation and measured against it on the same machines; AMD
   cannot be tested on the current machines.
9. **Build requirements:** Rust, Visual Studio Build Tools (C++), and NASM on the PATH: without NASM, OpenH264
   builds silently without its assembly and runs about 5x slower (measured 35 ms instead of 6.6 ms per 1080p
   frame); `scripts\check.ps1` warns when NASM is missing.

## Components

| Component | Runs on | Job |
|---|---|---|
| `queqiao-server` | server (the PC for testing) | Accounts, devices, presence, session tokens, WebSocket signaling (offer/answer/candidates), STUN + TURN relay. HTTPS API + WebSocket + UDP. SQLite. |
| `queqiao-service` | each controlled PC, Windows service (LocalSystem) | Starts and guards `queqiao-agent` in the active console session with a SYSTEM token (so it can reach the login/UAC desktop); restarts it on session change; Ctrl+Alt+Del (SendSAS); remote restart. |
| `queqiao-agent` | controlled PC, user session | Keeps the presence connection; on a session: capture -> encode -> send, receive input -> inject, clipboard, files. Tray icon + banner whenever someone is connected. |
| `queqiao-viewer` | controlling PC | Login, device list, viewer window: receive -> decode -> present; local input capture (Raw Input, cursor lock) -> send. |
| `queqiao-core` crates | shared | Protocol, crypto/identity, transport, capture, codec, input. |

The controlled and controlling roles live in one installed app: every device can be both.

## Media pipeline (controlled side)

1. Capture: DXGI Desktop Duplication on a D3D11 device created on the GPU that drives the screen (frames stay on
   the GPU; dirty rects + cursor shape from the API). The first frame after starting can be an empty mouse-only
   update; use frames with a present time. Fallbacks as above. Follow the input desktop (login screen / UAC) by
   re-creating the duplication after desktop switches.
2. Convert: BGRA -> NV12 on the GPU with the D3D11 video processor (BT.709, limited range; scales when the encode
   size is smaller than the screen).
3. Encode: H.264 first (every GPU and Windows can decode it), H.265/AV1 later. Hardware path through Media
   Foundation's asynchronous hardware encoders: one code path for every vendor; the first encoder that accepts our
   D3D11 device wins (this also picks the right GPU on multi-GPU laptops). Low-latency settings: low-latency mode,
   constant bitrate, no B-frames, long GOP with on-demand IDR. Settings an encoder refuses are reported, not fatal.
4. Packetise: RTP (H.264 packetisation mode 1) through `str0m`; NACK/RTX for lost packets.
5. Rate control: the bandwidth estimate from `str0m` drives encoder bitrate (reconfigure without a new keyframe)
   and frame rate; the viewer asks for an IDR (PLI) when a frame can't be recovered.

Encoder probing runs in a separate child process (a driver crash must not kill the agent); results are cached per GPU.

The first picture of a session comes from a GDI screenshot (desktop duplication only delivers changes). Frames
are encoded at most once per frame time; on the M1 loop that wait is up to one frame (median 1 ms, p95 14 ms at
60 fps), so encoding right when a change arrives, when the rate allows, is the next latency gain. CPU paths
(OpenH264, CPU conversion, no GPU) must adapt size and rate to the measured encode time: on a 12th-gen Intel
laptop they do not hold 1080p30 on busy content.

## Viewer side

Receive -> reassemble/FEC -> decode (Windows' H.264 decoder with D3D11 video on any GPU, OpenH264 otherwise) ->
present through a D3D11 swap chain (flip model, blit model before Windows 10): the GPU video processor converts
and scales NV12 straight into the window's buffer, the CPU does it on devices without one. Cursor drawn locally
from the sent shape for lower perceived latency.
Input: Raw Input for mouse and keyboard (scancodes), optional cursor lock for games; sent on a reliable stream.

## Transport (revised after the UU decompile)

UU builds on WebRTC's standard pieces (`research\uu\STREAMER_FINDINGS.md`). We do the same, in Rust:

- **WebRTC via `str0m`** (pure-Rust, sans-IO WebRTC): ICE with STUN for hole punching, DTLS-SRTP encryption,
  RTP/RTCP with transport-wide congestion control and a GCC-style bandwidth estimator, NACK/RTX, SCTP data
  channels. Sans-IO means we own the sockets and threads, which suits a low-latency engine.
- **Relay: TURN.** `queqiao-server` includes a TURN server (`turn` crate); agents get relay candidates from it, so ICE
  falls back to the relay automatically when direct UDP fails. Later: TURN over TCP/TLS for UDP-blocked networks.
- **Data channels:** `control` (reliable, ordered: session control, clipboard), `input` (unordered, low latency),
  `files` (reliable, flow-controlled).
- **Identity and encryption:** each device has its own DTLS certificate (key protected with DPAPI). The account
  server publishes each device's certificate fingerprint to the other devices on the account; the signed SDP carries
  it and peers check it. The TURN relay only forwards encrypted packets.
- **Signaling:** our own WebSocket protocol on `queqiao-server` (JSON or protobuf messages: offer, answer, candidate,
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
| M1 | Local loop on one PC: capture -> encode -> decode -> window, with the software fallbacks | latency and fps measured; every fallback forced and tested. **Done 2026-11-20**: RTX 5080 PC 4/4 paths pass (hardware: 12 ms median screen change -> shown); Intel Iris Xe laptop 3/4 (hardware 20 ms; no-GPU path 55 ms, needs adaptive size) |
| M2 | LAN: agent + viewer by IP, video + mouse/keyboard | control the PC from a second process / PC. **Done 2026-11-20**: one PC (611/611 frames, input 7/7) and PC viewer -> Intel laptop agent over Tailscale (direct, 5 ms ping): 17 ms median / 29 ms p95 screen change -> shown, input checked 7/7, 2 frames lost in 20 s and recovered by keyframe requests |
| M3 | `queqiao-server`: accounts, devices, presence; device list in the viewer | log in, see devices, connect by click |
| M4 | NAT traversal + relay | connect across networks, forced-relay test |
| M5 | `queqiao-service`: login screen, UAC, Ctrl+Alt+Del, restart | control at the lock screen |
| M6 | Clipboard, file transfer, multi-monitor, audio | each feature end to end |
| M7 | Adaptive bitrate, optional vendor fast paths, installer, auto-update | quality under packet loss / low bandwidth |
| M8 | Release checks on several real machines (NVIDIA, AMD, Intel, virtual machine, ARM64) | the full test list passes on each |
