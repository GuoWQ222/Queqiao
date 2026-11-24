//! Shared code for all Queqiao programs.
//!
//! The wire protocol between viewer and agent ([`protocol`]), the version every program prints at
//! start-up, and the test switch that forces fallback paths. Planned (docs/DESIGN.md): identity.

pub mod account;
pub mod invitation;
#[cfg(windows)]
pub mod platform;
pub mod protocol;
pub mod stun;
pub mod text;

/// Version of the messages exchanged between agent, viewer and server.
/// Peers with a different version refuse to connect.
pub const PROTOCOL_VERSION: u32 = 1;

/// This build's version: the workspace's, or QUEQIAO_VERSION when it was set at build time (an update test builds
/// a later version of the same code that way).
pub const VERSION: &str = match option_env!("QUEQIAO_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// One-line start-up banner: program name, workspace version, protocol version.
pub fn banner(program: &str) -> String {
    format!("{program} {VERSION} (protocol {PROTOCOL_VERSION})")
}

/// Test switch for fallback paths, so every path can run on any machine: the environment variable
/// `QUEQIAO_FORCE` holds `stage:choice` pairs separated by commas, for example
/// `encode:software,adapter:warp`. Returns the forced choice for `stage` (lower case), if any.
/// The programs' `--force` option ([`set_forced`]) takes precedence over the variable.
pub fn forced(stage: &str) -> Option<String> {
    let spec = match FORCED.get() {
        Some(spec) => spec.clone(),
        None => std::env::var("QUEQIAO_FORCE").ok()?,
    };
    forced_in(&spec, stage)
}

static FORCED: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets this process's test switches (same form as `QUEQIAO_FORCE`); the first call wins.
pub fn set_forced(spec: &str) {
    let _ = FORCED.set(spec.to_string());
}

/// This PC's local time now, to the minute, as the logs and lists show it ("2026-11-23 15:04").
pub fn local_time_minutes() -> String {
    #[cfg(windows)]
    {
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute
        )
    }
    #[cfg(unix)]
    {
        let now: libc::time_t = unsafe { libc::time(std::ptr::null_mut()) };
        let mut t: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&now, &mut t) }.is_null() {
            return String::new();
        }
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            t.tm_year + 1900,
            t.tm_mon + 1,
            t.tm_mday,
            t.tm_hour,
            t.tm_min
        )
    }
}

fn forced_in(spec: &str, stage: &str) -> Option<String> {
    spec.split(',')
        .filter_map(|item| item.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(stage))
        .map(|(_, choice)| choice.trim().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_names_program_and_versions() {
        let text = banner("queqiao-agent");
        assert!(text.starts_with("queqiao-agent "));
        assert!(text.ends_with(&format!(" (protocol {PROTOCOL_VERSION})")));
    }

    #[test]
    fn forced_reads_stage_choices() {
        let spec = "encode:Software, adapter:warp";
        assert_eq!(forced_in(spec, "encode").as_deref(), Some("software"));
        assert_eq!(forced_in(spec, "ADAPTER").as_deref(), Some("warp"));
        assert_eq!(forced_in(spec, "capture"), None);
        assert_eq!(forced_in("", "encode"), None);
    }

    #[test]
    fn local_time_has_the_list_form() {
        let text = local_time_minutes();
        let bytes = text.as_bytes();
        assert_eq!(bytes.len(), 16, "{text}");
        assert!(text.starts_with("20"), "{text}");
        for (index, byte) in bytes.iter().enumerate() {
            let want = match index {
                4 | 7 => b'-',
                10 => b' ',
                13 => b':',
                _ => b'0',
            };
            if want == b'0' {
                assert!(byte.is_ascii_digit(), "{text}");
            } else {
                assert_eq!(*byte, want, "{text}");
            }
        }
    }
}
