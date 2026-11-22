//! The lock screen, as UU Remote handles it: while this PC is locked, a viewer sees and operates nothing of it until
//! it gives this PC's Windows password ("verify that it is you; this does not unlock it"), with a few tries. Someone
//! helping from another account is never asked (the person at the PC unlocks it).
//!
//! The password is checked by Windows itself (LogonUserW, for the account signed in here); a few tries per session and
//! per PC, so that guessing never reaches Windows' own lockout of the account.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Wrong passwords a session may give.
pub const SESSION_TRIES: u32 = 5;
/// Wrong passwords for this PC in all sessions, in FAILURE_WINDOW.
const PC_TRIES: usize = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);

/// When wrong passwords were given here (all sessions of this program).
static FAILURES: Mutex<Vec<Instant>> = Mutex::new(Vec::new());
/// The agent for the sign-in screen: the Windows account ("DOMAIN\user") of the one who set it up.
static SIGN_IN_OWNER: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The agent for the sign-in screen: whose Windows password it asks for.
pub fn set_sign_in_owner(account: &str) {
    let _ = SIGN_IN_OWNER.set(account.to_string());
}

pub struct LockGate {
    /// Tests: locked from the start (nothing is really locked); the sign-in screen: always.
    simulated: bool,
    /// The sign-in screen (nobody signed in): the account whose password opens it (empty: not known, nothing does).
    owner: Option<String>,
    engaged: bool,
    verified: bool,
    told: bool,
    wrong: u32,
    checked: Option<Instant>,
}

impl LockGate {
    /// The sign-in screen with nobody signed in: closed from the start, opened by the password of the one who set it
    /// up.
    pub fn signed_out() -> LockGate {
        LockGate {
            owner: Some(SIGN_IN_OWNER.get().cloned().unwrap_or_default()),
            ..LockGate::new(true)
        }
    }

    pub fn signed_out_mode(&self) -> bool {
        self.owner.is_some()
    }

    pub fn new(simulated: bool) -> LockGate {
        LockGate {
            owner: None,
            simulated,
            engaged: false,
            verified: false,
            told: false,
            wrong: 0,
            checked: None,
        }
    }

    /// Looks whether this PC is locked (twice a second at most). True when the gate has just closed: the viewer is
    /// to be asked for the password now.
    pub fn refresh(&mut self) -> bool {
        if self
            .checked
            .is_some_and(|at| at.elapsed() < Duration::from_millis(500))
        {
            return false;
        }
        self.checked = Some(Instant::now());
        let engaged = self.simulated || session_locked().unwrap_or(false);
        if engaged != self.engaged {
            self.engaged = engaged;
            self.told = false;
        }
        let ask = self.closed() && !self.told;
        if ask {
            self.told = true;
        }
        ask
    }

    /// The picture and input wait (locked, and no password given in this session yet).
    pub fn closed(&self) -> bool {
        self.engaged && !self.verified
    }

    /// Tries left for this session (fewer when this PC had wrong passwords lately).
    pub fn tries_left(&self) -> u8 {
        // At a sign-in screen set up before owners were noted: no password can be checked.
        if self.owner.as_deref() == Some("") {
            return 0;
        }
        let session = SESSION_TRIES.saturating_sub(self.wrong) as usize;
        let pc = PC_TRIES.saturating_sub(recent_failures());
        session.min(pc) as u8
    }

    /// A password from the viewer: right (the gate opens for this session) or wrong (one try less). `expected` is the
    /// tests' password instead of Windows' check.
    pub fn check(&mut self, password: &str, expected: Option<&str>) -> bool {
        if self.tries_left() == 0 {
            return false;
        }
        let right = match expected {
            Some(test) => password == test,
            None => windows_password_ok(password, self.owner.as_deref()),
        };
        if right {
            self.verified = true;
        } else {
            self.wrong += 1;
            if let Ok(mut failures) = FAILURES.lock() {
                failures.push(Instant::now());
            }
        }
        right
    }
}

fn recent_failures() -> usize {
    FAILURES.lock().map_or(PC_TRIES, |mut failures| {
        failures.retain(|at| at.elapsed() < FAILURE_WINDOW);
        failures.len()
    })
}

