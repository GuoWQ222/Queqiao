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
| Saved login, log out, remove (unbind) a device from the account | done (removing: "More..." in the device list, 2026-11-21) |
| Device remarks (a name of your own) and favourites (listed first), kept on this PC | done (2026-11-21, "More..." in the device list) |
| Direct on the LAN, through NAT (STUN), through the relay (TURN), forced relay | done (M4); manual "switch to relay" while connected: planned |
| Help someone else: connect to a device of another account with its help ID (9 digits) and a one-use code (8 characters, 40 bits) that the person there reads out from "Let someone help me..."; "Help someone..." in the device list | done (2026-11-21) |
| Help: an own fixed code, asking without a code (the other side approves), invitation links, the code read from the clipboard | planned |
| Consent on the controlled PC before a session (Allow / Deny), tray icon, session banner with End, session history | done (security point 2) |
| Code plus local confirmation for helpers from other accounts (always asked, whatever the settings); 5 wrong codes close help; the code works once | done (2026-11-21) |
| Auto-lock the client's settings while being helped (the helper holds the mouse; only ending the session stays) | done (2026-11-21) |
| Lock the client's settings with a separate password (stored as PBKDF2-HMAC-SHA256 on this PC) | done (2026-11-21, tray menu); the prompts are not driven by the automatic tests |
| Network status: delay, frame rate, data rate, direct or relayed | done (2026-11-21, in the session window title, once a second); an overlay on the picture: planned (UI) |

## Picture

| Feature | Status |
|---|---|
| Hardware encode and decode on any GPU, software fallback | done (M1) |
| Adaptive bitrate from the network's bandwidth estimate, paced sending, retransmission of lost packets | done (M4) |
| Frame-rate cap (30, 60, or what that screen shows, up to 240) and quality presets (smooth, balanced, sharp = up to twice the size-based bitrate) from the session window menu | done (2026-11-21) |
| Resolution choice during the session (that screen's own, 1080p, 720p) | done (2026-11-21, session window menu) |
| Custom bitrate ceiling (0.5 to 200 Mbit/s) from the session window menu | done (2026-11-21) |
| Codec choice: H.264, H.265, AV1 when both ends have hardware | planned |
| True colour (4:4:4) for sharp text; HDR | planned |
| Several monitors: switch screens during the session ("Screens" in the session window menu) | done (2026-11-21); tested by switching on a one-screen PC (both test PCs have one screen) - a real second screen still to be tried |
| Several monitors each in its own window | planned |
| Virtual displays: create / remove, automatic when no monitor is attached, custom high resolution and refresh ("super screen") | planned: needs an IddCx display driver (Microsoft's MS-PL sample), which must be signed for release |
| Privacy screen: the controlled PC's monitors dark while controlled, the controlling side still sees the desktop | done (2026-11-21, "Darken that PC's screen" in the session window menu; needs Windows 10 2004 or later there; the session banner with End stays above it) |
| Block the controlled PC's own mouse and keyboard while the screen is dark | planned (M5: needs the service) |
| Window: full screen / window, zoom, scaling, small always-on-top window with opacity, several sessions at once | partly: window, full screen (menu or Ctrl+Alt+Enter), always on top (2026-11-21); zoom, opacity, several sessions: planned |

## Input

| Feature | Status |
|---|---|
| Mouse and keyboard by scancode | done (M2) |
| Windows shortcuts (Win, Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc) to the remote while the session window is in front, or kept local | done (2026-11-21, session window menu) |
| Ctrl+Alt+Del | partly: "Send Ctrl+Alt+Del" in the session window menu (the controlled PC answers that it needs the service until M5) |
| Configurable hotkeys, "boss key" | planned |
| Game mouse: relative movement from raw input, the pointer hidden and held in the window (menu or Ctrl+Alt+M; leaving the window gives it back) | done (2026-11-21) |
| Local cursor vs remote cursor, game mouse speed | planned |
| Emulated mouse/keyboard through a virtual HID device (games, secure desktop) | later: needs a signed driver |
| Gamepads (virtual Xbox / DualShock through ViGEmBus) | planned |
| Touch and pen input | later |

## Sound

| Feature | Status |
|---|---|
| The controlled PC's sound on the controlling PC (Opus, 10 ms packets, follows the default device); turn it off from the session window menu | done (2026-11-21) |
| Mute the controlled PC's speakers during the session, put back after (its sound still comes to the viewer) | done (2026-11-21, "Mute that PC's speakers" in the session window menu) |
| The controlling PC's microphone on the controlled PC (virtual microphone) | planned (M6; a virtual audio device needs a driver) |

## Files and clipboard

| Feature | Status |
|---|---|
| Clipboard both ways: text | done (2026-11-21; the person at the controlled PC can turn it off in the tray) |
| Clipboard both ways: images (compressed, checked with SHA-256, up to 64 MB) | done (2026-11-21) |
| Clipboard both ways: files (copy here, paste there) | planned |
| File transfer both ways: drop files on the session window; into Downloads\Queqiao, checked with SHA-256 | done (2026-11-21; receiving can be turned off in the tray) |
| File transfer by copy and paste, folders, a transfer list with pause, resume and cancel | planned (M6) |
| Helpers from other accounts get no file transfer, command lines, privacy screen or restart | done (2026-11-21); a setting to allow file transfer for helpers: planned |

## The controlled PC

| Feature | Status |
|---|---|
| Runs as a service: login screen, UAC prompts, lock screen; unlock with the PC's account password | planned (M5) |
| Restart (after confirming, with Windows' 10-second notice) or lock the controlled PC from the session window menu | done (2026-11-21) |
| Lock the PC when a session ends (tray setting) | done (2026-11-21) |
| Restart Queqiao remotely | planned (M5) |
| Start with Windows (at sign-in, with the saved login), stay in the tray, one agent per user session | done (2026-11-21, "Start with Windows" in the tray); before sign-in: M5 (service) |
| Wake-on-LAN through another Queqiao device of the account on the same network ("Wake up" in the device list) | done (2026-11-21) |
| Wake-on-LAN set-up check (network card and BIOS settings) | planned |
| Remote command line: several at once (ConPTY: PowerShell, or cmd), each in a console window; the person at the PC can forbid them in the tray | done (2026-11-21) |
| Command lines: detach and resume, rename | planned |

## The program

| Feature | Status |
|---|---|
| Installer, self-update with rollback, crash reports | planned (M7) |
| Languages: Simplified Chinese and English, from Windows' display language (QUEQIAO_LANG=en/zh decides instead) | done (2026-11-21) for everything people click and read; the controlled PC's short notices to the viewer are still English |
| Mobile and macOS clients, phone as an extra screen | later (the first version is Windows only) |
