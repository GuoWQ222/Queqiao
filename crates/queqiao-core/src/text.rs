//! The words people see, in their language: the one chosen in Settings (Display language), else the system's:
//! Simplified Chinese when Windows' display language is Chinese (Linux: LANG), English otherwise.
//! `QUEQIAO_LANG=en` or `zh` decides above both (tests). Reports and logs stay English (tests read them).
//!
//! A text is looked up by its English wording, so code reads as English; one without a translation shows in
//! English. `{name}` marks a value filled in by [`trf`].

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    English,
    Chinese,
}

/// What Settings' "Display language" holds: the system's language, or one of Queqiao's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LanguageChoice {
    #[default]
    System,
    Chinese,
    English,
}

impl LanguageChoice {
    pub const ALL: [LanguageChoice; 3] = [
        LanguageChoice::System,
        LanguageChoice::Chinese,
        LanguageChoice::English,
    ];

    /// As Settings lists it: each language in its own words, so anyone can find theirs.
    pub fn label(self) -> &'static str {
        match self {
            LanguageChoice::System => tr("Same as the system"),
            LanguageChoice::Chinese => "简体中文",
            LanguageChoice::English => "English",
        }
    }

    fn word(self) -> &'static str {
        match self {
            LanguageChoice::System => "system",
            LanguageChoice::Chinese => "zh",
            LanguageChoice::English => "en",
        }
    }

    /// As the file keeps it (anything else: the system's).
    fn from_word(word: &str) -> LanguageChoice {
        LanguageChoice::ALL
            .into_iter()
            .find(|choice| choice.word() == word.trim())
            .unwrap_or_default()
    }
}

/// The language in use (0: not decided yet, then 1 English, 2 Chinese): decided on first use, changed by
/// [`choose_language`] while the program runs (what is drawn after that is in the new language).
static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn language() -> Language {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Language::English,
        2 => Language::Chinese,
        _ => {
            let decided = decide();
            CURRENT.store(code(decided), Ordering::Relaxed);
            decided
        }
    }
}

fn code(language: Language) -> u8 {
    match language {
        Language::English => 1,
        Language::Chinese => 2,
    }
}

fn decide() -> Language {
    match std::env::var("QUEQIAO_LANG").ok().as_deref() {
        Some("en") => Language::English,
        Some("zh") => Language::Chinese,
        _ => match saved_choice() {
            LanguageChoice::English => Language::English,
            LanguageChoice::Chinese => Language::Chinese,
            LanguageChoice::System => system_language(),
        },
    }
}

/// The file that keeps the choice: one word (system, zh or en) in this person's Queqiao folder.
fn choice_file() -> Option<std::path::PathBuf> {
    crate::config_folder()
        .ok()
        .map(|folder| folder.join("language.txt"))
}

/// The display language chosen in Settings (the system's when none was).
pub fn saved_choice() -> LanguageChoice {
    let word = choice_file()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    LanguageChoice::from_word(&word)
}

/// Keeps `choice` for every Queqiao program of this person, and uses it in this one from now on.
pub fn choose_language(choice: LanguageChoice) -> std::io::Result<()> {
    let path = choice_file().ok_or_else(|| std::io::Error::other("no settings folder"))?;
    std::fs::write(path, choice.word())?;
    CURRENT.store(code(decide()), Ordering::Relaxed);
    Ok(())
}

#[cfg(windows)]
fn system_language() -> Language {
    // The primary language of the display language (LANGID & 0x3FF); 0x04 is Chinese.
    let id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    if id & 0x3FF == 0x04 {
        Language::Chinese
    } else {
        Language::English
    }
}

/// The locale's language, as programs read it: LC_ALL, else LC_MESSAGES, else LANG ("zh_CN.UTF-8" is Chinese).
#[cfg(not(windows))]
fn system_language() -> Language {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_default();
    if locale.starts_with("zh") {
        Language::Chinese
    } else {
        Language::English
    }
}

