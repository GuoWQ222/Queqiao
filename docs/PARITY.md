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
| Network status overlay: delay, frame rate, bandwidth | planned |

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
| Privacy screen: turn the controlled PC's monitor off or cover it with an image while controlled | planned |
| Window: full screen / window, zoom, scaling, small always-on-top window with opacity, several sessions at once | partly (window); rest planned (UI) |

## Input

| Feature | Status |
|---|---|
| Mouse and keyboard by scancode | done (M2) |
| Ctrl+Alt+Del, system shortcuts to the remote or kept local, configurable hotkeys, "boss key" | planned (M5 for Ctrl+Alt+Del) |
| Local cursor vs remote cursor; game mouse (relative, locked, speed) | planned |
| Emulated mouse/keyboard through a virtual HID device (games, secure desktop) | later: needs a signed driver |
| Gamepads (virtual Xbox / DualShock through ViGEmBus) | planned |
| Touch and pen input | later |

## Sound

| Feature | Status |
|---|---|
| The controlled PC's sound on the controlling PC; mute its speakers during the session, restore after | planned (M6) |
| The controlling PC's microphone on the controlled PC (virtual microphone) | planned (M6; a virtual audio device needs a driver) |

## Files and clipboard

| Feature | Status |
|---|---|
| Clipboard both ways (text, images, files) | planned (M6) |
| File transfer both ways by copy/paste and drag and drop; transfer list with pause, resume, cancel | planned (M6) |
| Allow or forbid file transfer when helping others | planned |

## The controlled PC

| Feature | Status |
|---|---|
| Runs as a service: login screen, UAC prompts, lock screen; unlock with the PC's account password | planned (M5) |
| Restart the PC, restart Queqiao, lock the PC when the session ends | planned (M5) |
| Start with Windows, stay in the tray | planned (M5) |
| Wake-on-LAN through another Queqiao device on the same LAN, with a set-up check (network card, BIOS) | planned |
| Remote command line: several terminal sessions (ConPTY), detach and resume, rename | planned |

## The program

| Feature | Status |
|---|---|
| Installer, self-update with rollback, crash reports | planned (M7) |
| Languages (Chinese, English) | planned (UI) |
| Mobile and macOS clients, phone as an extra screen | later (the first version is Windows only) |
