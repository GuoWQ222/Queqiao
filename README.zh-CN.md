<p align="center">
  <img src="docs/images/logo.png" alt="" width="120">
</p>

<h1 align="center">Queqiao</h1>

<p align="center"><a href="README.md">English</a> · <strong>简体中文</strong></p>

<p align="center">
  <b>用电脑或手机，远程控制自己的电脑。</b><br>
  登录后即可查看自己的设备，选择一台发起连接。设备优先直连，也支持中转服务器中转。
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platforms-Windows%20%7C%20Linux%20%7C%20Android%20%7C%20iOS-3D5FAE" alt="支持 Windows、Linux、Android 和 iOS">
  <img src="https://img.shields.io/badge/written%20in-Rust-E3B45A?logo=rust&logoColor=white" alt="使用 Rust 开发">
  <img src="https://img.shields.io/badge/languages-English%20%7C%20%E7%AE%80%E4%BD%93%E4%B8%AD%E6%96%87-2FBF71" alt="支持英文和简体中文">
  <img src="https://img.shields.io/badge/license-all%20rights%20reserved-141A3B" alt="保留所有权利">
</p>

<p align="center">
  <img src="docs/images/main-window.png" alt="Queqiao 主窗口左侧列出账号下的设备，右侧显示所选电脑正由另一台设备控制" width="760">
</p>

<p align="center"><i>Queqiao 的名字来自七夕传说中的鹊桥。喜鹊在银河上架起桥，让牛郎和织女得以相会。</i></p>

---

## 目录

