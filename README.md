<p align="center">
  <img src="docs/images/logo.png" alt="" width="120">
</p>

<h1 align="center">Queqiao</h1>

<p align="center">
  <b>Remote control for your own computers, from your computer or your phone.</b><br>
  Log in, see your devices, and control any of them from another, directly or through your own server.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platforms-Windows%20%7C%20Linux%20%7C%20Android%20%7C%20iOS-3D5FAE" alt="Platforms: Windows, Linux, Android, iOS">
  <img src="https://img.shields.io/badge/written%20in-Rust-E3B45A?logo=rust&logoColor=white" alt="Written in Rust">
  <img src="https://img.shields.io/badge/languages-English%20%7C%20%E7%AE%80%E4%BD%93%E4%B8%AD%E6%96%87-2FBF71" alt="English and Simplified Chinese">
  <img src="https://img.shields.io/badge/license-all%20rights%20reserved-141A3B" alt="License: all rights reserved">
</p>

<p align="center">
  <img src="docs/images/main-window.png" alt="Queqiao's main window: the account's devices, one of them being controlled by another device" width="760">
</p>

<p align="center"><i>Named after the magpie bridge (鹊桥) of the Qixi legend, which joins two people across the Milky Way.</i></p>

---

## Contents

- [Features](#features)
- [Installation & Usage](#installation--usage)
  - [Install](#install)
  - [Getting started](#getting-started)
  - [Run your own server](#run-your-own-server)
  - [Build from source](#build-from-source)
- [Compatibility](#compatibility)
- [Questions & Feedback](#questions--feedback)
- [License](#license)

---

## Features

<table>
  <tr>
    <td width="33%" valign="top">
      <h4>🔗 Connect anywhere</h4>
      Direct peer-to-peer connections across networks (NAT traversal, IPv4 and IPv6), and a relay through your server when no direct path exists.
    </td>
    <td width="33%" valign="top">
      <h4>🎞️ Sharp, fast picture</h4>
      Hardware encoding on NVIDIA, Intel and AMD GPUs: H.264, H.265 (with 4:4:4 colour and 10-bit) and AV1, with adaptive bitrate. A software fallback runs on any PC.
    </td>
    <td width="33%" valign="top">
      <h4>👥 One controller at a time</h4>
      When another of your devices is in control, Queqiao says so. You can disconnect it, or take control after confirming.
    </td>
  </tr>
  <tr>
    <td valign="top">
      <h4>📁 Files and clipboard</h4>
      A two-pane file transfer window, drag and drop, and text, images and files through the clipboard, both ways.
    </td>
    <td valign="top">
      <h4>🤝 Help someone</h4>
      Help a person on another account with their help ID and code. They see who is helping and can end it at any time.
    </td>
    <td valign="top">
      <h4>🔒 Security first</h4>
      Signed signalling between device keys, a pinned server certificate, consent on the controlled computer, a privacy screen and encrypted saved logins.
    </td>
  </tr>
</table>

### Everything it does

- **Accounts and devices:** one login form for signing in and signing up, the account's devices with online state, device names and favourites, and Wake-on-LAN through another device on the same network.
- **Sessions:** several at once, a floating session bar, a control center for every session setting, and a card of live figures (path, delay, frame rate, data rate).
- **Picture:** quality, frame rate, resolution and bitrate settings, several screens, full screen, and a privacy screen that darkens the controlled computer and turns off its keyboard and mouse.
- **Sound:** the controlled computer's sound (Opus), with its speakers muted while you listen.
- **Input:** mouse and keyboard by scancode, Windows shortcuts (Win, Alt+Tab, Ctrl+Alt+Del with the service), game mouse with relative movement, the remote pointer's shape, your own hotkeys and a boss key.
- **Tools:** remote command lines in a terminal window (detach, show again, rename), and lock and restart.
- **The controlled computer's person decides:** whether your devices may control it, whether to ask first, clipboard, files, command lines, starting with the system, locking after a session, and a password for these settings.
- **Sign-in screen access:** an optional service shows the sign-in, lock, Ctrl+Alt+Del and UAC screens.
- **Phones:** Android and iOS apps to control your computers, with touch and mouse modes, the phone's keyboard in any language, a key bar, several screens and the computer's sound.
- **Updates:** signed updates on Windows, installed between sessions and rolled back if the new version does not start. On Linux, a new package installs over the old one.
- **Languages:** English and Simplified Chinese, following the system or chosen in Settings.

The full list, compared function by function with UU Remote: [docs/PARITY.md](docs/PARITY.md).

---

## Installation & Usage

### Install

| Platform | Package | How to install |
|---|---|---|
| **Windows** | `queqiao-setup-0.1.0.exe` | Run it. It installs for you without an administrator, and the optional service needs one UAC prompt. Quiet install: `queqiao-setup-0.1.0.exe --quiet --autostart [--service]`. |
| **Ubuntu, Debian** | `queqiao_0.1.0_amd64.deb` | `sudo apt install ./queqiao_0.1.0_amd64.deb` |
| **Fedora, openSUSE Tumbleweed** | `queqiao-0.1.0-1.x86_64.rpm` | `sudo dnf install ./queqiao-0.1.0-1.x86_64.rpm` (openSUSE: `sudo zypper install ./queqiao-0.1.0-1.x86_64.rpm`) |
| **Android** | `queqiao.apk` | Open it on the phone and allow installing from this source. |
| **iOS** | `Queqiao.ipa` | Install it with [Sideloadly](https://sideloadly.io) and your Apple ID. Apple asks for it to be signed again every 7 days. |

> [!NOTE]
> Prebuilt packages are not published on the Releases page yet. Until they are, build them as described in [Build from source](#build-from-source). The Windows programs are currently signed with a test certificate, so Windows SmartScreen may warn before the first start.

### Getting started

<table>
  <tr>
    <td width="300" valign="top"><img src="docs/images/login.png" alt="The login window" width="280"></td>
    <td valign="top">

1. **Log in.** Open Queqiao and enter a user name and password. If the user name is new, the account is created and you are signed in. If it already exists, the password is checked.
2. **Add your computers.** Install Queqiao on each computer and log in with the same account. Each one appears in **My devices**.
3. **Connect.** Choose a device and press **Connect**. If another of your devices is controlling it, you are asked whether to take control, and the other session ends only if you confirm.
4. **Use the session.** The session bar at the top opens the **control center** (picture, sound, screens, input, tools), the **file transfer** window, full screen and **End**.
5. **Help someone.** They choose *Let someone help me* and read you their help ID and code. You enter them under **Help someone**.

</td>
  </tr>
</table>

Sessions on a phone work the same way. Choose a device and tap **Connect**. Tap or drag to move the pointer, or switch to mouse mode. Use the floating ball for the keyboard, keys and tools.

### Run your own server

Queqiao's server holds the accounts, shows each account its devices, passes the signals that set up a session, and
relays a session when no direct path exists. It runs well on a small cloud machine.

```sh
queqiao-server init --name <public address>      # makes the certificate; prints its fingerprint for the clients
queqiao-server add-user <name>                   # optional: an account (the password on standard input)
queqiao-server serve --name <public address> --public-ip <public address> --allow-signup
```

Open **TCP 47100** (accounts), **UDP 3478** (STUN/TURN) and **UDP 47200–47399** (relayed sessions). Without
`--allow-signup`, only accounts made with `add-user` can log in. With it, a new user name in the login window makes
an account, at most 3 an hour from one address. Clients reach the server by its address and the certificate
fingerprint, which the packages can carry built in (see [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)).

### Build from source

```sh
git clone https://github.com/GuoWQ222/Queqiao.git
cd Queqiao
cargo build --release --workspace
```

Windows needs Visual Studio Build Tools (C++ workload, CMake) and NASM. Linux needs clang, CMake, NASM and the X11,
Wayland, PipeWire and PulseAudio development packages. The installers come from `scripts\package.ps1` (Windows) and
`scripts/package_linux.sh` (Linux). The repository layout, checks and tests are in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

---

## Compatibility

### Platforms

| Platform | Versions | Control other devices | Be controlled |
|---|---|:---:|:---:|
| **Windows** | Windows 10 (1809 or later), Windows 11; x64 | ✅ | ✅ including the sign-in screen, with the service |
| **Linux** | glibc 2.35 or later (Ubuntu 22.04+, Debian 12+, Fedora 36+); x86_64 | ✅ | ✅ on X11 and Wayland, including the sign-in screen, with the service |
| **Android** | Android 8.0 or later | ✅ | — planned |
| **iOS / iPadOS** | iOS 15 or later | ✅ | — (iOS allows no app to control the device) |

### Linux desktops

| Session | Screen and input |
|---|---|
| X11 (any desktop) | ✅ MIT-SHM capture, XTest input |
| Wayland: GNOME | ✅ through the desktop portal. The person allows it once; the permission is remembered |
| Wayland: wlroots (Sway and others) | ✅ screen through the desktop portal, input through the compositor's virtual pointer and keyboard |
| Wayland: KDE Plasma | ⚠️ through the desktop portal: input tested, picture still being tested |

### Graphics and codecs

Queqiao finds what each computer has and falls back step by step, so every stage also works without a GPU.

| | Encoding (controlled side) | Decoding (controlling side) |
|---|---|---|
| **NVIDIA** | NVENC: H.264, H.265 (4:2:0 / 4:4:4, 8 / 10-bit), AV1 | NVDEC |
| **Intel** | Quick Sync (oneVPL): H.264, H.265 | oneVPL |
| **AMD** | AMF: H.264, H.265 | AMF |
| **Any other** | Media Foundation (Windows), OpenH264 (software) | Media Foundation, OpenH264 |
| **Phones** | — | The phone's own hardware decoder (MediaCodec, VideoToolbox) |

The codec is chosen automatically, using the best one both ends support, or set by hand in the session's control center.
On Linux, hardware encoding and decoding currently use NVIDIA GPUs, and other GPUs use the software path.

### Network

Direct connections work across most home and office networks, over IPv4 or IPv6. When no direct path exists, for
example behind a phone's hotspot, the session is relayed through your server. Proxies and VPN adapters on the
computer are bypassed for the server connection.

---

## Questions & Feedback

- **Questions, problems and ideas:** open an [issue](https://github.com/GuoWQ222/Queqiao/issues). For a problem, include what you did, what happened, your system and Queqiao's version (`queqiao-agent --version`).
- **Security problems:** please don't post the details in a public issue. Open an issue asking for a private way to send them.

---

## License

Copyright © 2026 GuoWQ222. All rights reserved. No part of Queqiao may be copied, modified, distributed or used
without the copyright holder's written permission: see [LICENSE](LICENSE).

The third-party parts (a patched sctp-proto, bindings generated from NVIDIA's, Intel's and AMD's MIT-licensed
headers, and the crates compiled in) keep their own licences: see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
