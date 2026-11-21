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
    ("Full screen (Ctrl+Alt+Enter)", "全屏（Ctrl+Alt+Enter）"),
    ("Game mouse (Ctrl+Alt+M)", "游戏鼠标（Ctrl+Alt+M）"),
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
    ("Command lines...", "命令行……"),
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
