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
| Screen capture | DXGI Desktop Duplication | X11: MIT-SHM (`xcb-shm`), damage from XDamage, pointer from XFixes. Wayland: the ScreenCast portal + PipeWire (memory; the pointer as metadata where the portal offers it, else in the pictures). Without a session (sign-in screen, the service): KMS/DRM framebuffer capture (root) |
| Screens | DXGI outputs | XRandR monitors; Wayland: one portal stream per chosen monitor |
| Colour conversion | D3D11 video processor | NVENC takes RGB itself; VA-API's video processor (VPP); a Vulkan compute shader; CPU fallback |
| Encode | Media Foundation, NVENC, oneVPL, AMF, OpenH264 | NVENC on CUDA (same API as on Windows, CUDA device instead of D3D11), VA-API (Intel and AMD: H.264, H.265, AV1 where the GPU has them), OpenH264 |
| Decode | MF, NVDEC, oneVPL, AMF | NVDEC on CUDA, VA-API, OpenH264 (H.264), dav1d (AV1, software); H.265 in software is left out (patents: hardware only, as on Windows) |
| Showing pictures | D3D11 swap chain, video processor, NVDEC shader | Now: the CPU's BT.709 conversion and scaling (yuv.rs, as Windows' CPU path) into an MIT-SHM image. Next: the GPU (OpenGL or Vulkan) drawing the YUV planes with our shader; DMA-BUF imported without a copy where the driver can |
| Windows, forms, main window | Win32 and GDI | The session window: X11 through x11rb, a connection and window per session thread as on Windows (winit allows one event loop per process, which would not let sessions run on threads of their own); on Wayland through XWayland. Forms, the main window, prompts, the banner and the session bar: egui (eframe, OpenGL) on one GUI thread, as the agent's UI thread on Windows (same pages and texts as on Windows) |
| Tray | Shell notify icon | StatusNotifierItem (KDE, Ubuntu's GNOME with its AppIndicator extension); without one, the main window stays open instead |
| Notices | Toasts | org.freedesktop.Notifications |
| Input injection | SendInput | X11: XTest. Wayland: the compositor's virtual pointer and keyboard (wlroots), else the RemoteDesktop portal (GNOME, KDE: its notify calls, in the session that also gives the picture). The service and the sign-in screen: uinput (root) |
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
| Service (sign-in, lock screens) | Windows service as SYSTEM + desktop helper | A systemd service (root): for the session on seat0 (logind) it starts the person's agent (as them, with their session's display and bus) or, at the sign-in screen, the agent for it (system user `queqiao`), and a desktop helper (root, reduced) whose KMS capture and uinput work on the sign-in and lock screens; never on the person's own desktop, which goes through the portals and their consent |
| Start at sign-in | Run key | XDG autostart entry (or the service, as on Windows) |
| Installer, uninstall | setup.exe | A .deb (Ubuntu, Debian) and an .rpm (Fedora, openSUSE, RHEL) from one build (scripts/package_linux.sh): the system user, folders and uinput module, the service enabled and started; `apt remove --purge` / `rpm -e` take everything out |
| Self-update | Signed manifest + installer | The same signed manifest (Ed25519, the same key) naming the .deb or .rpm with its SHA-256 (queqiao-update-deb.json, -rpm.json); downloaded by the person's agent, checked again and installed by the service (root) in a systemd unit of its own, the earlier package put back if the new one does not keep running |
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
- The service reduces what it starts itself, not through its unit: systemd's sandboxing options would bind every process
  of the unit, the person's agent too (a command line from a viewer runs their programs, sudo included). The desktop
  helper keeps only CAP_SYS_ADMIN (a framebuffer's buffer) and CAP_DAC_OVERRIDE (another user's display, for the notice)
  in its bounding set, with no new privileges; the agent for the sign-in screen runs as the system user `queqiao`
  with no new privileges; the person's agent runs as them, never as root, closed to their other programs
  (PR_SET_DUMPABLE 0, as the agent at High integrity on Windows). Each inherits only its channel's pipes and an
  environment made by the service (the display, bus and locale names from a fixed list, read from the session's own
  programs).
