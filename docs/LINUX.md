# Queqiao on Linux: design (draft 1, 2026-11-24)

The user, 2026-11-24: "design and invent the code for linux system ... Same function and feature as Windows", developed
on cc4-1 (Ubuntu 22.04). This document says how: what is shared with Windows, what each platform feature becomes on
Linux, the security choices, what can be tested where, and the order of the work.

## Principle: one Queqiao, two platform layers

The session logic (accounts, signalling, the transport, bitrate control, the protocol's commands, clipboard and file
transfers, help sessions, consent, history) is the same program on both systems. Only the layer that touches the
machine differs: capture, encode, decode, showing pictures, input, sound, the clipboard's formats, windows and the
tray, the service and the installer. A Windows viewer controls a Linux PC and the other way round, with the same
protocol; nothing in it is Windows-only today (scancodes are mapped on each side, see Input).

Two programs that only look alike would drift apart, so the work is a refactor, not a copy:

- Each platform-bound crate gets `windows/` and `linux/` modules behind one API (`#[cfg(windows)]`,
  `#[cfg(target_os = "linux")]`). The Windows code moves, it does not change.
- Pictures travel as a platform `Surface`: on Windows a Direct3D 11 texture (as now); on Linux a DMA-BUF (Wayland
  capture, VA-API), a CUDA buffer (NVENC/NVDEC), or memory (X11 capture, software codecs). The session code passes
  surfaces along and never looks inside; each encoder, decoder and presenter says which kinds it takes, and a
  converter fills the gap (GPU first, CPU as the fallback, as on Windows).
- The agent's and viewer's session loops move out of their `main.rs` into shared modules that call the platform API;
  `main.rs` keeps start-up and the platform's own pieces.
- Windows is checked after every step (sunshine: `check.ps1`, then the M4 sessions), so the refactor costs nothing
  there.