- [主要功能](#主要功能)
- [快速上手](#快速上手)
- [兼容性](#兼容性)
- [问题与反馈](#问题与反馈)
- [许可](#许可)

---

## 主要功能

<table>
  <tr>
    <td width="33%" valign="top">
      <h4>🔗 跨网络连接</h4>
      通过 NAT 穿透建立点对点连接，支持 IPv4 和 IPv6。无法直连时，通过服务器中转（延迟50-100ms）。
    </td>
    <td width="33%" valign="top">
      <h4>🎞️ 高清低延迟画面</h4>
      可用 NVIDIA、Intel 和 AMD 显卡进行硬件编码。支持 H.264、H.265（含 4:4:4 色度采样和 10 位色深）、AV1 及自适应码率。没有可用的硬件编码器时，可使用软件编码。
    </td>
    <td width="33%" valign="top">
      <h4>👥 会话接管</h4>
      同一时间只有一台设备可以控制目标电脑。如果你的另一台设备正在控制它，Queqiao 会显示提示。你可以断开现有连接，也可以确认后接管。
    </td>
  </tr>
  <tr>
    <td valign="top">
      <h4>📁 文件与剪贴板</h4>
      在双栏文件传输窗口中拖放文件，也可以通过剪贴板双向传送文字、图片和文件。
    </td>
    <td valign="top">
      <h4>🤝 远程协助</h4>
      使用对方提供的帮助 ID 和验证码，即可跨账号发起协助。对方可以看到谁在协助，也可以随时结束会话。
    </td>
    <td valign="top">
      <h4>🔒 连接安全</h4>
      设备用各自的密钥为连接信令签名，并校验预先指定的服务器证书。被控端可以要求连接前征得同意，也可以启用隐私屏。保存的登录信息会加密存储。
    </td>
  </tr>
</table>

---

## 快速上手

<table>
  <tr>
    <td width="300" valign="top"><img src="docs/images/login.png" alt="Queqiao 登录窗口" width="280"></td>
    <td valign="top">

1. **登录。** 打开 Queqiao，输入用户名和密码。如果用户名尚未注册，系统会创建账号并登录。如果账号已存在，系统会验证密码。
2. **添加电脑。** 在每台电脑上安装 Queqiao，用同一账号登录。它们会出现在「我的设备」中。
3. **发起连接。** 选择目标设备，点击「连接」。如果你的另一台设备正在控制它，系统会询问是否接管，确认后才会结束原会话。
4. **使用工具栏。** 从顶部会话栏打开「控制中心」调整画面、声音、显示器和输入设置，或使用远程工具。也可以打开「文件传输」、切换全屏，或点击「结束」断开连接。
5. **帮助别人。** 请对方选择「让别人帮助我……」，把帮助 ID 和验证码告诉你。在你的设备上选择「帮助别人」，输入对方提供的信息。

</td>
  </tr>
</table>

在手机上同样是选择设备后点「连接」。轻点或拖动屏幕即可移动指针，也可以切换到鼠标模式。点击悬浮球可调出键盘、快捷键和工具。

---

## 兼容性

### 平台

| 平台 | 版本 | 作为主控端 | 作为被控端 |
|---|---|:---:|:---:|
| **Windows** | Windows 10（1809 或更高版本）、Windows 11，x64 | ✅ | ✅ 安装服务后也可访问登录界面 |
| **Linux** | glibc 2.35 或更高版本（Ubuntu 22.04+、Debian 12+、Fedora 36+），x86_64 | ✅ | ✅ 支持 X11 和 Wayland，安装服务后也可访问登录界面 |
| **Android** | Android 8.0 或更高版本 | 即将推出 | ❌ 不支持 |
| **iOS / iPadOS** | iOS 15 或更高版本 | 即将推出 | ❌ 不支持，iOS 不允许应用远程控制本机 |

### Linux 桌面环境

| 会话类型 | 画面采集与输入 |
|---|---|
| X11（任意桌面） | ✅ 通过 MIT-SHM 采集画面，通过 XTest 处理输入 |
| Wayland（GNOME） | ✅ 通过桌面门户采集画面并处理输入，用户首次授权后会记住权限 |
| Wayland（wlroots，如 Sway） | ✅ 通过桌面门户采集画面，通过合成器的虚拟指针和键盘处理输入 |
| Wayland（KDE Plasma） | ⚠️不稳定，未测试 |

### 显卡与编解码器

Queqiao 默认选择两端都支持的视频格式，也可在会话控制中心手动指定。可用格式取决于显卡型号和驱动。

| 平台 | 显卡厂商 | 编码（被控端） | 解码（主控端） |
|---|---|---|---|
| **Windows** | NVIDIA | NVENC（H.264、H.265、AV1） | NVDEC（H.264、H.265） |
| **Windows** | Intel | Quick Sync / oneVPL（H.264、H.265） | oneVPL（H.264、H.265） |
| **Windows** | AMD | AMF（H.264、H.265） | AMF（H.264、H.265） |
| **Windows** | 各厂商通用 | Media Foundation（H.264、H.265、AV1） | Media Foundation（H.264、H.265、AV1） |
| **Linux** | NVIDIA | NVENC（H.264、H.265、AV1） | NVDEC（H.264、H.265、AV1） |
| **Linux** | Intel、AMD 及其他 | OpenH264（H.264，软件） | OpenH264（H.264，软件） |

Windows 上，AV1 解码统一使用 Media Foundation。两种桌面平台都可以用 OpenH264 在 CPU 上编解码 H.264。

移动主控端计划支持 Android（MediaCodec 解码）和 iOS/iPadOS（VideoToolbox 解码）。

#### 色彩

| 会话编码 | 色度采样与色深 |
|---|---|
| H.264 | 4:2:0，8 位 |
| H.265（NVIDIA NVENC/NVDEC、Intel oneVPL） | 4:2:0 或 4:4:4，均支持 8 位或 10 位 |
| H.265（AMD AMF） | 4:2:0，8 位或 10 位 |
| H.265（Windows Media Foundation） | 4:2:0，8 位 |
| AV1 | 4:2:0，8 位 |

桌面视频采用 BT.709 有限范围。两端都支持时，可在会话控制中心开启 H.265 的 4:4:4，保留彩色小字和细线的边缘细节。

### 网络

在大多数家庭和办公网络中，设备可通过 IPv4 或 IPv6 直连。如果网络条件限制直连（例如使用手机热点时），会话会通过你的服务器中转。连接服务器时，Queqiao 会绕过本机代理和 VPN 网卡。

---

## 问题与反馈

- 如有疑问、遇到故障或有改进建议，欢迎[提交 Issue](https://github.com/GuoWQ222/Queqiao/issues)。报告故障时，请附上操作步骤、实际结果、操作系统和 Queqiao 版本（运行 `queqiao-agent --version` 查看）。

---

## 许可

版权所有 © 2026 GuoWQ222，保留所有权利。未经著作权人书面许可，不得复制、修改、分发或使用 Queqiao 的任何部分。详见[许可文件](LICENSE)。

第三方组件各自遵循原有许可证，包括打过补丁的 sctp-proto、从 NVIDIA、Intel 和 AMD 以 MIT 许可证发布的头文件生成的绑定，以及编译进程序的 Rust 依赖。详见[第三方声明](THIRD_PARTY_NOTICES.md)。