/// Whether this program's Windows session is locked (WTS session flags); None when Windows does not say.
fn session_locked() -> Option<bool> {
    use windows::Win32::System::RemoteDesktop::{
        WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTSFreeMemory, WTSINFOEXW,
        WTSQuerySessionInformationW, WTSSessionInfoEx,
    };
    use windows::core::PWSTR;
    // WTS_SESSIONSTATE_LOCK and WTS_SESSIONSTATE_UNLOCK (wtsapi32.h).
    const LOCK: i32 = 0;
    const UNLOCK: i32 = 1;
    let mut buffer = PWSTR::null();
    let mut bytes = 0u32;
    unsafe {
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buffer,
            &mut bytes,
        )
    }
    .ok()?;
    let flags = (bytes as usize >= size_of::<WTSINFOEXW>() && !buffer.is_null()).then(|| {
        let info = unsafe { &*(buffer.0 as *const WTSINFOEXW) };
        unsafe { info.Data.WTSInfoExLevel1.SessionFlags }
    });
    unsafe { WTSFreeMemory(buffer.0.cast()) };
    match flags? {
        LOCK => Some(true),
        UNLOCK => Some(false),
        _ => None,
    }
}

/// Whether `password` is the Windows password of `account` ("DOMAIN\user"), or of the account signed in here
/// (Windows checks it).
fn windows_password_ok(password: &str, account: Option<&str>) -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::Authentication::Identity::{GetUserNameExW, NameSamCompatible};
    use windows::Win32::Security::{
        LOGON32_LOGON_INTERACTIVE, LOGON32_LOGON_NETWORK, LOGON32_PROVIDER_DEFAULT, LogonUserW,
    };
    use windows::core::{PCWSTR, PWSTR};
    let account = match account {
        Some(account) => account.to_string(),
        None => {
            let mut buffer = vec![0u16; 512];
            let mut size = buffer.len() as u32;
            if !unsafe {
                GetUserNameExW(
                    NameSamCompatible,
                    Some(PWSTR(buffer.as_mut_ptr())),
                    &mut size,
                )
            } {
                return false;
            }
            String::from_utf16_lossy(&buffer[..size as usize])
        }
    };
    let (domain, user) = account.split_once('\\').unwrap_or((".", account.as_str()));
    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (user, domain, password) = (wide(user), wide(domain), wide(password));
    // A network logon first (no profile loaded); an interactive one for accounts that only allow that.
    [LOGON32_LOGON_NETWORK, LOGON32_LOGON_INTERACTIVE]
        .into_iter()
        .any(|kind| {
            let mut token = HANDLE::default();
            let ok = unsafe {
                LogonUserW(
                    PCWSTR(user.as_ptr()),
                    PCWSTR(domain.as_ptr()),
                    PCWSTR(password.as_ptr()),
                    kind,
                    LOGON32_PROVIDER_DEFAULT,
                    &mut token,
                )
            }
            .is_ok();
            if ok {
                let _ = unsafe { CloseHandle(token) };
            }
            ok
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_opens_with_the_right_password_and_counts_wrong_ones() {
        let mut gate = LockGate::new(true);
        assert!(
            gate.refresh(),
            "a simulated lock closes the gate and asks at once"
        );
        assert!(gate.closed());
        assert!(!gate.refresh(), "asked once, not again");
        assert_eq!(gate.tries_left(), 5);
        assert!(!gate.check("wrong", Some("right")));
        assert_eq!(gate.tries_left(), 4);
        assert!(gate.check("right", Some("right")));
        assert!(!gate.closed(), "open for the rest of the session");
        // Wrong ones count for this PC too: a new session has fewer tries.
        let mut other = LockGate::new(true);
        for _ in 0..3 {
            assert!(!other.check("wrong", Some("right")));
        }
        let fresh = LockGate::new(true);
        assert_eq!(
            fresh.tries_left(),
            1,
            "4 wrong in all: one left for this PC"
        );
        assert!(other.check("right", Some("right")));
    }

    #[test]
    fn the_sign_in_screen_needs_a_known_owner() {
        // Set up before owners were noted: nothing opens it.
        let gate = LockGate {
            owner: Some(String::new()),
            ..LockGate::new(true)
        };
        assert!(gate.signed_out_mode());
        assert_eq!(gate.tries_left(), 0);
    }
}