Already portable (no change, or paths only): `queqiao-server`, `queqiao-core` (protocol, STUN, invitations; paths
in `platform.rs`), `queqiao-account` (TLS, keys; DPAPI -> see Settings), `queqiao-transport` (str0m, QUIC, FEC,
flow; the interface list and one timer call are Windows), `queqiao-local`, `queqiao-update` (the feed, signatures),
`queqiao-files` (transfers; the clipboard's file list and compression flags are Windows), OpenH264 and Opus.

## Each feature on Linux

| Area | Windows | Linux |
|---|---|---|
| Screen capture | DXGI Desktop Duplication | X11: MIT-SHM (`xcb-shm`), damage from XDamage, pointer from XFixes. Wayland: the ScreenCast portal + PipeWire (DMA-BUF when offered, else memory; pointer as metadata). Without a session (sign-in screen, the service): KMS/DRM framebuffer capture (root) |
| Screens | DXGI outputs | XRandR monitors; Wayland: one portal stream per chosen monitor |
| Colour conversion | D3D11 video processor | NVENC takes RGB itself; VA-API's video processor (VPP); a Vulkan compute shader; CPU fallback |
| Encode | Media Foundation, NVENC, oneVPL, AMF, OpenH264 | NVENC on CUDA (same API as on Windows, CUDA device instead of D3D11), VA-API (Intel and AMD: H.264, H.265, AV1 where the GPU has them), OpenH264 |
| Decode | MF, NVDEC, oneVPL, AMF | NVDEC on CUDA, VA-API, OpenH264 (H.264), dav1d (AV1, software); H.265 in software is left out (patents: hardware only, as on Windows) |
| Showing pictures | D3D11 swap chain, video processor, NVDEC shader | Now: the CPU's BT.709 conversion and scaling (yuv.rs, as Windows' CPU path) into an MIT-SHM image. Next: the GPU (OpenGL or Vulkan) drawing the YUV planes with our shader; DMA-BUF imported without a copy where the driver can |
| Windows, forms, main window | Win32 and GDI | The session window: X11 through x11rb, a connection and window per session thread as on Windows (winit allows one event loop per process, which would not let sessions run on threads of their own); on Wayland through XWayland. Forms, the main window, prompts, the banner and the session bar: egui (eframe, OpenGL) on one GUI thread, as the agent's UI thread on Windows (same pages and texts as on Windows) |
| Tray | Shell notify icon | StatusNotifierItem (KDE, Ubuntu's GNOME with its AppIndicator extension); without one, the main window stays open instead |
| Notices | Toasts | org.freedesktop.Notifications |
| Input injection | SendInput | X11: XTest. Wayland: the RemoteDesktop portal (libei, GNOME 45+, KDE 6). The service and the sign-in screen: uinput (root) |
| Keys | Windows scancodes | The protocol keeps Windows scancodes; Linux maps them to and from evdev key codes (one table, tested both ways) |
| Ctrl+Alt+Del | The service's SAS | Has no meaning on Linux: the menu item is left out when that PC is Linux; lock and switch user are offered instead |
| Shortcuts to the remote (Super, Alt+Tab) | Low-level keyboard hook | X11: grab the keyboard while the window has focus. Wayland: the keyboard-shortcuts-inhibit protocol (GNOME, KDE, wlroots) |
| Game mouse | Raw input, clipped cursor | Relative pointer + pointer constraints (Wayland), XInput2 raw motion + grab (X11) |
| System sound | WASAPI loopback | PulseAudio / PipeWire: the default sink's monitor source |
| Sound out | WASAPI | PulseAudio / PipeWire |
| Mute that PC's speakers | Endpoint volume | The sound moved, not muted: a PulseAudio monitor follows its sink's mute (measured: muted, the capture got silence), so the streams (and what starts meanwhile) go to a null sink whose monitor is captured; moved back after |
| Microphone to the controlled PC | A virtual cable must be installed there | Queqiao makes its own: a null sink and a virtual source named "Queqiao microphone" for the session, removed after (no third-party driver) |
| Clipboard | Text, images, CF_HDROP | X11 selections / Wayland data-control (or the portal's clipboard): `text/plain;charset=utf-8`, `image/png`, `text/uri-list` (+ `x-special/gnome-copied-files`) |
| Files | Downloads\Queqiao | The XDG download folder + `/Queqiao` |
| Command lines | ConPTY + console window | A PTY with the person's login shell; on a Linux viewer the console is shown in the person's terminal program through a small bridge (as the Windows helper does) |
| Lock / restart / lock after a session | LockWorkStation, shutdown | logind over D-Bus (`LockSession`, `Reboot` through polkit) |
| Privacy screen, block local input | Magnification, low-level hooks | X11: a black override-redirect window is captured too, so the picture comes from before it (planned: capture under it with XComposite); KMS path: the monitor's plane. Local input: EVIOCGRAB on the input devices (the service). Wayland in the session: not possible without the compositor's help; said so in the menu |
| Service (sign-in, lock screens) | Windows service as SYSTEM + desktop helper | A systemd system service (root, hardened unit): KMS capture + uinput work on the greeter, the lock screen and the session alike; it starts the person's agent in each graphical session (logind) |
| Start at sign-in | Run key | XDG autostart entry (or the service, as on Windows) |
| Installer, uninstall | setup.exe | .deb (Ubuntu, Debian) first, .rpm next; the postinst sets up the service and the uinput rule; `apt remove` / `--purge` take everything out |
| Self-update | Signed manifest + installer | The same signed manifest (Ed25519) naming a .deb with its SHA-256; installed by the service (root), put back on failure |
| Settings secrets | DPAPI | Secret Service (GNOME Keyring, KWallet); without one, a key file readable by the person only |
| Wake-on-LAN check | powercfg, driver keys | ethtool (`SIOCETHTOOL`: WoL modes) and sysfs `power/wakeup` |
| Network interfaces | GetAdaptersAddresses | `getifaddrs` + netlink routes (uplinks, tunnels by the same rule: a tunnel holds no gateway of its own) |
| Crash reports | %APPDATA%\Queqiao\crashes | `$XDG_STATE_HOME/queqiao/crashes` |
| Languages | Windows display language | `LANG` / `LC_MESSAGES` (zh*, else English); `QUEQIAO_LANG` as on Windows |

## Security (as on Windows: security first)

- uinput lets whoever opens it type as anyone, so `/dev/uinput` is never opened up: only the root service holds it,
  and it takes input only from the session's agent over a local socket whose peer it checks (`SO_PEERCRED`).
- On Wayland, capture and input go through the portals: the person grants them once; the restore token is kept
  readable by them only. Unattended control of a Wayland session without that grant is the service's job (KMS +
  uinput), as the Windows service handles the secure desktop.
- The service's unit is hardened (`ProtectSystem=strict`, `NoNewPrivileges`, only the devices it needs); the agent it
  starts runs as the signed-in person, never as root.
- Everything else (accounts, pinning, consent, help codes, no files for helpers) is the shared code, unchanged.

## Where it is tested

cc4-1 is a container: Ubuntu 22.04, root, 4 x RTX 4090 with NVIDIA's encode/decode/CUDA libraries, no systemd, no
`/dev/uinput`, no sound card, no monitor. There: the build, the unit tests, X11 sessions on Xvfb (capture, XTest,
the X11 clipboard), Wayland on a headless compositor (sway + xdg-desktop-portal-wlr: the ScreenCast portal and
PipeWire), PulseAudio with null sinks, NVENC/NVDEC on the 4090s, Linux agent and viewer against each other, and the
.deb built and installed in a clean container. Not there (a real Linux PC is needed): GNOME's and KDE's own portals
and permission prompts, VA-API on Intel/AMD, uinput, the systemd service on a real greeter and lock screen, KMS
capture, real sound devices. Windows <-> Linux sessions need both on one network (or a reachable test server).

## Where it stands (2026-11-24)

Steps 1 to 3 are in for X11: the capture, input, encoders (OpenH264, NVENC on CUDA), the OpenH264 decoder, sound
(PulseAudio), the clipboard (X11 selections), files, command lines on the controlled side, and the agent's and
viewer's sessions, shared with Windows. `scripts/run_m2_linux.sh` runs a Linux agent against a Linux viewer (video,
input, clipboard, files, commands, sound with the speakers muted) and passes on cc4-1.

