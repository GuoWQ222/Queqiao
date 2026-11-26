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
9. **Encoders: Media Foundation and direct vendor SDKs, compared** (user decision 2026-11-20): direct NVENC,
   AMF and oneVPL paths are built beside Media Foundation and measured against it on the same machines; AMD
   cannot be tested on the current machines.
   NVENC (direct, `QUEQIAO_FORCE=encode:nvenc`) is built against two API versions, the same session code for each
   (nvenc_session.rs): 12.0 where the driver has it (R522 on Windows, R520 on Linux, late 2022), which adds AV1, and
   11.1 for drivers from R470 (2021), which make H.264 and H.265 as before; never the newest header, which needs the
   newest driver (13.1 was refused by a March 2026 driver that supports 13.0). `QUEQIAO_NVENC_API=11` keeps a newer
   driver on 11.1, to test that path. Measured on the RTX 5080 PC,
   2560x1440 at 60 fps, live screen: Media Foundation 2.99 ms per frame (p95 3.12), NVENC P1 2.45 ms (p95
   2.61), NVENC P4 3.68 ms (p95 3.77); round-trip luma PSNR at about the same data rate: Media Foundation
   70-76 dB, NVENC P4 66-68 dB, P1 61-63 dB (NVENC with a one-frame VBV; keyframes may use 8x a frame's bits,
   2x gave a 19 dB first picture). So on NVIDIA the direct path saves about 0.5 ms only with the fastest,
   lowest-quality preset; Media Foundation stays the default. What the direct SDK adds is control Media
   Foundation does not expose (bitrate changes without a keyframe, intra refresh, 4:4:4 for sharp text).
   **AV1 is the default where both ends can (user decision 2026-11-25: "AV1 encoding as the default, the existing
   encoding as fallback"):** with the viewer's codec on Automatic (its default) the agent makes AV1 when the viewer's
   GPU decodes it and the agent has an AV1 encoder: NVENC directly (API 12; Windows and Linux), else on Windows the
   GPU's AV1 encoder through Media Foundation (Intel Arc, AMD, NVIDIA with an older driver); otherwise H.265 where
   both GPUs do it (since 2026-11-26: on sunshine's Iris Xe about 8 dB more than H.264 at 5 Mbit/s, docs/PARITY.md),
   then H.264. NVENC's AV1 is 8-bit 4:2:0 from NV12; 4:4:4 and 10 bits stay H.265, separate switches (off by
   default) that never change which codec Automatic takes. NVENC does not apply
   the keyframe scale to AV1 (its first picture got one frame's bits: 41 kB, 20.5 dB on the 5080 at 12.4 Mbit/s), so
   the buffer is 8 frames for each AV1 keyframe alone (165 kB, 35.6 dB; H.264 with the scale 255 kB, 33.7 dB) and one
   frame again right after (a larger buffer for every frame let P-frames reach 2.4x their share). Tested: the PC (RTX
   5080) makes AV1 through NVENC for the M4 viewer, explicitly and by default; with `QUEQIAO_NVENC_API=11` NVIDIA's
   AV1 Media Foundation encoder takes over; Linux (RTX 4090) makes AV1 and its viewer decodes it with NVDEC
   (run_m3_linux.sh av1, av1_api11 falls back to H.264). Between two machines (the PC and an Intel Iris Xe laptop,
   direct over the internet, 2026-11-25): the laptop's agent has no AV1 encoder and sends H.264 through Quick Sync;
   the PC's agent sends AV1 through NVENC, which the laptop's viewer decodes on its GPU (419 of 420 frames AV1).
10. **Build requirements:** Rust, Visual Studio Build Tools (C++), and NASM on the PATH: without NASM, OpenH264
   builds silently without its assembly and runs about 5x slower (measured 35 ms instead of 6.6 ms per 1080p
   frame); `scripts\check.ps1` warns when NASM is missing.
11. **Antivirus must not block it.** A remote-control agent looks like a remote-access trojan to antivirus
   heuristics: an unknown program that captures the screen, injects input and talks to a server. Seen on
   2026-11-20: Kaspersky Endpoint Security on the test laptop locked the M3 agent as `VHO:Trojan.Win32.Agent.gen`
   (cloud heuristic) minutes after it arrived, so it could not start; the M2 agent had run there. Before release:
   Authenticode signing of every binary (a code-signing certificate; EV builds reputation fastest), version
   information in each executable, the visible session indicator on the controlled PC (tray icon and banner),
   and false-positive submissions to the vendors common on Chinese PCs (Kaspersky, Huorong, Tencent PC Manager,
   360) as UU Remote and others do. Test machines exclude their test folders instead; never rename or pack a
   binary to get past a scanner.

## Components

| Component | Runs on | Job |
|---|---|---|
| `queqiao-server` | server (the PC for testing) | Accounts, devices, presence, session tokens, WebSocket signaling (offer/answer/candidates), STUN + TURN relay. HTTPS API + WebSocket + UDP. SQLite. |
| `queqiao-service` | each controlled PC, Windows service (LocalSystem), no network | In the console session it keeps two processes running with a private channel between them: its desktop helper (SYSTEM, the only account that may see and use the sign-in, lock, Ctrl+Alt+Del and UAC screens) and `queqiao-agent` as the person signed in; starts both again when the session at the screen changes, either ends, or the agent asks (restarting Queqiao); Ctrl+Alt+Del (SendSAS). |
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
- **As built in M4:** STUN and TURN over UDP are our own code (`queqiao-core::stun`, a sans-IO TURN client in
  queqiao-transport, a tokio TURN server in queqiao-server); no third-party TURN crate. Before the offer or answer each
  side gathers its host candidate, its public address (from the allocation, or a STUN Binding) and a relayed
  address, within 1.5 s. Relay credentials are issued over the account connection only, per device, valid an hour
  for new allocations (username "<expiry>:<account>:<device>:<program>", password HMAC-SHA1 with a secret made at
  each server start); a logout, a removed device or a new password revokes them and ends their allocations.
  The server relays only to peers with a permission, never to loopback, link-local, multicast or reserved addresses
  or to this machine's own addresses and services, to private and 100.64/10 addresses only with `--relay-private`;
  on its own public address a permission is for one relayed address, not the whole IP; 4 allocations per device
  (16 per account); every request after the
  Allocate is checked against the allocation's key. str0m's controlled ICE side asks for its first timeout years away
  until it is given time, which deadlocks behind a TURN permission: the network thread starts with a timeout and
  waits at most 50 ms until connected (tests/relay_ice.rs reproduces both). `QUEQIAO_FORCE=network:relay` (or
  `--force network:relay`) offers only the relayed candidate.
- **Relay: TURN.** `queqiao-server` includes a TURN server (our own, see "As built in M4" above); both sides get relay candidates from it, so ICE
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
  As built in M3: each program (agent, viewer) keeps one TLS WebSocket to the server and logs in on it once,
  with the password or its refresh token, so no separate access token is needed. Refresh tokens are random 256-bit
  values, stored on the server only as SHA-256, valid for one device and one program, 90 days, and revoked by
  logging out; on the device they are encrypted with DPAPI for the Windows user. A failed login is answered
  after 1 s.
- The server's certificate: a self-hosted server makes its own on first start and prints its SHA-256
  fingerprint; clients pin it (no CA needed, and no other certificate is accepted). A server with a CA-issued
  certificate can be used the same way; CA checking for the public server comes with the installer (M7).
- A device joins an account after login; new devices are shown to the account's other devices.
- Rule: only devices on the same account can control each other (checked by the server when it issues a session
  token, and again by the controlled agent against its pinned list).
- The controlled PC always shows that a session is active (tray icon + banner) and can end it.

## Security first

The user, 2026-11-20: "security is also the most important thing". Remote control hands a PC to another device,
so a security gap outranks any feature or speed gain, and the milestones below wait for it.

What is to be prevented: someone controlling a PC that is not theirs; someone watching or changing a session;
a stolen password, token or device; a compromised or impersonated server; malware using the agent.

Built so far (M2-M3): video and input encrypted end to end (WebRTC DTLS-SRTP); the server's certificate pinned
by the clients; passwords stored with Argon2id; refresh tokens random, stored as hashes, per device and program,
revocable, DPAPI-encrypted on the device; signals only between devices of one account (checked by the server,
tested from outside by `account_probe`); failed logins slowed to one per second per connection; message sizes
capped.

Open, in this order, all before any release:
1. **Peers must prove who they are, not only the server.** **Done 2026-11-21:** every device has an Ed25519 key
   (DPAPI-protected, `device_key.json`), registered with the server (a password login may register a new key, a
   saved-login resume may not) and published in the device list; every signal (hello, offer, answer) is signed over
   session, kind, sender, receiver and body, and the receiving device checks it against the key it pinned at first
   contact (`known_devices.json`); a changed key is refused until confirmed (`--trust-new-key <device id>`).
   The agent and the viewer share that file and change it one at a time under `known_devices.lock`, each starting
   from what the other wrote (the security review of 2026-11-22: pins kept in memory were lost to the other's
   write, and the next contact pinned again on the server's word).
   Checked from outside by `account_probe` (keys in the list, signature checks out, a forged one is refused).
   Was: Today the DTLS fingerprints in the offer and answer
   pass through the server, so a compromised server could sit in the middle of a session. Each device gets its
   own long-lived key (DPAPI-protected) at its first login; the account's devices learn each other's public keys
   and pin them (a changed key is refused until the user confirms it on a device they trust); the DTLS
   fingerprint in every offer and answer is signed with the device key and checked by the other side.
2. **Nothing happens unseen on the controlled PC.** **Done 2026-11-21 (except the device password):** a tray
   icon whenever the agent runs (re-added when Explorer restarts; menu: end the session, let this account's devices
   control the PC, ask before each session, quit); a banner on top of the screen during every session, with End
   (a session whose banner cannot be shown is refused); an Allow / Deny prompt before each session when asking is on
   (Deny is the default button; denied after 25 s); every session in `sessions.log`. As UU Remote does, the
   account's own devices connect without a prompt by default (unattended access to your own PCs), and the person at
   the PC can turn that off or turn the prompt on; the separate device password belongs to helping devices of other
   accounts (planned). Tested by `run_m4_local.ps1` (allowed, declined, ended at the PC).
   Was: A tray icon whenever the agent runs, a banner during every
   session with a one-click end, and a list of past sessions. A session needs consent on the controlled PC
   (Allow / Deny) unless its owner turned on unattended access for that device, which then needs a separate
   device password (as UU Remote does).
3. **Logins:** **Done 2026-11-21 except TOTP:** 20 failed logins per address in 10 minutes, 5 wrong passwords per
   account name in 15 minutes (names that do not exist count the same, so a lockout tells nothing; saved logins
   keep working; a wrong current password in a password change counts too); a new device is announced to the
   account's other devices; removing a device ends its saved logins and disconnects it; a password change needs
   the current one, revokes every saved login of the account and closes the other connections. Checked from
   outside by `account_probe` (41 checks). Still open: optional two-factor codes, and leaving the test-only
   `--password` option out of release builds. Was: limits per account and per address with lockout, optional two-factor codes (TOTP), a notice to
   the account's devices when a new device logs in, removing a device revokes its tokens, a password change
   revokes all of them. Test-only options (`--password` on the command line) are left out of release builds.
4. **The agent accepts only what it must:** no listening ports; input only from the session the server
   introduced and the device key confirmed; with the service (M5), the SYSTEM part stays minimal and talks to
   the user-session agent only through an authenticated local channel.
   *Done 2026-11-21 for the person signed in:* the SYSTEM part (`queqiao-service.exe`, the service and its desktop
   helper) has no network; everything from the network is read by the agent, as the person. The channels are
   anonymous pipes the service creates and hands to exactly one process each when it starts it
   (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`): nothing else can open them. The helper checks every message as strictly as
   network input (fixed sizes, a picture exactly width x height x 4, short texts; robustness test), ends at the
   first one it does not know, injects input only during a session and never on the person's own desktop (there
   the agent injects with the person's rights). The agent's process is created with a descriptor that lets its
   own user only wait for it and end it: other programs of that person cannot copy its handles (the channel to
   the helper), write into it or start threads in it (checked by `run_m5_service.ps1` with a program of the same
   user without debug rights). It runs at High integrity (the security review of 2026-11-22: threads it made later
   could be opened, and a global window hook could load code into it): programs at the person's ordinary level
   (Medium) can neither hook it nor send its windows messages, except its tray icon's (`run_m5_service.ps1` with
   a Medium program), nor open its later threads; what it starts (the viewer, Explorer, command lines) runs at
   Medium again. An administrator's full token is High already: there the protection is the descriptor alone. The programs are copied to `%ProgramFiles%\Queqiao` (administrators only) and the
   log folder `%ProgramData%\Queqiao` is set so that only SYSTEM and administrators may change it.
   What remains: threads the agent starts later get the person's default descriptor (the person's programs at
   High could open them, Medium ones no longer); an administrator's elevated programs have debug rights and may open any process anyway; and Microsoft
   does not count UAC as a security boundary for administrators. For a standard user, a UAC prompt asks for an
   administrator's password, which input alone cannot give.
5. **The code and the release:** `cargo audit` / `cargo deny` on every build, fuzzing of everything that parses
   network input (the data-channel protocol, the server's JSON), signed binaries and signed updates (see also
   point 11 of "Runs on any PC"), and a review of all of the above before the first public build.
   *Partly done (2026-11-21):* every decoder of the other side's bytes (the session protocol, STUN and
   ChannelData, file and command-line messages) has a robustness test: 200 000 random and mutated inputs each,
   which must never panic, and whatever is accepted must encode back to the same message. The receiving side of
   file transfer gets random streams of offers, chunks and endings and must leave nothing outside its folder and
   no part files. They found two faults, both fixed: a frame delay that was NaN or infinite was accepted (now
   refused), and a second offer under a number still on its way replaced the first and left its part file
   behind (now refused). The receiver also refuses more than 16 files at once and files larger than the free
   disk space less 1 GB. Still to do: `cargo audit` / `cargo deny` (installing them on the build PC is the
   user's call), coverage-guided fuzzing, signed binaries.

**Security review, 2026-11-22.** Four reviews of the whole code (server and accounts, the service's privilege
boundary, installing and updating, a session in both directions) found about 35 weaknesses; they are fixed in order
of severity. The user's decisions: the account's own devices still connect without a prompt (as UU Remote), but every
PC shows when a device joins the account or a device's key changes; Help stays the only way in from another account
(help ID and code, or an invitation, and the person's Allow each time); someone helping can never operate UAC prompts
or the Ctrl+Alt+Del screen, and cannot copy files out.

- **Test builds and shipping builds.** The options the tests need (`--connect` / `--listen` without any account,
  `--answer allow`, a login on the command line, `--report`, `--send-file`, the update feed in the registry, the
  service's stored agent options ...) exist only with the `test-switches` features: `cargo build-test`,
  `clippy-test` and `run-tests` (.cargo\config.toml) are what `scripts\check.ps1` and the test scripts use. A shipping
  build is a plain `cargo build`: the agent reads only what the service passes, the viewer only a queqiao: link.
  `scripts\package.ps1 -Ship` makes it in target\ship and checks that each program refuses those options;
  `check.ps1` lints and unit-tests the shipping configuration too. `--logon` (the agent for the sign-in screen,
  which shows nothing on the screen) is refused unless the agent runs as LocalService, as only the service can start it.
- **queqiao: links** take exactly the one value after `--invite`: a link with a quote in it cannot add options.
- **The service's folders.** Users may make folders in %ProgramData% and own them, so `%ProgramData%\Queqiao` is
  never taken over: `install` removes whatever is at the path (links as themselves) and creates it anew with its
  final security in one step; each update's installer gets a new folder for SYSTEM and administrators only.
  Program Files and ProgramData come from HKLM, not from environment variables an elevated program inherits.
- **The administrator's step of an install.** The installer carries its programs as a resource of its own image
  (not appended to the file), so what it reads as an administrator or as SYSTEM is what Windows checked when it
  started. With the service chosen, the installer itself runs as an administrator (`--install-service`): the
  service and agent go from its own files into a new folder in Program Files, and `queqiao-service install` runs
  from there; nothing comes from the person's folder (`%LOCALAPPDATA%\Programs\Queqiao`), which any program of
  theirs may change. Every program is linked with `/DEPENDENTLOADFLAG:0x800` (imports from System32 only: a
  WTSAPI32.dll put next to the installer was loaded before), and the installer and the service load DLLs by name
  only from System32 (`SetDefaultDllDirectories`).
- **Logins.** Each password or saved-login try is counted before it is checked, under one lock, and forgiven after
  when it was right or no guess: tries at once on many connections count as many (before, all passed the check and
  were counted after). New accounts per address the same way. User names are one account in any case. Changing the
  password counts as a try too (a saved login could otherwise guess it without limit).
- **The server's resources.** At most 4 MB waits to go out to one connection, and each write has 10 s: a client
  that stops reading is closed, not fed. Connections not logged in yet: 32 per address, 2048 in all. An error from
  accepting a connection or from one UDP datagram is logged; it no longer ends the server.

## Milestones

| # | Result | Check |
|---|---|---|
| M0 | Toolchain, Cargo workspace, empty crates build | `cargo build` passes |
| M1 | Local loop on one PC: capture -> encode -> decode -> window, with the software fallbacks | latency and fps measured; every fallback forced and tested. **Done 2026-11-20**: RTX 5080 PC 4/4 paths pass (hardware: 12 ms median screen change -> shown); Intel Iris Xe laptop 3/4 (hardware 20 ms; no-GPU path 55 ms, needs adaptive size) |
| M2 | LAN: agent + viewer by IP, video + mouse/keyboard | control the PC from a second process / PC. **Done 2026-11-20**: one PC (611/611 frames, input 7/7) and PC viewer -> Intel laptop agent over Tailscale (direct, 5 ms ping): 17 ms median / 29 ms p95 screen change -> shown, input checked 7/7, 2 frames lost in 20 s and recovered by keyframe requests |
| M3 | `queqiao-server`: accounts, devices, presence; device list in the viewer | log in, see devices, connect by click. **Done on one PC 2026-11-20** (`scripts\run_m3_local.ps1`): account probe 17/17 (pinned certificate, wrong password answered after 1 s, tokens per device and program, logout revokes, live presence, other accounts cannot see or reach a device); viewer through the login form and device list controls the PC through the server (connected in 107-137 ms, all frames shown, input 7/7); saved login, logout. Across machines the same day (server and viewer on the PC, agent on the Intel laptop over Tailscale, after the laptop's Kaspersky was told to allow the test folder): connected in 467 ms, first picture 603 ms, 746/746 frames shown (4 after a loss, repaired by keyframe requests), screen change -> shown median 33 ms / p95 40 ms at a 29 ms round trip, Intel Quick Sync at 2240x1400 60 fps, input 7/7 |
| M4 | NAT traversal + relay | connect across networks, forced-relay test. **Relay done 2026-11-21**: one PC (`scripts\run_m4_local.ps1`): probe 27/27 incl. 10 relay checks (no credentials / wrong password / forged expiry refused, no relaying to loopback or the server's services, unsigned requests refused, quota, relaying works); a normal session goes direct, a forced one through TURN (connected in 396 ms, all frames, input 7/7). PC viewer -> Intel laptop agent over Tailscale: direct 599 ms to connect, 36 ms median / 43 p95 screen change -> shown; forced relay 793 ms, 34 / 42 ms; input 7/7 both. **Over the internet 2026-11-21** (tools\m4_remote.ps1 -PcAddress 59.66.29.128 -Public: the server on the PC's public address, inbound open only to the laptop's addresses; the laptop on campus Wi-Fi behind a proxy in TUN mode): direct through the laptop's Wi-Fi socket, first picture 0.44 s after connecting, 567 of 568 frames shown, 20 ms median screen change -> shown, input 7/7; forced relay 238 ms (the laptop's TURN socket goes through its proxy), input 7/7. Found on the way: the agent finished connecting about 3 s before the viewer, its first keyframe was dropped and no loss told it to send another (first picture after 19 s): the viewer now asks for a keyframe for every frame before its first picture that is not one. Connecting took 3.4 to 6 s: the viewer's first ClientHello reached the agent's Wi-Fi socket before the agent's ICE had a pair and was dropped (rtc.accepts, used on the extra sockets), so DTLS waited for resends; without it the whole handshake takes 30 ms after ICE: connected after 2.0 s (viewer) and 1.65 s (agent), first picture 2.2 s, 730 of 730 frames, 7 ms round trip, 20.7 ms median, input 7/7; relayed 3.3 s, 238 ms. Open: TURN over TCP/TLS for networks without UDP. Host candidates on every uplink: done 2026-11-21 (the first internet attempt between the PC and the laptop failed because both run a proxy in TUN mode holding the default route: UDP on "any" left through the tunnel and the answers came back from the proxy's exit, found with pktmon; every socket is now bound to its interface's address, and the server's to its public address). Loss on the laptop's Wi-Fi (66 frames after a loss in 20 s, keyframes as unpaced bursts) led to pacing and adaptive bitrate (2026-11-21): str0m's bandwidth estimation and pacer (it paces at 1.1x the estimate), the encoder at 85 % of the estimate (Media Foundation, NVENC and OpenH264 change bitrate without a keyframe), starting at 8 Mbit/s as UU does. Two root causes found on the way with an in-memory bottleneck test (tests/loss_recovery.rs: 20 Mbit/s, 50 ms queue, 0.5 % loss): str0m's pacer releases one packet per timer input and asks again a fraction of a millisecond later, so a loop that sleeps at least 1 ms capped paced video near 10 Mbit/s (deadlines under 1 ms away are now served at once, never more than 1 ms ahead of real time); measured there: defaults 2 frames with gaps, median 38 ms; tuned 0 gaps, median 34 ms, p95 81 ms. The laptop re-test waits for its antivirus (each new build is flagged again) |
| M5 | `queqiao-service`: login screen, UAC, Ctrl+Alt+Del, restart | control at the lock screen. **Part 1 done 2026-11-21** (`scripts\run_m5_service.ps1`, on the PC): the service installed from `%ProgramFiles%\Queqiao` starts the desktop helper (SYSTEM) and the agent (as the console user) in session 1; Ctrl+Alt+Del through the service (SendSAS); the agent's capture loses the desktop, the helper's pictures go out instead (the viewer showed 12 of that screen), Escape injected by the helper closes it (the person's desktop back 2.1 s after Ctrl+Alt+Del, 0.1 s after Escape; on its own that screen closed after 4.8 s), the agent captures itself again; "Restart Queqiao" ends and starts both; the agent's process protection (point 4). The lock screen and UAC prompts take the same path (the Winlogon desktop) but were not tried: unlocking the PC needs its password, and the PC's account (the built-in Administrator) gets no UAC prompts. **Part 2 done 2026-11-21** (`scripts\run_m5_logon.ps1` on the PC: the console session disconnected through the WTS API, the agent for the sign-in screen as LocalService in session 0 with the device handed over, a viewer got 28 pictures of the sign-in screen through the helper, the banner shown there, Ctrl+Alt+Del and Escape injected, the session connected again and the person's agent back; found on the way: every process of a job must be in the job's session, so the helper and the agent have a job each): with nobody signed in at the screen, the service starts the helper and the agent for the sign-in screen, `queqiao-agent --logon` as LocalService in session 0 on a window station of its own, given this device over its channel (the person's agent hands it to the service while they allow control without asking; kept DPAPI-encrypted for SYSTEM in a file only SYSTEM may open). It shows nothing, takes only the account's own devices, nothing that needs someone signed in, and works the screen through the helper, which also shows the session banner with End on every desktop that is not the person's. `scripts\run_m5_logon.ps1` tests it on a test PC (the console session disconnected and connected again through the WTS API). On the laptop the antivirus (a centrally managed Kaspersky Endpoint Security) holds the service's process before its first instruction (the same program runs at once by hand): an exclusion for `%ProgramFiles%\Queqiao` is the user's call. |
| M6 | Clipboard, file transfer, multi-monitor, audio | each feature end to end |
| M7 | Adaptive bitrate, optional vendor fast paths, installer, auto-update | quality under packet loss / low bandwidth. **Installer done 2026-11-21** (`crates/queqiao-setup`, `scripts\package.ps1`, tested with `scripts\run_m7_setup.ps1` on the PC): one file, the installer with the signed programs appended (name, size, SHA-256 and bytes each; a footer says where they start) and signed after that, so its signature covers them; each file is checked against its SHA-256 again before it is written, and only plain file names are accepted. It installs for the person using the PC, without an administrator: `%LOCALAPPDATA%\Programs\Queqiao`, a Start menu shortcut to the device list, an entry under Settings > Apps that uninstalls, the agent in the Run key if chosen; the service only if chosen (Windows asks for an administrator once; `queqiao-service install` copies its programs to `%ProgramFiles%\Queqiao`, where only administrators may write). An update ends the programs running from the earlier installation, keeps it as `Queqiao.old` until the new one is in place, and puts it back if anything fails. Uninstalling removes all of it, the service too (one UAC prompt). Found by its test: the agent ended at once when it had no login or could not reach its server; it now stays in the tray, offers "Log in..." and tries again in the background (`scripts\run_m7_agent_login.ps1`). **Self-update done 2026-11-22** (`crates/queqiao-update`, tested with `scripts\run_m7_update.ps1`): the feed is two small files next to the newest installer in the latest GitHub release, `queqiao-update.json` (version, installer name, size, SHA-256) and its Ed25519 signature by the release key; every build carries the key's public half and refuses anything else, and the installer must have the manifest's size and SHA-256; only later versions are taken. Downloads use WinHTTP (system proxy and certificates, never HTTPS to HTTP, size and time limits). The agent of a per-person installation looks 2 minutes after it starts and daily (or when asked from the tray), downloads into `%LOCALAPPDATA%\Queqiao\updates`, and between sessions starts the installer with `--update` and leaves; the installer keeps the person's choices, waits for any other installer of the session, starts the new agent and watches it for 8 s (exit code 0 or 4, "left on purpose" and "another agent runs", count as started), and otherwise puts the earlier version back and starts it; the agent notes the version it installs and skips it while it is still older (no retry loop). The release key: made with `queqiao-setup release-key` (DPAPI-encrypted for the publisher, never in this repository), `queqiao-setup release` writes the signed feed. For now a test key on the build PC. With the service (its agent runs from `%ProgramFiles%\Queqiao`, where only administrators may write), the agent updates the person's own installation, then hands the manifest, its signature and the installer's bytes (not a path) to the service over its channel; the service checks all of it again with its own built-in key (signature, SHA-256, a later version than itself), writes the installer under `%ProgramData%\Queqiao\updates` (SYSTEM and administrators only) and starts it as SYSTEM with `--service-update`: the service stopped, `Queqiao` kept as `Queqiao.old`, the new programs in place, the service started, and its new agent must appear within 30 s and keep running 8 s, else the earlier programs go back (tested with `scripts\run_m7_service_update.ps1`: 0.1.0 -> 0.1.1 through the service, then a broken 0.1.2 put back). The sign-in screen's agent may not ask for updates. Still to do: a release key and code-signing certificate for real releases (the user's call), publishing releases. |
| M8 | Release checks on several real machines (NVIDIA, AMD, Intel, virtual machine, ARM64) | the full test list passes on each |