- The helper never captures the person's own unlocked desktop (KMS would go round GNOME's consent dialog): only the
  sign-in and lock screens, during a session. Input it injects only there too.
- On screens where no notice can be shown (GNOME's sign-in and lock screens show no other program), the session goes
  on and the person is told afterwards: the person's choice (2026-11-25), within the same limits as on Windows (the
  account's own devices only, after that PC's password; helpers from other accounts never get these screens).
- Everything else (accounts, pinning, consent, help codes, no files for helpers) is the shared code, unchanged.

## Where it is tested

cc4-1 is a container: Ubuntu 22.04, root, 4 x RTX 4090 with NVIDIA's encode/decode/CUDA libraries, no systemd, no
`/dev/uinput`, no sound card, no monitor. There: the build, the unit tests, X11 sessions on Xvfb (capture, XTest,
the X11 clipboard), Wayland on headless compositors (sway + xdg-desktop-portal-wlr: the ScreenCast portal and
PipeWire; GNOME's mutter + xdg-desktop-portal-gnome: the RemoteDesktop portal and its dialog; the same with GNOME 46
in an Ubuntu 24.04 root entered with proot, scripts/noble_root_linux.sh; KDE's KWin 5.27 + xdg-desktop-portal-kde in
that root: the portal session and input, not its pictures), PulseAudio with null sinks, NVENC/NVDEC on the 4090s, Linux
agent and viewer against each other. In a virtual machine on cc4-1 (scripts/vm_linux.sh: QEMU, Ubuntu 24.04 with GNOME
and GDM): systemd and logind, GNOME Shell, the service on the real sign-in and lock screens, uinput, KMS capture
(virtio-gpu), the .deb and .rpm installed and removed, updates. Not there (a real Linux PC is needed): KDE's screen-cast
pictures (KWin's virtual backend records none), VA-API on Intel/AMD, a real GPU's tiled framebuffers (cc4-1's NVIDIA
driver exports none: modeset off), real sound devices. Windows <-> Linux sessions need both on one network (or a reachable test server).

## Where it stands (2026-11-24)

Steps 1 to 3 are in for X11: the capture, input, encoders (OpenH264, NVENC on CUDA), the OpenH264 decoder, sound
(PulseAudio), the clipboard (X11 selections), files, command lines on the controlled side, and the agent's and
viewer's sessions, shared with Windows. `scripts/run_m2_linux.sh` runs a Linux agent against a Linux viewer (video,
input, clipboard, files, commands, sound with the speakers muted) and passes on cc4-1.