Step 5 so far: the GUI (egui on one GUI thread; every window a viewport): the tray icon (StatusNotifierItem, the menu
shared with Windows in tray.rs; without a tray the main window opens instead), the main window (Devices, Help,
Transfers, Settings), the session banner and the consent prompt, the help window, the login form, the device list,
the "help someone" form, the session's transfer list and password form, the small dialogs and menus, the session bar and card of figures on the viewer (child windows
of the session window, drawn on the CPU), and pictures of these windows for tests. The account mode is
shared with Windows (the agent's and the viewer's account.rs): login, online, the device list, help requests,
Wake-on-LAN (the cards from /sys/class/net; the check from sysfs and ethtool), start at sign-in (an XDG autostart
entry), one agent per user (a lock in a private runtime folder; opened again, it shows the running one's window).
`scripts/run_m3_linux.sh` runs both with a queqiao-server of their own on 127.0.0.1: direct (with the clipboard),
allowed and declined (the consent prompt), and ui (the viewer through its login form and device list, with pictures),
and passes on cc4-1. Updates come with the system's packages (step 6), not from Queqiao.

Not yet on Linux: command-line
windows on the viewer side, NVDEC and VA-API, Wayland, the privacy screen and input block, the service and packages.

## Order of the work

1. Foundation: the workspace builds on Linux (Windows parts behind `cfg`), the portable crates' tests pass there,
   `scripts/check.sh` (fmt, clippy, tests) beside `check.ps1`; Windows unchanged.
2. The platform API (Surface, capture, encode, decode, present, input, sound, clipboard) with the Windows
   implementation moved behind it; the session loops shared. Windows M4 must pass unchanged.
3. Linux agent on X11: capture, OpenH264 + NVENC, XTest, sound, clipboard, files; Linux viewer: window, decode
   (OpenH264, NVDEC), input, sound, clipboard, file drop. `scripts/run_m4_local.sh` on cc4-1.
4. Wayland (portal capture, libei input), VA-API, dav1d, H.265 / 4:4:4 / 10-bit where the hardware has them.
5. The main window, forms, tray, settings, transfers, languages (egui), the session bar; then accounts (the agent's
   and viewer's account modes shared as the sessions are). Next, before step 4.
6. The service (greeter, lock screen: KMS + uinput), autostart, .deb, self-update, power, Wake-on-LAN check,
   command lines, privacy screen and input block where Linux allows them, crash reports.
7. Windows <-> Linux sessions both ways, and a Linux column in PARITY.md, on real Linux PCs.