/// `english` in the user's language.
/// `english` and its translations (to read text written in any language Queqiao speaks).
pub fn in_all_languages(english: &'static str) -> Vec<&'static str> {
    let mut all = vec![english];
    all.extend(
        CHINESE
            .iter()
            .filter(|(source, _)| *source == english)
            .map(|(_, chinese)| *chinese),
    );
    all
}

pub fn tr(english: &'static str) -> &'static str {
    translate(english, language())
}

/// [`tr`] with each `{name}` replaced by its value.
pub fn trf(english: &'static str, values: &[(&str, &str)]) -> String {
    fill(tr(english), values)
}

fn translate(english: &'static str, language: Language) -> &'static str {
    match language {
        Language::English => english,
        Language::Chinese => CHINESE
            .iter()
            .find(|(source, _)| *source == english)
            .map_or(english, |(_, chinese)| chinese),
    }
}

/// A relay's place as people read it (the user, 2026-11-26: "add the relay's name"): its name from the account server
/// ("shanghai", "hong-kong") in words ("Shanghai", "Hong Kong"), in the person's language when the table has the
/// place (上海). Empty for an empty name.
pub fn relay_place(name: &str) -> String {
    relay_place_in(name, language())
}

/// [`relay_place`] in English or in Chinese, for the phone apps (they pick by the phone's language).
pub fn relay_place_for(name: &str, chinese: bool) -> String {
    relay_place_in(
        name,
        if chinese {
            Language::Chinese
        } else {
            Language::English
        },
    )
}

fn relay_place_in(name: &str, language: Language) -> String {
    let words: Vec<String> = name
        .split(['-', '_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect();
    let english = words.join(" ");
    match language {
        Language::English => english,
        Language::Chinese => CHINESE
            .iter()
            .find(|(source, _)| *source == english)
            .map_or(english.clone(), |(_, chinese)| (*chinese).to_string()),
    }
}

fn fill(text: &str, values: &[(&str, &str)]) -> String {
    let mut out = text.to_string();
    for (name, value) in values {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// English -> Simplified Chinese.
const CHINESE: &[(&str, &str)] = &[
    ("Queqiao", "鹊桥"),
    // Logging in and the device list.
    ("Queqiao - log in", "鹊桥 - 登录"),
    ("Server (host:port)", "服务器（主机:端口）"),
    (
        "Server certificate fingerprint (SHA-256)",
        "服务器证书指纹（SHA-256）",
    ),
    ("User name", "用户名"),
    ("Password", "密码"),
    ("Log in", "登录"),
    ("Please fill in all four fields.", "请填写全部四项。"),
    (
        "Signed in as {user}. Your devices:",
        "已登录：{user}。你的设备：",
    ),
    (
        "Double-click a device to connect to it.",
        "双击设备即可连接。",
    ),
    ("Log out", "退出登录"),
    ("Connect", "连接"),
    ("Online", "在线"),
    ("Offline", "离线"),
    ("this PC", "本机"),
    ("Connected to {name}.", "已连接到 {name}。"),
    (
        "The session with {name} has ended.",
        "与 {name} 的会话已结束。",
    ),
    (
        "Could not connect to {name}: {error}",
        "无法连接到 {name}：{error}",
    ),
    (
        "{name} is offline: Queqiao is not running there.",
        "{name} 不在线：那台电脑上没有运行鹊桥。",
    ),
    (
        "Lost the connection to the server ({reason}). Trying again...",
        "与服务器的连接已断开（{reason}）。正在重试……",
    ),
    ("Connected to the server again.", "已重新连接到服务器。"),
    (
        "The server cannot be reached ({error}). Trying again...",
        "无法连接到服务器（{error}）。正在重试……",
    ),
    // The controlled PC: tray, banner, consent, privacy screen.
    (
        "{viewer} is controlling this PC",
        "{viewer} 正在控制这台电脑",
    ),
    ("End", "结束"),
    ("Queqiao - allow remote control?", "鹊桥 - 允许远程控制？"),
    (
        "{viewer}, a device of your account, wants to control this PC.",
        "你账号下的设备 {viewer} 想要控制这台电脑。",
    ),
    ("Deny", "拒绝"),
    ("Allow", "允许"),
    (
        "Denied automatically in {seconds} s unless you allow it.",
        "{seconds} 秒后自动拒绝，除非你允许。",
    ),
    (
        "Queqiao: {viewer} is controlling this PC",
        "鹊桥：{viewer} 正在控制这台电脑",
    ),
    (
        "Queqiao: ready for your devices",
        "鹊桥：已就绪，等待你的设备连接",
    ),
    ("Queqiao: ready", "鹊桥：已就绪"),
    (
        "Queqiao: cannot reach the server, trying again",
        "鹊桥：连不上服务器，正在重试",
    ),
    ("Queqiao: not logged in", "鹊桥：未登录"),
    ("Log in...", "登录……"),
    ("Check for updates...", "检查更新……"),
    ("Wake-on-LAN check...", "检查网络唤醒……"),
    (
        "Wake-on-LAN on this PC (another device of your account wakes it through a PC on the same network):",
        "这台电脑的网络唤醒（你账号的其他设备通过同一网络里的电脑来唤醒它）：",
    ),
    ("yes", "是"),
    ("no", "否"),
    ("unknown", "未知"),
    (
        "- {connection} ({device}): may wake the PC: {armed}; on a magic packet: {magic}",
        "- {connection}（{device}）：允许唤醒电脑：{armed}；响应魔术包：{magic}",
    ),
    (
        "  A Wi-Fi card can seldom wake a PC that is off: use a network cable for it.",
        "  无线网卡很少能唤醒已关机的电脑：请改用网线。",
    ),
    ("- No network card found.", "- 没有找到网卡。"),
    (
        "- Fast Startup is on: many network cards do not wake a PC shut down with it. Turn it off in Control Panel > Power Options > Choose what the power buttons do.",
        "- 已开启快速启动：用它关机后，很多网卡无法唤醒电脑。请在“控制面板 > 电源选项 > 选择电源按钮的功能”中关闭。",
    ),
    (
        "Windows is ready. If the PC still does not wake, turn on Wake on LAN (or Power On by PCI-E) in its BIOS/UEFI.",
        "Windows 这边已就绪。如果电脑仍无法唤醒，请在 BIOS/UEFI 中开启 Wake on LAN（或 Power On by PCI-E）。",
    ),
    (
        "To set: Device Manager > Network adapters > the wired card > Power Management: Allow this device to wake the computer, and Only allow a magic packet; Advanced: Wake on Magic Packet: Enabled (this needs an administrator). Then Wake on LAN in the BIOS/UEFI.",
        "设置方法：设备管理器 > 网络适配器 > 有线网卡 > 电源管理：勾选“允许此设备唤醒计算机”和“只允许幻数据包唤醒计算机”；高级：Wake on Magic Packet 设为“启用”（需要管理员）。然后在 BIOS/UEFI 中开启 Wake on LAN。",
    ),
    ("Install updates automatically", "自动安装更新"),
    (
        "Queqiao is up to date (version {version}).",
        "鹊桥已是最新版本（{version}）。",
    ),
    (
        "Queqiao {version} is ready to install. Queqiao closes for a moment and starts again.\n\n{notes}\n\nInstall it now?",
        "鹊桥 {version} 已下载，可以安装。安装时鹊桥会关闭片刻，然后重新启动。\n\n{notes}\n\n现在安装吗？",
    ),
    ("The update check failed: {error}", "检查更新失败：{error}"),
    (
        "Queqiao {version} did not start on this PC and was put back. Queqiao waits for a later version.",
        "鹊桥 {version} 在这台电脑上没能启动，已恢复为原来的版本。鹊桥会等待更新的版本。",
    ),
    (
        "Updates come with an installed Queqiao (installed with queqiao-setup).",
        "只有用 queqiao-setup 安装的鹊桥才会更新。",
    ),
    ("Queqiao - log in this PC", "鹊桥 - 登录这台电脑"),
    ("Logging in...", "正在登录……"),
    (
        "This PC is not online, so nobody can be let in to help. Log in from the tray icon, or check the network.",
        "这台电脑未联网，暂时无法让别人来协助。请从托盘图标登录，或检查网络。",
    ),
    ("End the session", "结束会话"),
    (
        "Let my account's devices control this PC",
        "允许我账号下的设备控制这台电脑",
    ),
    ("Ask me before each session", "每次会话前询问我"),
    ("Share the clipboard", "共享剪贴板"),
    ("Receive files", "接收文件"),
    ("Allow remote command lines", "允许远程命令行"),
    ("Start with Windows", "开机自动启动"),
    ("Start when I sign in", "登录时自动启动"),
    ("Yes", "是"),
    ("No", "否"),
    (
        "Files to send (paths, separated by ;):",
        "要发送的文件（路径，用 ; 分隔）：",
    ),
    ("Lock this PC when a session ends", "会话结束时锁定这台电脑"),
    (
        "Lock these settings with a password...",
        "用密码锁定这些设置……",
    ),
    ("Remove the settings password...", "移除设置密码……"),
    ("The settings password:", "设置密码："),
    ("A password for these settings:", "为这些设置设一个密码："),
    ("The same password again:", "再输入一次同样的密码："),
    ("Quit Queqiao on this PC", "退出这台电脑上的鹊桥"),
    (
        "This PC is being used remotely with Queqiao. Its screen stays dark and its keyboard and mouse stay off until the session ends. Ctrl+Alt+Del gives them back.",
        "这台电脑正在通过鹊桥被远程使用。会话结束前屏幕保持黑屏，键盘和鼠标暂时不可用。按 Ctrl+Alt+Del 可恢复。",
    ),
    // The session window.
    ("Send Ctrl+Alt+Del", "发送 Ctrl+Alt+Del"),
    ("Lock that PC", "锁定那台电脑"),
    ("Restart Queqiao on that PC", "重启那台电脑上的鹊桥"),
    ("Restart that PC...", "重启那台电脑……"),
    ("Play that PC's sound", "播放那台电脑的声音"),
    ("Mute that PC's speakers", "让那台电脑的扬声器静音"),
    ("Darken that PC's screen", "让那台电脑黑屏（隐私屏）"),
    ("Open a command line on that PC", "在那台电脑上打开命令行"),
    ("Full screen ({key})", "全屏（{key}）"),
    ("Game mouse ({key})", "游戏鼠标（{key}）"),
    ("Hotkeys...", "快捷键……"),
    (
        "Full screen key (for example Ctrl+Alt+Enter):",
        "全屏快捷键（例如 Ctrl+Alt+Enter）：",
    ),
    (
        "Game mouse key (for example Ctrl+Alt+M):",
        "游戏鼠标快捷键（例如 Ctrl+Alt+M）：",
    ),
    (
        "Boss key, hides this window at once (for example Ctrl+Alt+H; empty: none):",
        "老板键，立即隐藏此窗口（例如 Ctrl+Alt+H；留空：不用）：",
    ),
    (
        "Not a key Queqiao can use: {key}. A key is Ctrl and/or Alt (Shift may be added) with one letter, digit, F1 to F12 or Enter, Space, Home and the like.",
        "鹊桥无法使用这个快捷键：{key}。快捷键需包含 Ctrl 和/或 Alt（可加 Shift），再加一个字母、数字、F1 到 F12，或 Enter、Space、Home 等。",
    ),
    ("Always on top", "窗口置顶"),
    ("Transfers...", "传输列表……"),
    ("Queqiao - transfers", "鹊桥 - 传输列表"),
    (
        "Files on their way (sent and received):",
        "正在传输的文件（发送与接收）：",
    ),
    ("Pause / resume", "暂停 / 继续"),
    ("Cancel", "取消"),
    ("Install Queqiao", "安装鹊桥"),
    ("Install", "安装"),
    (
        "Also at the sign-in, lock and UAC screens (installs the Queqiao service; needs an administrator)",
        "在登录、锁屏和 UAC 界面也可控制（安装鹊桥服务，需要管理员）",
    ),
    (
        "Queqiao will be installed for you on this PC, in {folder}. No administrator is needed for that.",
        "鹊桥将为你安装在这台电脑的 {folder}，这一步不需要管理员。",
    ),
    ("Installing...", "正在安装……"),
    (
        "Queqiao is installed. Find it in the Start menu.",
        "鹊桥已安装，可在开始菜单中找到。",
    ),
    ("Remove Queqiao from this PC?", "要从这台电脑上卸载鹊桥吗？"),
    (
        "Queqiao has been removed from this PC.",
        "鹊桥已从这台电脑上卸载。",
    ),
    ("sending", "发送"),
    ("receiving", "接收"),
    ("paused", "已暂停"),
    ("Custom bitrate...", "自定义码率……"),
    (
        "Highest bitrate in Mbit/s (0: as the quality setting says):",
        "最高码率（Mbit/s，0 表示按画质设置）：",
    ),
    (
        "Send Windows shortcuts to that PC (Win, Alt+Tab)",
        "把 Windows 快捷键发给那台电脑（Win、Alt+Tab）",
    ),
    ("Frame rate", "帧率"),
    ("Picture quality", "画质"),
    ("Resolution", "分辨率"),
    ("Screens", "屏幕"),
    ("Screen {n}", "屏幕 {n}"),
    ("As that screen", "与那台电脑的屏幕一致"),
    ("30 frames per second", "每秒 30 帧"),
    ("60 frames per second", "每秒 60 帧"),
    ("As many as that screen shows", "跟随那台电脑屏幕的刷新率"),
    (
        "Smooth (fewer bits, slow networks)",
        "流畅（码率低，适合慢速网络）",
    ),
    ("Balanced", "均衡"),
    (
        "Sharp (twice the bits, for text)",
        "清晰（码率加倍，适合文字）",
    ),
    (
        "Restart {agent}? Programs there may ask to save their work; unsaved work can be lost.",
        "要重启 {agent} 吗？那里的程序可能会要求保存工作，未保存的内容可能会丢失。",
    ),
    ("direct", "直连"),
    ("relayed", "中转"),
    // Helping someone of another account.
    ("Let someone help me...", "让别人帮助我……"),
    ("Queqiao - let someone help you", "鹊桥 - 让别人帮助你"),
    (
        "Give these to the person who helps you. They can ask once to control this PC; you decide in a prompt.",
        "把下面的信息告诉帮助你的人。对方可以请求一次控制这台电脑，由你在弹出的提示中决定是否允许。",
    ),
    ("Help ID:", "帮助 ID："),
    ("Code:", "验证码："),
    ("New code", "换一个验证码"),
    ("Close", "关闭"),
    ("Help someone...", "帮助别人……"),
    (
        "Please help me on my PC with Queqiao: in Queqiao choose Help someone... (or open the link).",
        "请用鹊桥远程帮我看看电脑：在鹊桥中选择“帮助别人……”（或打开链接）。",
    ),
    ("Copy invitation", "复制邀请"),
    ("Create an account", "注册账号"),
    (
        "The two passwords are not the same.",
        "两次输入的密码不一致。",
    ),
    ("Creating the account...", "正在注册……"),
    ("Command lines...", "命令行……"),
    ("Opacity", "透明度"),
    ("That PC's pointer shape", "使用对方电脑的指针形状"),
    ("Game mouse speed", "游戏鼠标速度"),
    ("Picture codec", "画面编码"),
    ("Send my microphone", "发送我的麦克风"),
    ("Automatic", "自动"),
    ("AV1 (both PCs need it)", "AV1（两台电脑都需支持）"),
    ("H.265 (both PCs need it)", "H.265（两台电脑都需支持）"),
    ("4:4:4 colour (H.265)", "4:4:4 色彩（H.265）"),
    ("10-bit colour (H.265)", "10 位色彩（H.265）"),
    ("Slow (x0.5)", "慢（×0.5）"),
    ("Normal", "正常"),
    ("Fast (x2)", "快（×2）"),
    ("Opaque", "不透明"),
    ("Actual size (1:1)", "原始大小（1:1）"),
    (
        "{name}: detach (it keeps running)",
        "{name}：分离（继续运行）",
    ),
    ("{name}: show", "{name}：显示"),
    ("{name}: rename...", "{name}：重命名……"),
    ("{name}: end", "{name}：结束"),
    ("No command lines are open.", "没有打开的命令行。"),
    ("A name for this command line:", "这个命令行的名称："),
    (
        "Detached: the command line keeps running on that PC (Command lines... in the session menu shows it again).",
        "已分离：命令行仍在那台电脑上运行（会话菜单中的“命令行……”可再次显示）。",
    ),
    (
        "A fixed help code for this PC...",
        "为这台电脑设置固定验证码……",
    ),
    ("Remove the fixed help code", "取消固定验证码"),
    (
        "A fixed help code (8 or more letters or digits):",
        "固定验证码（8 位以上字母或数字）：",
    ),
    (
        "A fixed help code needs 8 or more letters or digits.",
        "固定验证码至少需要 8 位字母或数字。",
    ),
    (
        "Copied: send it to the person who helps you.",
        "已复制，请发给帮助你的人。",
    ),
    (
        "Filled in from the clipboard: check it and press Connect.",
        "已从剪贴板填入，请核对后点“连接”。",
    ),
    (
        "Filled in from the invitation link: check it and press Connect.",
        "已从邀请链接填入，请核对后点“连接”。",
    ),
    ("Wake up", "唤醒"),
    ("More...", "更多……"),
    ("Rename...", "重命名……"),
    ("Favourite", "收藏"),
    ("Remove from the account...", "从账号中移除……"),
    (
        "A name of your own for {name}:",
        "为 {name} 起一个你自己的名字：",
    ),
    (
        "Remove {name} from the account? Its saved login ends; it needs the password again to come back.",
        "要把 {name} 从账号中移除吗？它保存的登录将失效，需要再次输入密码才能回来。",
    ),
    ("OK", "确定"),
    ("Select a device first.", "请先选择一台设备。"),
    ("Queqiao - help someone", "鹊桥 - 帮助别人"),
    (
        "Ask the person you help for the help ID and code that Queqiao shows them (tray menu: Let someone help me...).",
        "请向你要帮助的人索取鹊桥显示的帮助 ID 和验证码（托盘菜单：让别人帮助我……）。",
    ),
    ("Help ID", "帮助 ID"),
    ("Code", "验证码"),
    (
        "Code (leave empty to ask the person to allow it)",
        "验证码（留空则请对方确认）",
    ),
    (
        "{viewer} (account {user}) asks to help you on this PC. Allow it?",
        "{viewer}（账号 {user}）请求帮助你操作这台电脑。允许吗？",
    ),
    (
        "{viewer} (account {user}) wants to control this PC to help you.",
        "{viewer}（账号 {user}）想要控制这台电脑来帮助你。",
    ),
    (
        "{viewer} (account {user}) is helping on this PC",
        "{viewer}（账号 {user}）正在这台电脑上帮助你",
    ),
    // Command line windows.
    ("The session has ended.", "会话已结束。"),
    (
        "The command line ended (exit code {code}).",
        "命令行已结束（退出码 {code}）。",
    ),
    (
        "This window closes in 10 seconds.",
        "此窗口将在 10 秒后关闭。",
    ),
    (
        "Queqiao: the connection to the session ended.",
        "鹊桥：与会话的连接已断开。",
    ),
    ("Queqiao: the viewer is not there.", "鹊桥：找不到控制端。"),
    // The main window.
    ("Devices", "设备"),
    ("Help", "远程帮助"),
    ("Transfers", "传输"),
    ("Settings", "设置"),
    (
        "Devices logged in to your account can control each other.",
        "登录同一账号的设备之间可以互相控制。",
    ),
    (
        "Help across accounts: always with the help ID and code, and only after an Allow.",
        "跨账号帮助：始终需要帮助 ID 和验证码，并且只有在对方点击“允许”之后。",
    ),
    (
        "Files sent and received in sessions.",
        "会话中发送和接收的文件。",
    ),
    (
        "What may happen on this PC, and how Queqiao runs.",
        "这台电脑上允许做什么，以及鹊桥如何运行。",
    ),
    ("Log in to see your devices.", "登录后即可看到你的设备。"),
    ("Stop", "停止"),
    ("Fixed help code...", "固定帮助码……"),
    (
        "Nothing yet: files sent in a session (drag them onto its window) and files received appear here.",
        "还没有记录：会话中发送的文件（拖到会话窗口上）和接收的文件会显示在这里。",
    ),
    ("Open received files", "打开接收的文件"),
    ("Clear the list", "清空列表"),
    (
        "This account's devices may control this PC",
        "本账号的设备可以控制这台电脑",
    ),
    ("Ask before each session", "每次会话前询问"),
    ("Allow command lines", "允许命令行"),
    ("Lock this PC after a session", "会话结束后锁定这台电脑"),
    ("Check for updates", "检查更新"),
    ("Lock settings with a password...", "用密码锁定设置……"),
    ("Remove the settings password", "移除设置密码"),
    (
        "Type the help ID and the code you were given.",
        "请输入对方给你的帮助 ID 和验证码。",
    ),
    (
        "No devices yet: log in to this account on another PC, and it appears here.",
        "还没有其他设备：在另一台电脑上登录这个账号，它就会出现在这里。",
    ),
    ("Cannot reach the server", "无法连接服务器"),
    ("Not logged in", "未登录"),
    ("Let someone help this PC", "让别人帮助这台电脑"),
    (
        "Someone of another account can control this PC with its help ID and code, after you allow it each time. Command lines, files, restarting and the sign-in screen stay closed to them.",
        "其他账号的人可以凭这台电脑的帮助 ID 和验证码控制它，每次都要你先允许。命令行、文件、重启和登录界面对他们保持关闭。",
    ),
    ("Help someone", "帮助别人"),
    (
        "Pasting an invitation into either field fills in both. The person there is asked before you see their screen.",
        "把邀请粘贴到任一栏即可同时填好两栏。在你看到对方屏幕之前，对方会先被询问。",
    ),
    ("This PC", "这台电脑"),
    ("Version {version}", "版本 {version}"),
    ("Remove from this account...", "从本账号中移除……"),
    (
        "Take {name} out of your account? It can no longer control your devices or be controlled until it logs in again.",
        "要把 {name} 从你的账号中移除吗？在它重新登录之前，它既不能控制你的设备，也不能被控制。",
    ),
    (
        "Log this PC out of your account? Your devices can no longer control it until it logs in again.",
        "要让这台电脑退出你的账号吗？在它重新登录之前，你的设备无法控制它。",
    ),
    ("Open Queqiao", "打开鹊桥"),
    (
        "from {peer} · {size} · {when}",
        "来自 {peer} · {size} · {when}",
    ),
    (
        "to {peer} · {size} · {when}",
        "发往 {peer} · {size} · {when}",
    ),
    (
        "The Queqiao viewer is not installed for you on this PC.",
        "这台电脑上没有为你安装鹊桥查看器。",
    ),
    ("Remote help", "远程帮助"),
    (
        "Someone of another account starts helping with:",
        "其他账号的人开始帮助时需要：",
    ),
    ("The code, then I allow it", "验证码，再由我允许"),
    ("The code alone", "只需验证码"),
    ("No code: I allow each request", "无需验证码，由我逐次允许"),
    ("Allow files while someone helps", "被帮助时允许传输文件"),
    (
        "The person at that PC answers this screen (sign-in, UAC or Ctrl+Alt+Del) themselves: someone helping cannot operate it.",
        "这个界面（登录、UAC 或 Ctrl+Alt+Del）由那台电脑前的人自己操作，帮助者无法操作。",
    ),
    ("New help ID", "换一个帮助 ID"),
    (
        "That PC is locked. Type the password of its account to show that it is you (this does not unlock it: unlock it on its own screen then).",
        "那台电脑已锁定。请输入它的账户密码来验证身份（这不会解锁它：之后请在它的锁屏上解锁）。",
    ),
    (
        "Nobody is signed in on that PC. Type the password of the account that set up Queqiao there to show that it is you.",
        "那台电脑上没有人登录。请输入在那里设置 Queqiao 的账户的密码来验证身份。",
    ),
    (
        "That PC is locked: only the person there can unlock it.",
        "那台电脑已锁定：只有那台电脑前的人才能解锁。",
    ),
    ("Queqiao - {device} is locked", "鹊桥 - {device} 已锁定"),
    (
        "Wrong password: {tries} tries left.",
        "密码错误，还可以再试 {tries} 次。",
    ),
    ("Checking...", "正在验证……"),
    (
        "A new device, {name}, logged in to your account. If that was not you, change your password and take it out of the account.",
        "新设备 {name} 登录了你的账号。如果不是你本人，请修改密码并把它从账号中移除。",
    ),
    (
        "{name} logged in again with a new device key. If that was not you, change your password and take it out of the account.",
        "{name} 用新的设备密钥重新登录了你的账号。如果不是你本人，请修改密码并把它从账号中移除。",
    ),
    (
        "{server} has another certificate: its fingerprint begins {actual}, the one entered {pinned}. Check the fingerprint with whoever runs the server.",
        "{server} 的证书与填写的不符：它的指纹以 {actual} 开头，填写的以 {pinned} 开头。请向服务器的管理者核对指纹。",
    ),
    // The main window's device page (2026-11-23).
    ("My devices", "我的设备"),
    ("No other devices yet.", "还没有其他设备。"),
    ("Send files", "发送文件"),
    ("Command line", "命令行"),
    ("More", "更多"),
    ("Recent connections", "最近连接"),
    (
        "No connections from this PC yet.",
        "这台电脑还没有连接过它。",
    ),
    (
        "Sessions from this PC to your other devices are listed on their pages.",
        "这台电脑连接其他设备的记录，显示在那些设备的页面上。",
    ),
    ("Online: you can connect now.", "在线：现在就可以连接。"),
    (
        "Offline: a device of yours on its network can wake it.",
        "离线：它所在网络里你的另一台设备可以唤醒它。",
    ),
    (
        "This PC: your other devices can control it.",
        "本机：你的其他设备可以控制它。",
    ),
    (
        "Your devices appear here once this PC is logged in to your account, and you can control them from here.",
        "这台电脑登录你的账号后，你的设备会显示在这里，并可以从这里控制它们。",
    ),
    (
        "Your other devices connect to it from their own Queqiao.",
        "你的其他设备在它们自己的鹊桥里连接这台电脑。",
    ),
    ("Today {time}", "今天 {time}"),
    ("under a minute", "不到一分钟"),
    ("{minutes} min", "{minutes} 分钟"),
    ("{hours} h {minutes} min", "{hours} 小时 {minutes} 分钟"),
    ("Send files...", "发送文件……"),
    ("Send files to {name}", "发送文件到 {name}"),
    ("All files", "所有文件"),
    // The session window's bar and card of figures.
    ("Full screen", "全屏"),
    ("Figures", "数据"),
    ("Session figures", "会话数据"),
    ("Direct", "直连"),
    ("Relayed", "中继"),
    // The relay a session goes through, by its place (relay_place).
    ("Relay", "中继服务器"),
    ("Beijing", "北京"),
    ("Shanghai", "上海"),
    ("Guangzhou", "广州"),
    ("Shenzhen", "深圳"),
    ("Chengdu", "成都"),
    ("Chongqing", "重庆"),
    ("Hangzhou", "杭州"),
    ("Nanjing", "南京"),
    ("Wuhan", "武汉"),
    ("Xian", "西安"),
    ("Qingdao", "青岛"),
    ("Hong Kong", "香港"),
    ("Taipei", "台北"),
    ("Tokyo", "东京"),
    ("Seoul", "首尔"),
    ("Singapore", "新加坡"),
    ("Data", "码率"),
    ("Codec", "编码"),
    ("Picture", "画面"),
    ("Time", "时长"),
    // The session bar and its control center (2026-11-25).
    ("File transfer", "文件传输"),
    ("Control center", "控制中心"),
    ("Screen", "屏幕"),
    ("Next screen", "切换下一个屏幕"),
    ("Window", "窗口"),
    ("Sound", "声音"),
    ("Security", "安全"),
    ("Input", "外设"),
    ("Tools", "工具"),
    ("End session", "结束远控"),
    ("That PC's screens", "那台电脑的屏幕"),
    ("One screen", "一个屏幕"),
    ("Quality", "画质"),
    ("Fewer bits: for slow networks.", "码率更低：适合慢速网络。"),
    (
        "As many bits as the picture's size wants.",
        "按画面大小所需的码率。",
    ),
    ("Twice the bits: sharp text.", "两倍码率：文字清晰。"),
    (
        "Sharp coloured text; both PCs must handle it.",
        "彩色文字更清晰；两台电脑都须支持。",
    ),
    (
        "Smooth gradients; both PCs must handle it.",
        "渐变更平滑；两台电脑都须支持。",
    ),
    (
        "Off: that PC sends no sound at all.",
        "关闭后，那台电脑不再发送声音。",
    ),
    ("Volume here", "本机音量"),
    (
        "Lock that PC when the session ends",
        "远控结束后锁定那台电脑",
    ),
    (
        "Its screen locks as soon as you leave.",
        "你离开后，它立即锁屏。",
    ),
    (
        "People at that PC see a dark screen, and their keyboard and mouse do nothing; you still see everything.",
        "那台电脑前的人只看到黑屏，键盘和鼠标也不起作用；你仍能看到全部画面。",
    ),
    (
        "Its sound still comes to you; the room there stays quiet.",
        "你仍能听到它的声音，那边保持安静。",
    ),
    ("Lock that PC now", "立即锁定那台电脑"),
    ("Keyboard", "键盘"),
    (
        "Send system shortcuts to that PC",
        "发送系统快捷键到那台电脑",
    ),
    (
        "Win, Alt+Tab and the like go to that PC while this window is in front.",
        "此窗口在前台时，Win、Alt+Tab 等快捷键发送到那台电脑。",
    ),
    ("Mouse", "鼠标"),
    (
        "The pointer here takes the shape it has there.",
        "本机指针显示为那台电脑上的形状。",
    ),
    ("Game mouse", "游戏鼠标"),
    (
        "The pointer stays in the window and sends movement: for games that turn the view.",
        "指针留在窗口内，只发送移动量：适合转动视角的游戏。",
    ),
    ("Microphone", "麦克风"),
    (
        "Programs there hear this PC's microphone.",
        "那台电脑上的程序能听到本机的麦克风。",
    ),
    ("Open a command line", "打开命令行"),
    ("Show the session's figures", "显示网络状态"),
    (
        "Direct or relayed, delay, frames, data and codec, at the bottom right.",
        "在右下角显示直连或中继、延迟、帧率、码率和编码。",
    ),
    // The file transfer window (2026-11-25).
    (
        "Queqiao - file transfer with {name}",
        "鹊桥 - 与 {name} 传输文件",
    ),
    ("That PC", "那台电脑"),
    ("Send →", "发送 →"),
    ("← Send", "← 发送"),
    ("Back", "后退"),
    ("Up", "上一级"),
    ("Refresh", "刷新"),
    ("Name", "名称"),
    ("Modified", "修改日期"),
    ("Type", "类型"),
    ("Size", "大小"),
    ("Folder", "文件夹"),
    ("Drive", "磁盘"),
    ("This PC's places", "这台电脑的常用位置"),
    ("That PC's places", "那台电脑的常用位置"),
    ("Reading...", "正在读取……"),
    (
        "That PC did not answer: its Queqiao may be older than this one.",
        "那台电脑没有回应：它的鹊桥可能比这台的旧。",
    ),
    ("Status", "状态"),
    ("From", "发送路径"),
    ("To", "接收路径"),
    ("Pause all", "全部暂停"),
    ("Start all", "全部开始"),
    ("Cancel all", "全部取消"),
    ("Clear finished", "清除已完成"),
    ("Waiting", "等待中"),
    ("Paused", "已暂停"),
    ("Sent", "已发送"),
    ("Received", "已接收"),
    ("Failed", "失败"),
    ("Cancelled", "已取消"),
    ("{count} done", "已完成 {count} 项"),
    (
        "Nothing sent yet: check files and press Send, or drop files here.",
        "尚未传输：勾选文件后点“发送”，或把文件拖到这里。",
    ),
    (
        "Nothing sent yet: check files and press Send.",
        "尚未传输：勾选文件后点“发送”。",
    ),
    ("Downloads/Queqiao", "下载/Queqiao"),
    ("Show in folder", "在文件夹中显示"),
    ("Pause", "暂停"),
    ("Resume", "继续"),
    ("Remove from the list", "从列表中移除"),
    // Settings, as tabs of cards (settings.rs), and the display language.
    ("Same as the system", "跟随系统"),
    ("General", "常规"),
    ("When controlled", "被控"),
    ("Display language", "显示语言"),
    (
        "The language of Queqiao's windows on this PC.",
        "这台电脑上鹊桥各个窗口使用的语言。",
    ),
    (
        "Queqiao starts with this PC, so your devices can reach it.",
        "开机后自动运行鹊桥，你的设备随时可以连接这台电脑。",
    ),
    (
        "New versions are installed while no session runs.",
        "有新版本时，在没有远控进行时自动安装。",
    ),
    ("Version", "版本"),
    ("Queqiao {version}", "鹊桥 {version}"),
    ("Wake-on-LAN", "网络唤醒"),
    (
        "Whether your other devices can wake this PC when it sleeps or is off.",
        "检查你的其他设备能否在这台电脑睡眠或关机时把它唤醒。",
    ),
    ("Check...", "检查..."),
    ("Fixed help code", "固定协助码"),
    (
        "A code of your own that stays the same, instead of a new one each time.",
        "使用固定不变的协助码，而不是每次换一个新的。",
    ),
    ("Remove", "移除"),
    ("Set...", "设置..."),
    (
        "Your other devices, logged in to the same account, can connect to it.",
        "登录同一账号的其他设备可以连接这台电脑。",
    ),
    (
        "Someone at this PC allows or declines each session first.",
        "每次远控前，先由这台电脑前的人允许或拒绝。",
    ),
    (
        "How someone of another account starts helping",
        "他人远程协助本机的方式",
    ),
    (
        "With this PC's help ID and the code it shows.",
        "使用本机的协助 ID 和本机显示的验证码。",
    ),
    (
        "Files can go both ways while someone of another account helps.",
        "被他人远程协助时，可以双向传输文件。",
    ),
    ("Settings password", "设置密码"),
    (
        "These settings, logging out and quitting ask for it.",
        "更改这些设置、退出登录和退出鹊桥时需要输入它。",
    ),
    (
        "Copy on one PC and paste on the other, both ways.",
        "在一台电脑上复制，在另一台上粘贴，双向可用。",
    ),
    (
        "Files can be sent to this PC, and taken from it, in a session.",
        "远控中可以向这台电脑发送文件，也可以从它取文件。",
    ),
    (
        "Your devices can open a command line on this PC.",
        "你的设备可以在这台电脑上打开命令行。",
    ),
    (
        "This PC locks as soon as a session ends.",
        "远控一结束，这台电脑立即锁定。",
    ),
    ("Shortcuts of the session window", "远控窗口的快捷键"),
    (
        "Pressed in a session, these work on this PC and never go to the other PC.",
        "远控中按下时只在本机生效，不会发送到被控端。",
    ),
    ("Restore the default keys", "还原默认按键"),
    ("Full screen or back to a window", "远控窗口全屏/窗口化"),
    ("Game mouse on or off", "开启/关闭游戏鼠标"),
    (
        "Boss key: hide the session window and its sound at once",
        "老板键：立即隐藏远控窗口和声音",
    ),
    ("On", "开"),
    ("Off", "关"),
    ("Not set", "未设置"),
    ("Change", "修改"),
    // One device controls a PC at a time (as UU Remote, 2026-11-26).
    ("Being controlled", "被控中"),
    (
        "A device on your account is currently controlling this computer.",
        "当前有本账号设备正在控制此电脑。",
    ),
    ("Disconnect", "断开远控"),
    ("Take Control", "接管设备"),
    (
        "Taking control will disconnect {device}. Do you want to continue?",
        "接管设备后将断开 {device} 的远程控制。是否继续？",
    ),
    (
        "{device}, a device of your account, took over control of this PC.",
        "本账号设备 {device} 已接管此电脑。",
    ),
    (
        "{device}, a device of your account, ended this remote control session.",
        "本账号设备 {device} 已断开此次远程控制。",
    ),
    (
        "{controller} is controlling {device} now.",
        "{controller} 正在控制 {device}。",
    ),
    (
        "{device} is in a session with someone helping from another account.",
        "{device} 正在接受其他账号的远程协助。",
    ),
    (
        "{device} stays with the device controlling it.",
        "{device} 仍由当前控制它的设备控制。",
    ),
    // One login form for logging in and signing up (as UU Remote, 2026-11-26).
    ("Use another server...", "使用其他服务器..."),
    ("Server key (SHA-256)", "服务器密钥 (SHA-256)"),
    (
        "Log in, or sign up with a new user name",
        "登录，或使用新用户名注册",
    ),
    (
        "© 2026 GuoWQ222. All rights reserved.",
        "© 2026 GuoWQ222 版权所有",
    ),
    (
        "Please fill in the server and its key.",
        "请填写服务器及其密钥。",
    ),
    (
        "Please enter a user name and a password.",
        "请输入用户名和密码。",
    ),
    (
        "A new account \"{user}\" was made with this password, and you are logged in. If you meant an account you \
         already have, log out and check the user name.",
        "已使用此密码创建新账号“{user}”并登录。如果您要登录的是已有账号，请退出登录并检查用户名。",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relays_place_reads_as_words_and_in_chinese_where_known() {
        assert_eq!(relay_place_in("shanghai", Language::English), "Shanghai");
        assert_eq!(relay_place_in("hong-kong", Language::English), "Hong Kong");
        assert_eq!(relay_place_in("shanghai", Language::Chinese), "上海");
        assert_eq!(relay_place_in("hong_kong", Language::Chinese), "香港");
        // A place the table does not have stays as written; no name, nothing.
        assert_eq!(relay_place_in("frankfurt", Language::Chinese), "Frankfurt");
        assert_eq!(relay_place_in("", Language::English), "");
    }

    fn placeholders(text: &str) -> Vec<&str> {
        let mut found: Vec<&str> = text
            .split('{')
            .skip(1)
            .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
            .collect();
        found.sort_unstable();
        found
    }

    #[test]
    fn translations_are_unique_and_keep_their_placeholders() {
        for (index, (english, chinese)) in CHINESE.iter().enumerate() {
            assert!(
                CHINESE[..index].iter().all(|(other, _)| other != english),
                "translated twice: {english}"
            );
            assert_eq!(placeholders(english), placeholders(chinese), "{english}");
        }
    }

    #[test]
    fn the_language_choice_is_kept_as_a_word() {
        for choice in LanguageChoice::ALL {
            assert_eq!(LanguageChoice::from_word(choice.word()), choice);
            assert_eq!(
                LanguageChoice::from_word(&format!(
                    "{}
",
                    choice.word()
                )),
                choice
            );
        }
        assert_eq!(LanguageChoice::from_word("fr"), LanguageChoice::System);
        assert_eq!(LanguageChoice::English.label(), "English");
        assert_eq!(LanguageChoice::Chinese.label(), "简体中文");
    }

    #[test]
    fn texts_are_translated_and_filled() {
        assert_eq!(translate("Allow", Language::Chinese), "允许");
        assert_eq!(translate("Allow", Language::English), "Allow");
        // Without a translation: English.
        assert_eq!(translate("no such text", Language::Chinese), "no such text");
        assert_eq!(
            fill(
                translate("{viewer} is controlling this PC", Language::Chinese),
                &[("viewer", "LAPTOP")]
            ),
            "LAPTOP 正在控制这台电脑"
        );
    }
}
