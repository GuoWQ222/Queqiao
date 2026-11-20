# Feature parity with UU Remote

The user, 2026-11-21: every function UU Remote has, Queqiao has too. This list comes from UU Remote 4.48.1's
interface text and its program and driver inventory (research notes kept outside this repository). It names what
each feature does, in our own words; nothing of UU's is copied. NetEase's own service business (cloud PCs, queues,
U-coin payments, memberships, real-name checks, the MuMu emulator) does not apply to Queqiao and is left out.

Status: **done**, **partly**, **planned** (with the milestone), **later** (after the Windows release).

## Connecting

| Feature | Status |
|---|---|
| Account, devices on the account, presence, connect by double-click | done (M3) |
| Saved login, log out, remove (unbind) a device from the account | partly (M3: saved login, log out; removing a device: planned M5) |
| Device names, remarks, favourites | planned (UI) |
| Direct on the LAN, through NAT (STUN), through the relay (TURN), forced relay | done (M4); manual "switch to relay" while connected: planned |
| Help someone else: connect to a device of another account with its device ID and a code (temporary code, own fixed code, or both), or ask and let the other side approve; invitation link; code read from the clipboard | planned (after security points 1-2) |
| Consent on the controlled PC before a session (Allow / Deny), tray icon, session banner with End, session history | done (security point 2) |
| Code plus local confirmation for helpers from other accounts, auto-lock the client UI while being helped | planned (with helping others) |
| Lock the client's settings with a separate password | planned |
| Network status: delay, frame rate, data rate, direct or relayed | done (2026-11-21, in the session window title, once a second); an overlay on the picture: planned (UI) |

## Picture

| Feature | Status |
|---|---|
| Hardware encode and decode on any GPU, software fallback | done (M1) |
| Adaptive bitrate from the network's bandwidth estimate, paced sending, retransmission of lost packets | done (M4) |
| Resolution, frame-rate cap (up to 144), bitrate (custom, very high), quality presets | partly (fixed rules and adaptive bitrate today); settings: planned |
| Codec choice: H.264, H.265, AV1 when both ends have hardware | planned |
| True colour (4:4:4) for sharp text; HDR | planned |
| Several monitors: switch, or each in its own window (drag a screen tab out) | planned (M6) |
| Virtual displays: create / remove, automatic when no monitor is attached, custom high resolution and refresh ("super screen") | planned: needs an IddCx display driver (Microsoft's MS-PL sample), which must be signed for release |
| Privacy screen: the controlled PC's monitors dark while controlled, the controlling side still sees the desktop | done (2026-11-21, "Darken that PC's screen" in the session window menu; needs Windows 10 2004 or later there; the session banner with End stays above it) |
| Block the controlled PC's own mouse and keyboard while the screen is dark | planned (M5: needs the service) |
| Window: full screen / window, zoom, scaling, small always-on-top window with opacity, several sessions at once | partly: window, full screen (menu or Ctrl+Alt+Enter, 2026-11-21); rest planned (UI) |

## Input

| Feature | Status |
|---|---|
| Mouse and keyboard by scancode | done (M2) |
| Ctrl+Alt+Del, system shortcuts to the remote or kept local, configurable hotkeys, "boss key" | partly: "Send Ctrl+Alt+Del" in the session window menu (the controlled PC answers that it needs the service until M5); rest planned |
| Local cursor vs remote cursor; game mouse (relative, locked, speed) | planned |
| Emulated mouse/keyboard through a virtual HID device (games, secure desktop) | later: needs a signed driver |
| Gamepads (virtual Xbox / DualShock through ViGEmBus) | planned |
| Touch and pen input | later |

## Sound

| Feature | Status |
|---|---|
| The controlled PC's sound on the controlling PC (Opus, 10 ms packets, follows the default device); turn it off from the session window menu | done (2026-11-21) |
| Mute the controlled PC's speakers during the session, restore after | planned |
| The controlling PC's microphone on the controlled PC (virtual microphone) | planned (M6; a virtual audio device needs a driver) |

## Files and clipboard

| Feature | Status |
|---|---|
| Clipboard both ways: text | done (2026-11-21; the person at the controlled PC can turn it off in the tray) |
| Clipboard both ways: images and files | planned (M6, with file transfer) |
| File transfer both ways: drop files on the session window; into Downloads\Queqiao, checked with SHA-256 | done (2026-11-21; receiving can be turned off in the tray) |
| File transfer by copy and paste, folders, a transfer list with pause, resume and cancel | planned (M6) |
| Allow or forbid file transfer when helping others | planned |

## The controlled PC

| Feature | Status |
|---|---|
| Runs as a service: login screen, UAC prompts, lock screen; unlock with the PC's account password | planned (M5) |
| Restart (after confirming, with Windows' 10-second notice) or lock the controlled PC from the session window menu | done (2026-11-21) |
| Restart Queqiao, lock the PC when the session ends | planned (M5) |
| Start with Windows, stay in the tray | planned (M5) |
| Wake-on-LAN through another Queqiao device on the same LAN, with a set-up check (network card, BIOS) | planned |
| Remote command line: several at once (ConPTY: PowerShell, or cmd), each in a console window; the person at the PC can forbid them in the tray | done (2026-11-21) |
| Command lines: detach and resume, rename | planned |

## The program

| Feature | Status |
|---|---|
| Installer, self-update with rollback, crash reports | planned (M7) |
| Languages (Chinese, English) | planned (UI) |
| Mobile and macOS clients, phone as an extra screen | later (the first version is Windows only) |
