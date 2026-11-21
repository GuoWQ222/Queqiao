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
| The controlled PC logs in from its tray ("Log in..."), and stays in the tray while it is offline: it tries again by itself (1 s, 2 s, ... 30 s) | done (2026-11-21, `scripts\run_m7_agent_login.ps1`: logged in through the form, the server stopped and started again, back online by itself); found by the installer test: before, an agent that could not reach its server, or had no login, ended at once |
| Device remarks (a name of your own) and favourites (listed first), kept on this PC | done (2026-11-21, "More..." in the device list) |
| Direct on the LAN, through NAT (STUN), through the relay (TURN), forced relay | done (M4); past a VPN or proxy tunnel holding the default route (Clash/Mihomo): a socket and candidates on every uplink, the server's UDP on its own address (2026-11-21); tested over the internet 2026-11-21 between the PC (public address) and the laptop behind its campus network and proxy: direct 20 ms median screen change -> shown, relayed 238 ms, input 7/7 both; manual "switch to relay" while connected: planned |
| Help someone else: connect to a device of another account with its help ID (9 digits) and a one-use code (8 characters, 40 bits) that the person there reads out from "Let someone help me..."; "Help someone..." in the device list | done (2026-11-21) |
| Help: asking without a code (the person there approves in a prompt; only while they take help requests, one ask at a time) | done (2026-11-21) |
| Help: an own fixed code, invitation links, the code read from the clipboard | partly (2026-11-22): "Copy invitation" in the help window (text with the help ID, the code and a queqiao://help link); the helper's help form fills itself from an invitation on the clipboard or from the link (the installer registers queqiao:); connecting stays the helper's click (`run_m4_local.ps1` help-link session). A fixed help code (2026-11-22, tray, behind the settings password, DPAPI-encrypted in the settings): help stays open with it (the help ID is the device's own and does not change), opens again after each use and 15 minutes after five wrong codes closed it; someone of another account is still asked every time, unattended control stays for the account's own devices (`run_m4_local.ps1` help-fixed session: two helpers in turn with the same code) |
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
| Virtual displays: create / remove, automatic when no monitor is attached, custom high resolution and refresh ("super screen") | not needed for now (the user, 2026-11-21); it needs an IddCx display driver, which must be signed for release |
| Privacy screen: the controlled PC's monitors dark while controlled, the controlling side still sees the desktop | done (2026-11-21, "Darken that PC's screen" in the session window menu; needs Windows 10 2004 or later there; the session banner with End stays above it) |
| Block the controlled PC's own mouse and keyboard while the screen is dark | done (2026-11-22): with the privacy screen, low-level keyboard and mouse hooks on a thread of their own let only injected input through (no service needed); Ctrl+Alt+Del gives them back (with the service, Windows' own screen ends the privacy screen and the block; without it, that ends the session). Tested in `run_m4_local.ps1` (the hooks set, the viewer's input test passing through them); held-back hardware input cannot be produced by a test (SendInput is always marked injected) |
| Window: full screen / window, zoom, scaling, small always-on-top window with opacity, several sessions at once | partly: window, full screen (menu or Ctrl+Alt+Enter), always on top (2026-11-21); opacity (100/80/60/40 %, layered window; `run_m4_local.ps1` relay session at 60 %, the picture and input still checked) and actual size 1:1 (the window fitted to the picture, at most the work area; not driven by a test) (2026-11-22); several sessions at once: planned (one account connection must carry several sessions: a second viewer process would log in as the same device and role, which the server keeps one connection for) |

## Input

| Feature | Status |
|---|---|
| Mouse and keyboard by scancode | done (M2) |
| Windows shortcuts (Win, Alt+Tab, Alt+Esc, Ctrl+Esc, Ctrl+Shift+Esc) to the remote while the session window is in front, or kept local | done (2026-11-21, session window menu) |
| Ctrl+Alt+Del | done (2026-11-21): "Send Ctrl+Alt+Del" in the session window menu, through the Queqiao service (without the service the controlled PC answers that it needs it) |
| Configurable hotkeys, "boss key" | done (2026-11-22): "Hotkeys..." in the session menu sets the full-screen and game-mouse keys and a boss key (Ctrl and/or Alt, Shift optional, with a letter, digit, F-key or Enter, Space, Home and the like; kept in viewer_settings.json). The boss key is registered with Windows, so it works with the window hidden: it hides the session window and its sound at once, and brings both back. Off until the person sets one. Tested in `run_m4_local.ps1` (relay session: registered, hidden, shown again) |
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
| Clipboard both ways: files and folders (copy here, paste there: they go over at once, up to 512 MB, and the other side's clipboard holds them once they arrived) | done (2026-11-21) |
| File transfer both ways: drop files on the session window; into Downloads\Queqiao, checked with SHA-256 | done (2026-11-21; receiving can be turned off in the tray) |
| File transfer of whole folders (inside kept, every path part made safe) | done (2026-11-21) |
| A transfer list with progress, pause, resume and cancel ("Transfers..." in the session window menu) | done (2026-11-21) |
| File transfer by copy and paste | done (2026-11-21, with the clipboard) |
| Helpers from other accounts get no file transfer, command lines, privacy screen or restart | done (2026-11-21); a setting to allow file transfer for helpers: planned |

## The controlled PC

| Feature | Status |
|---|---|
| Runs as a service: login screen, UAC prompts, lock screen; unlock with the PC's account password | partly (2026-11-21): with the service (`queqiao-service install`), the lock, Ctrl+Alt+Del and UAC screens of the person signed in are shown and worked through its SYSTEM desktop helper during a session (tested with the Ctrl+Alt+Del screen, with its banner there too); the sign-in screen with nobody signed in: done (2026-11-21, tested on the PC with `scripts\run_m5_logon.ps1`: the agent for the sign-in screen as LocalService, the device handed over by the person's agent and kept for SYSTEM only; pictures, the banner and input there); a session that starts on the lock screen: written, not yet tried |
| Restart (after confirming, with Windows' 10-second notice) or lock the controlled PC from the session window menu | done (2026-11-21) |
| Lock the PC when a session ends (tray setting) | done (2026-11-21) |
| Restart Queqiao remotely | done (2026-11-21, "Restart Queqiao on that PC" in the session window menu; needs the service) |
| Start with Windows (at sign-in, with the saved login), stay in the tray, one agent per user session | done (2026-11-21, "Start with Windows" in the tray; with the service installed, the service starts Queqiao for whoever signs in and the Run key's copy leaves); before sign-in: M5 (service) |
| Wake-on-LAN through another Queqiao device of the account on the same network ("Wake up" in the device list) | done (2026-11-21) |
| Wake-on-LAN set-up check (network card and BIOS settings) | done (2026-11-22): "Wake-on-LAN check..." in the tray: for each physical card, may it wake the PC (`powercfg -devicequery wake_armed`, no administrator needed) and does its driver wake on a magic packet (`*WakeOnMagicPacket`), and whether Fast Startup is on; what to set where it is not (Device Manager, Power Options, and Wake on LAN in the BIOS/UEFI, which Windows cannot read). Nothing is changed. `queqiao-agent --wol-check` on the PC: the wired card and the Wi-Fi card both may wake it, Fast Startup off |
| Remote command line: several at once (ConPTY: PowerShell, or cmd), each in a console window; the person at the PC can forbid them in the tray | done (2026-11-21) |
| Command lines: detach and resume, rename | done (2026-11-22): "Command lines..." in the session menu: detach (the window closes, the command line keeps running there, its output kept up to 256 KB), show again (the kept output first), rename (the window title), end. None outlives the session, so nothing keeps running unseen afterwards. Tested in `run_m4_local.ps1` (the terminal test detaches first, types while detached, shows the window again) |

## The program

| Feature | Status |
|---|---|
| Crash reports kept on the PC (what panicked, where, the call stack; newest 20 in %APPDATA%\Queqiao\crashes) | done (2026-11-21); sending them anywhere: not without the user's say |
| Installer | done (2026-11-21, M7 part 1): `queqiao-setup-<version>.exe`, one signed file (`scripts\package.ps1`; a test certificate for now) carrying the signed programs, each checked against its SHA-256 before it is written; installs for the person without an administrator (`%LOCALAPPDATA%\Programs\Queqiao`, Start menu, Settings > Apps entry to uninstall, "Start with Windows" if chosen), and if chosen the service for the sign-in, lock and UAC screens (one UAC prompt); an earlier installation is replaced (its programs ended first) or put back if anything fails; `--quiet` and `--uninstall`. Tested on the PC with `scripts\run_m7_setup.ps1` (the window, install, update, uninstall, with and without the service) |
| Self-update with rollback | done (2026-11-22, M7 part 2) for Queqiao installed per person: the agent looks at the update feed (the latest GitHub release's files) 2 minutes after it starts and once a day, or from the tray ("Check for updates..."); only a manifest signed with the release key (Ed25519, its public half built in) and an installer with the manifest's size and SHA-256 are taken; installed between sessions, by itself unless "Install updates automatically" is off; the installer puts the earlier version back when the new agent does not keep running, and that version is not tried again. Tested with `scripts\run_m7_update.ps1` (0.1.0 -> 0.1.1, then a broken 0.1.2 rolled back). With the service (2026-11-22): its agent updates the person's installation and hands the update to the service, which checks it again itself and replaces its programs in Program Files as SYSTEM, put back if the new agent does not keep running (`scripts\run_m7_service_update.ps1`). Not yet: no release is published (the release key is a test key) |
| Languages: Simplified Chinese and English, from Windows' display language (QUEQIAO_LANG=en/zh decides instead) | done (2026-11-21) for everything people click and read; the controlled PC's short notices to the viewer are still English |
| Mobile and macOS clients, phone as an extra screen | later (the first version is Windows only) |
