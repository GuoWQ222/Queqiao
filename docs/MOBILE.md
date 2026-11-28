# Queqiao on phones and tablets (iOS, Android)

The user asked (2026-11-26) for an iOS app and an Android app with every function and feature UU Remote's mobile apps
have. UU's apps are a behaviour reference only (their store pages and reviews; nothing of theirs goes into this
repository).

## How they are built

- **One Rust core for both** (`crates/queqiao-mobile`): the account (log in, saved login, the device list, its
  changes), sessions as a viewer through the server (signalling, the WebRTC transport, the data channel and its
  streams), input, the clipboard and files: the crates the desktop viewer already uses (`queqiao-core`,
  `queqiao-account`, `queqiao-transport`, `queqiao-files`), with the platform parts they lack on Android and iOS
  (network interfaces, the settings folder given by the app). The apps call it through a small C interface (JSON
  events in, JSON commands out; frames as bytes): JNI on Android, a static library in an XCFramework on iOS.
- **Pictures and sound are decoded by the phone**: the core hands over H.264/H.265 frames (Annex B) and Opus packets;
  Android decodes them with MediaCodec (into a SurfaceView), iOS with VideoToolbox (AVSampleBufferDisplayLayer) and
  AudioToolbox. Hardware decoding, no codec library to build.
- **The apps' own interfaces** in Queqiao's look (night blue, star gold, the magpie): Jetpack Compose on Android,
  SwiftUI on iOS.

## Where they are built and tested

- **Android**: GitHub Actions on Linux runners (KVM): the core with the NDK (`scripts/build_mobile_core.sh`), the APK with Gradle (`scripts/build_android.sh`), then the Android
  emulator. A test there starts the account server and a Linux agent on a virtual screen on the same runner, logs the
  app in (the emulator reaches the runner at 10.0.2.2), connects, checks that pictures are decoded and that input
  arrives (`scripts/run_android_e2e.sh`, `.github/workflows/android.yml`). Nothing is installed on the user's machines. The Linux build machine builds the
  core and the APK too (the NDK, SDK and Gradle in ~/android) for quicker checks, and
  `scripts/run_mobile_core_probe.sh` runs the core against a server and an agent there without a phone.
- **iOS**: GitHub Actions on macOS runners (the repository is public): the core for the device and the simulator, the
  app with Xcode, its tests in the iPhone Simulator against a test agent that streams a test picture. The user installs
  the unsigned `.ipa` with **Sideloadly and a free Apple ID** (Apple's limit: signed again every 7 days).
- Results (logs, pictures, the `.apk` and `.ipa`) are pushed by the workflows to the branches
  `ci-results-android` and `ci-results-ios` (replaced each run), which the relay fetches with git (no GitHub token there).

## What they do (UU Remote's mobile apps, with Queqiao's own)

Controlling a PC (both apps):
- [x] Log in, saved login, the device list (online, offline, this device), remove a device
- [x] Connect; remote help with a help ID and code (the invitation link: not yet); wake a PC up (Wake-on-LAN through
      another device of the account)
- [x] The PC's Windows password when it is locked or signed out (as on the desktop)
- [x] Touch mode (the finger is the pointer: tap = click, long press = right click, drag = drag) and mouse mode (a
      pointer moved like a touchpad, its buttons on screen); two fingers scroll, pinch zooms the picture, it pans
- [x] The phone's keyboard, typing any language (Unicode text to the PC); a key bar: Esc, Tab, Ctrl, Alt, Shift, Win,
      F1-F12, arrows, Home/End, Delete, Ctrl+Alt+Del, copy/paste/undo, Win+D and the like
- [ ] Game keys: on-screen buttons and a stick mapped to keys (custom layouts, saved), and a virtual gamepad
- [ ] Hardware keyboards, mice and gamepads attached to the phone or tablet
- [ ] Quality (smooth, balanced, sharp, custom bitrate), frame rate (30, 60, up to what the screen shows), codec
      (H.264, H.265, AV1 where the phone decodes it), resolution
- [x] Several screens: switch between them
- [x] The PC's sound on the phone; mute the PC's speakers
- [ ] Privacy screen, lock the PC, restart it, end the session; lock after the session (done: privacy screen, lock,
      end; not yet: restart, lock after)
- [ ] Clipboard both ways (text: done; images: not yet)
- [ ] Files both ways (pick on the phone, save from the PC; the transfer list)
- [x] The session's figures (direct or relayed, delay, frame rate, data), as the desktop's card
- [ ] Landscape and portrait, full screen, the picture fitted or 1:1
- [ ] Settings: display language (as the desktop), quality defaults, haptics, keep the screen on

Being controlled (Android only; iOS lets no app control the phone):
- [ ] The phone's screen to a PC (MediaProjection, encoded with MediaCodec), with the person's consent each time
- [ ] Input from the PC through an AccessibilityService (taps, swipes, text, back/home/recent), only once the person
      has enabled it

Status (this file records what is done as it lands):
- 2026-11-26: the core (log in, the device list, a session, frames, input) passes the probe on the Linux build machine. The Android
  app's first version (`mobile/android`): log in, the device list (remove a device), connect, H.264/H.265 decoded
  by MediaCodec, the PC's sound (Opus by MediaCodec), touch and mouse modes (tap, long press, double tap and drag,
  two-finger tap, scroll, pinch zoom and pan), the phone's keyboard (US keys; other text through the PC's clipboard
  and Ctrl+V for now), a key bar, Ctrl+Alt+Del, lock, privacy screen, the PC's password form; English and Chinese.
- 2026-11-26: the iOS app's first version (`mobile/ios`), with the same functions; built on GitHub's Mac with an
  unsigned `Queqiao.ipa` (branch ci-results-ios, for Sideloadly). Both apps: typing any language (Message::TypeText:
  SendInput KEYEVENTF_UNICODE on Windows, keysyms on X11, the portal on Wayland), help someone by help ID and code,
  wake up, picture settings (quality, frame rate, screens), the figures, the clipboard both ways (text), mute the PC.
  Checked: the core's probe on the Linux build machine (typed "typed 中文 é" in an xterm, a help session); iOS in the Simulator against
  a stand-in PC (examples/test_agent: 867 frames shown, the typed text received); Windows typing on the test laptop
  (examples/type_text_probe, Chinese keyboard layout: TYPE_TEXT_PASS). The Android emulator test (log in, connect,
  frames, input) runs in android.yml.
