//! The viewer's Lock and Restart commands on Linux (docs/LINUX.md), as power.rs does on Windows: locking asks logind
//! (the desktop's own screen locker answers, as Windows' LockWorkStation), else the freedesktop screensaver; restarting
//! asks systemd for a reboot after the same delay as Windows' (polkit decides whether this user may).

use std::process::{Command, Stdio};

/// Seconds between the command and the restart (the viewer is told; as on Windows).
pub const RESTART_DELAY: u32 = 10;

pub fn lock() -> Result<(), String> {
    let logind = std::env::var("XDG_SESSION_ID")
        .ok()
        .map(|session| run("loginctl", &["lock-session", &session]));
    match logind {
        Some(Ok(())) => Ok(()),
        // Without logind (or a session of its own): the screensaver of the desktop.
        _ => run("xdg-screensaver", &["lock"]).map_err(|error| match logind {
            Some(Err(first)) => format!("{first}; {error}"),
            _ => error,
        }),
    }
}

pub fn restart() -> Result<(), String> {
    // A reboot in RESTART_DELAY seconds, so the notice reaches the viewer first: systemd-run starts the timer as a
    // transient unit of this user's manager, which outlives the agent.
    run(
        "systemd-run",
        &[
            "--user",
            &format!("--on-active={RESTART_DELAY}"),
            "systemctl",
            "reboot",
        ],
    )
}

/// Runs `program` with `args`: Ok when it ran and succeeded.
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program}: {} {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