Step 5 so far: the GUI (egui on one GUI thread; every window a viewport): the tray icon (StatusNotifierItem, the menu
shared with Windows in tray.rs; without a tray the main window opens instead), the main window (Devices, Help,
Transfers, Settings), the session banner and the consent prompt, the help window, the login form, the device list,
the "help someone" form, the session's password form, the small dialogs and menus, the session bar and card of figures on the viewer (child windows
of the session window, drawn on the CPU), and pictures of these windows for tests. The session's control center (the bar's
"Control center", 2026-11-25) and file transfer window (UU Remote's two panes and transfer list) are windows of the GUI
thread too, with the Windows windows' contents (panel.rs, model.rs): the control center opens at the session window's
top right (where the compositor places windows itself, as on Wayland, wherever it puts it) and closes on a choice, Esc
or a click elsewhere. The account mode is
shared with Windows (the agent's and the viewer's account.rs): login, online, the device list, help requests,
Wake-on-LAN (the cards from /sys/class/net; the check from sysfs and ethtool), start at sign-in (an XDG autostart
entry), one agent per user (a lock in a private runtime folder; opened again, it shows the running one's window).
`scripts/run_m3_linux.sh` runs both with a queqiao-server of their own on 127.0.0.1: direct (with the clipboard),
allowed and declined (the consent prompt; the banner and prompt pictured), ui (the viewer through its login form and
device list, with pictures), main (the agent's main window, each page pictured), and the transfer list and password
form (examples/linux_forms_probe), and passes on cc4-1. Updates come with the system's packages (step 6), not from Queqiao.

Command lines on the viewer side open in the desktop's terminal emulator (x-terminal-emulator, else GNOME Terminal,
Konsole, Xfce's, xterm) running the viewer's helper over a Unix socket in the private runtime folder (this user only,
with a one-time token); --terminal-test passes in run_m3_linux.sh.

The viewer decodes on NVIDIA GPUs with NVDEC (CUDA): H.264, and H.265 4:2:0 and 4:4:4 at 8 and 10 bits as the GPU
decodes them, each picture copied into memory as NVDEC lays it out (NV12, P016, three planes for 4:4:4) and drawn by
the CPU converter Windows uses when its GPU cannot (yuv.rs); else OpenH264. The agent's NVENC (CUDA, fed from memory)
makes H.265 at 8 and 10 bits: 4:2:0 10-bit from the CPU converter's P010 (BT.709 limited range in 10 bits, 64-940 /
64-960, the same bilinear scaling as NV12), 4:4:4 10-bit from the screen's pixels widened to R10G10B10A2 (v << 2 | v >> 6,
red in the lowest bits: NVENC's ABGR10), which NVENC turns into YUV itself, as at 8 bits. run_m3_linux.sh's h265,
h265_10 and h265_444 sessions have NVENC make each and NVDEC show it, and check colours: five windows (red, green,
blue, grey, orange) on the agent's screen against the viewer's picture (at most 12 apart a channel; 8-bit 4:2:0 shows
1, the 10-bit ones 0; red and blue swapped in the 4:4:4 packing fails it with 255). AV1 (docs/DESIGN.md, item 9): the
agent makes it with NVENC API 12 where the driver has it (8-bit 4:2:0), the viewer decodes it with NVDEC where the GPU
does, and the viewer's default (Automatic) gets AV1 when both can, else H.265 when both can, else H.264; run_m3_linux.sh's av1 session checks
it (colours too), av1_api11 the agent on API 11.1 (an older driver): H.264.

Wayland (2026-11-25): the agent captures through the ScreenCast portal (xdg-desktop-portal, which GNOME, KDE and the
wlroots desktops answer) and its PipeWire stream (libpipewire loaded at run time, as CUDA and PulseAudio are; only its
stable C API, the format pods written by hand: it builds without PipeWire's headers and runs with any PipeWire 0.3 or
later), and injects input through the wlroots compositors' virtual pointer and virtual keyboard (the keymap the seat
uses, else libxkbcommon's default). Where the compositor has no virtual devices (GNOME, KDE), the agent opens one
RemoteDesktop portal session for both, the screen and the keyboard and pointer (crate queqiao-portal; the person
answers one dialog, which must allow remote interaction), and injects through its notify calls. GNOME adds that
session's pointer and keyboard to the seat only with their first event, and on a PC without a keyboard or mouse of its
own (a server, a virtual machine) programs learn of them after it: the first key would reach none. So the agent brings
them in when the session opens: a move by nothing, and KEY_UNKNOWN pressed (no symbol in the keymaps: nothing acts on
it). On wlroots, xdg-desktop-portal-wlr (0.5, Ubuntu 22.04) asks for every frame "with damage", the first too, and
may give the first change to a capture of an earlier session: on a still screen a session got no picture and ended
after 5 s. The capture now waits only for the format PipeWire agrees on, and until its first picture commits a 1 x 1
fully transparent overlay surface (the layer shell; no input) every 250 ms: the screen counts as changed, nothing
shows (wayland_nudge.rs; the sway test checks it with two captures of a still screen in a row). The session's first
picture (its snapshot, taken right after the capture opens) waits up to 5 s with the same pokes: KWin also sends none
for a still screen. The GUI runs natively
on Wayland (its hidden root window is a 4 x 4 transparent window there: Wayland hides no
window; mutter refuses smaller); the session window runs through XWayland and shows its last picture again where it
was uncovered or resized (the X server keeps none, and a still remote screen sends no new one: the session bar left a
black strip after tucking away). Tested on cc4-1 on virtual Wayland displays (the user, 2026-11-25: passing on cc4-1's
virtual screens is enough): scripts/wayland_display_linux.sh starts, as the user qqtest, headless sway (DESKTOP=sway,
the default) or GNOME's mutter (DESKTOP=gnome, with a system bus) with XWayland, PipeWire, WirePlumber and the portals;
scripts/run_wayland_linux.sh runs a session against it: the viewer's picture against the Wayland screen (grim on
sway; on GNOME the test window's white), and the input test (five pointer places, a click, F24) as a native Wayland
window sees it: wev on sway, scripts/wayland_test_window.py (GTK 4, its Wayland protocol log for keys GTK drops) on
GNOME, where scripts/portal_approve.py answers GNOME's dialog through AT-SPI as the person would (only the tests do;
Queqiao never goes round it). The person's answer is kept where the portal can keep it (RemoteDesktop 2,
xdg-desktop-portal 1.17 or later, and a desktop backend that keeps it): the agent asks for that ("until revoked"),
keeps the restore token for the user only (0600) and offers it the next time, and the desktop starts the session
without asking until the person revokes it in its settings; elsewhere the desktop asks each session (Ubuntu 22.04:
1.14). Tested with GNOME 46 (Ubuntu 24.04, its dialog's "Remember This Selection" ticked by default): the first session
asks, the next starts with no dialog, picture and input as in the first (run_wayland_linux.sh NOBLE=1, then KEPT=1).
That root (Ubuntu 24.04 on cc4-1, a container without mount rights or user namespaces) runs under proot 5.5 (22.04's
5.1 does not know statx), its programs as the test user from the start (proot cannot follow a change of user), with a
system bus that keeps its user, and a library in PipeWire alone answering its sandbox check (/proc/<pid>/root, which
proot cannot open) with "permission denied", which PipeWire takes for "not sandboxed": all for the test bench only.
KDE (Plasma 5.27, Ubuntu 24.04, in the same root; run_wayland_linux.sh DESKTOP=kde NOBLE=1): the RemoteDesktop portal
session opens and input reaches a window (five places, a click, F24). Plasma 5.27 shows no dialog to a program that is
not sandboxed and keeps no answer (KDE bug 479013; Plasma 6 asks, and 6.3 can pre-authorize): Queqiao's own consent
(the account's devices allowed, "ask before a session") and its banner are what the person sees there. KWin's virtual
backend records no frames for screen casts (its buffers are made, the stream runs, nothing comes whatever changes on
screen), so KDE's pictures are untested here; the session's own check (the probe) stands in for the agent there.
The KEY_UNKNOWN press that brings in the keyboard reaches the focused program on KDE (it has a keyboard already):
nothing acts on it.

The pointer on Wayland, as on Windows and X11 where the portal allows it: the agent asks for the pointer as metadata
(GNOME, KDE; xdg-desktop-portal-wlr 0.5 offers only the pointer in the pictures, where it stays). The pictures then
have no pointer; the stream's buffers carry its bitmap and hot spot (spa_meta_cursor), sent to the viewer as the
pointer's shape, which draws it at its own mouse, and a bitmap of no size hides it. The screen's side offers room for
its bitmap at one size (mutter: 384 x 384) that must fall in the range asked for (up to 1024 x 1024), or no pointer meta
comes at all; shapes over 256 x 256 are passed over. A buffer that carries only the pointer is no frame. Tested: GNOME
42 and 46 send the shapes (the test checks at least one), GNOME 42 with a cursor theme sends the arrow (cc4-1 had none:
mutter then sends a grey square), KDE gives the pointer apart (no frames there, so no shapes). Not yet on Wayland:
KDE's pictures (a real KDE desktop), the banner at the top (Wayland programs cannot place themselves: it floats where
the compositor puts it).

The Queqiao service (step 6, 2026-11-25), as on Windows: `queqiao-service run` under systemd follows the session on
seat0 as logind gives it and keeps a desktop helper and an agent running for it, with private pipes between them
(crates/queqiao-service/src/linux). A person's desktop gets their agent, as them, with the display, bus and locale its
own programs have (the service reads those names from them); the sign-in screen (logind's greeter) gets the agent for
it, as the system user `queqiao`, with the device the person handed over (only while they let the account's devices in
without asking; only they may take it back). The helper says which screen takes input (logind: the person's own, its
lock screen by LockedHint, the sign-in screen), sends pictures of the sign-in and lock screens as the display hardware
shows them (KMS: the framebuffer each lit CRTC scans out, with the pointer's plane drawn in; a linear one read where it
lies, one in a GPU's tiled layout imported by the GPU through EGL with its modifier and drawn into a plain texture) and
types and points there through uinput. It wakes a screen the desktop switched
off. A session that starts on the lock screen goes to the helper at once, and back to the agent's own capture when the
person's desktop is unlocked. The lock gate asks that PC's password as on Windows: PAM checks it (`/etc/pam.d/queqiao`,
the system's common checks), by the service for the account it decides (the person signed in, or the one who set up
the sign-in screen), at most 10 wrong ones in 15 minutes for all agents together. On screens where no notice can be
shown, a notice afterwards: the person's agent when the session is unlocked or ends; at the sign-in screen, the service
keeps it for whoever signs in next. Where an X11 screen lets it, the helper shows the session banner there.

Packages and updates: scripts/package_linux.sh makes the .deb and the .rpm (packaging/linux: the unit, the system user,
folders, uinput, PAM, desktop entries, icons from assets/queqiao.ico). An installed Queqiao updates through the service:
the person's agent finds the manifest for its kind of package on the feed, downloads and checks it, and hands it to the
service, which checks it again (the release key, size and SHA-256, a later version, the system's kind of package and
Queqiao's name), installs it from a systemd unit of its own (installing restarts the service) and puts back the package
an earlier update installed if the new service or its agent does not keep running. `queqiao-setup release-package`
signs a package's manifest from its name, size and SHA-256 where the release key is.

Tested in a virtual machine on cc4-1 (scripts/vm_linux.sh: Ubuntu 24.04, GNOME and GDM on Wayland, systemd, uinput,
virtio-gpu's KMS; QEMU without KVM) by scripts/run_service_vm_linux.sh, with the server and viewer on cc4-1: the .deb
installs and the service starts qq's agent, which hands the device over (install); qq's session locked, the viewer gives
the password (a wrong one refused), sees the lock screen through the helper and unlocks it by typing, and qq is told
(lock); qq signed out, the agent for the sign-in screen serves, the viewer gives qq's password, sees GDM and signs qq in
by typing, and qq's agent is handed the notice (signin); 0.1.0 updates itself to 0.1.1 from a signed feed (update);
0.1.2, broken on purpose, is put back to 0.1.1 (rollback); the .rpm installs and starts under the same systemd and
leaves nothing after `rpm -e` (rpm); `apt remove --purge` leaves nothing (purge). scripts/rpm_deps_linux.sh installs
the .rpm in Fedora 43 with dnf (its dependencies from Fedora's repositories), runs the programs there and removes it.
The GPU's reading was tested on the machine's own framebuffers, forced through EGL (kms_probe <folder> gpu: Mesa there,
the lock screen read right with its pointer). Not tested: framebuffers in a real GPU's tiled layout (no such machine:
cc4-1's NVIDIA driver has nvidia-drm's modeset off, so it exports nothing; `kms_probe gpu-self-test` checks it where
it does), GDM or LightDM on X11 (the banner there), several screens at the sign-in screen (the pointer's range covers all
of them).

Not yet on Linux: VA-API, the privacy screen and input block.

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
