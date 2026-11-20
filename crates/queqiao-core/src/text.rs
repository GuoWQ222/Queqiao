//! The words people see, in their language: English, or Simplified Chinese when Windows' display language is
//! Chinese. `QUEQIAO_LANG=en` or `zh` decides instead. Reports and logs stay English (tests read them).
//!
//! A text is looked up by its English wording, so code reads as English; one without a translation shows in
//! English. `{name}` marks a value filled in by [`trf`].

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    English,
    Chinese,
}

pub fn language() -> Language {
    static LANGUAGE: OnceLock<Language> = OnceLock::new();
    *LANGUAGE.get_or_init(|| match std::env::var("QUEQIAO_LANG").ok().as_deref() {
        Some("en") => Language::English,
        Some("zh") => Language::Chinese,
        _ => system_language(),
    })
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

#[cfg(not(windows))]
fn system_language() -> Language {
    Language::English
}

/// `english` in the user's language.
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
    ("Quit Queqiao on this PC", "退出这台电脑上的鹊桥"),
    (
        "This PC is being used remotely with Queqiao. Its screen stays dark until the session ends.",
        "这台电脑正在通过鹊桥被远程使用。会话结束前屏幕将保持黑屏。",
    ),
    // The session window.
    ("Send Ctrl+Alt+Del", "发送 Ctrl+Alt+Del"),
    ("Lock that PC", "锁定那台电脑"),
    ("Restart that PC...", "重启那台电脑……"),
    ("Play that PC's sound", "播放那台电脑的声音"),
    ("Darken that PC's screen", "让那台电脑黑屏（隐私屏）"),
    ("Open a command line on that PC", "在那台电脑上打开命令行"),
    ("Full screen (Ctrl+Alt+Enter)", "全屏（Ctrl+Alt+Enter）"),
    ("Game mouse (Ctrl+Alt+M)", "游戏鼠标（Ctrl+Alt+M）"),
    ("Frame rate", "帧率"),
    ("Picture quality", "画质"),
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
    ("Wake up", "唤醒"),
    ("Select a device first.", "请先选择一台设备。"),
    ("Queqiao - help someone", "鹊桥 - 帮助别人"),
    (
        "Ask the person you help for the help ID and code that Queqiao shows them (tray menu: Let someone help me...).",
        "请向你要帮助的人索取鹊桥显示的帮助 ID 和验证码（托盘菜单：让别人帮助我……）。",
    ),
    ("Help ID", "帮助 ID"),
    ("Code", "验证码"),
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
];

#[cfg(test)]
mod tests {
    use super::*;

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
