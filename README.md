<p align="center">
  <img src="docs/images/logo.png" alt="" width="120">
</p>

<h1 align="center">Queqiao</h1>

<p align="center"><strong>English</strong> · <a href="README.zh-CN.md">简体中文</a></p>

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
- [Getting started](#getting-started)
- [Compatibility](#compatibility)
- [Questions & Feedback](#questions--feedback)
- [License](#license)

---

## Features

<table>
  <tr>
    <td width="33%" valign="top">
      <h4>🔗 Connect anywhere</h4>
      Direct peer-to-peer connections across networks (NAT traversal, IPv4 and IPv6). When a direct connection is unavailable, the session uses a relay server (50–100 ms latency).
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

---

## Getting started

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

---

## Compatibility

### Platforms

| Platform | Versions | Control other devices | Be controlled |
|---|---|:---:|:---:|
| **Windows** | Windows 10 (1809 or later), Windows 11; x64 | ✅ | ✅ including the sign-in screen, with the service |
| **Linux** | glibc 2.35 or later (Ubuntu 22.04+, Debian 12+, Fedora 36+); x86_64 | ✅ | ✅ on X11 and Wayland, including the sign-in screen, with the service |
| **Android** | Android 8.0 or later | Planned | Not supported |
| **iOS / iPadOS** | iOS 15 or later | Planned | — (iOS allows no app to control the device) |

### Linux desktops

| Session | Screen and input |
|---|---|
| X11 (any desktop) | ✅ MIT-SHM capture, XTest input |
| Wayland: GNOME | ✅ through the desktop portal. The person allows it once; the permission is remembered |
| Wayland: wlroots (Sway and others) | ✅ screen through the desktop portal, input through the compositor's virtual pointer and keyboard |
| Wayland: KDE Plasma | ⚠️ Unstable, untested |

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
