//! A Linux account's password, checked by PAM as the system's own sign-in and lock screens check it (docs/LINUX.md):
//! the lock screen's gate and the sign-in screen (as UU, a viewer gives that PC's password: "this does not unlock
//! it"). libpam is loaded at run time. The PAM service is Queqiao's own (`/etc/pam.d/queqiao`, which the package puts
//! there: the system's common authentication and account checks), else `login`'s.
//!
//! Run as root (the Queqiao service), PAM checks any account; run as a person, pam_unix checks only that person's own
//! password (through unix_chkpwd), which is all an agent asks.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_SILENT: c_int = 0x8000;
const PAM_DISALLOW_NULL_AUTHTOK: c_int = 0x1;

#[repr(C)]
struct Message {
    style: c_int,
    text: *const c_char,
}

#[repr(C)]
struct Response {
    text: *mut c_char,
    code: c_int,
}

type Conversation = unsafe extern "C" fn(
    count: c_int,
    messages: *mut *const Message,
    responses: *mut *mut Response,
    data: *mut c_void,
) -> c_int;

#[repr(C)]
struct Conv {
    conversation: Conversation,
    data: *mut c_void,
}

type Start =
    unsafe extern "C" fn(*const c_char, *const c_char, *const Conv, *mut *mut c_void) -> c_int;
type Call = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;

/// What the conversation answers: the account's name when PAM asks for it, its password when PAM asks unseen.
struct Answers {
    user: CString,
    password: CString,
}

unsafe extern "C" fn converse(
    count: c_int,
    messages: *mut *const Message,
    responses: *mut *mut Response,
    data: *mut c_void,
) -> c_int {
    const PAM_CONV_ERR: c_int = 19;
    if count <= 0 || count > 32 || messages.is_null() || responses.is_null() || data.is_null() {
        return PAM_CONV_ERR;
    }
    let answers = unsafe { &*data.cast::<Answers>() };
    // PAM frees the array and each text with free(): both from calloc / strdup.
    let array = unsafe { libc::calloc(count as usize, size_of::<Response>()) }.cast::<Response>();
    if array.is_null() {
        return PAM_CONV_ERR;
    }
    for index in 0..count as usize {
        // Linux-PAM passes an array of pointers to messages.
        let message = unsafe { *messages.add(index) };
        let style = if message.is_null() {
            0
        } else {
            unsafe { (*message).style }
        };
        let text = match style {
            PAM_PROMPT_ECHO_OFF => unsafe { libc::strdup(answers.password.as_ptr()) },
            PAM_PROMPT_ECHO_ON => unsafe { libc::strdup(answers.user.as_ptr()) },
            _ => std::ptr::null_mut(),
        };
        unsafe {
            (*array.add(index)).text = text;
            (*array.add(index)).code = 0;
        }
    }
    unsafe { *responses = array };
    PAM_SUCCESS
}

/// libpam's functions, loaded once.
struct Pam {
    start: Start,
    authenticate: Call,
    account: Call,
    end: Call,
}

fn pam() -> Result<&'static Pam, String> {
    static PAM: std::sync::OnceLock<Result<Pam, String>> = std::sync::OnceLock::new();
    PAM.get_or_init(|| {
        let library =
            unsafe { libc::dlopen(c"libpam.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if library.is_null() {
            return Err("libpam.so.0 is not on this system".into());
        }
        let symbol = |name: &CStr| {
            let found = unsafe { libc::dlsym(library, name.as_ptr()) };
            (!found.is_null())
                .then_some(found)
                .ok_or_else(|| format!("libpam has no {}", name.to_string_lossy()))
        };
        Ok(unsafe {
            Pam {
                start: std::mem::transmute::<*mut c_void, Start>(symbol(c"pam_start")?),
                authenticate: std::mem::transmute::<*mut c_void, Call>(symbol(
                    c"pam_authenticate",
                )?),
                account: std::mem::transmute::<*mut c_void, Call>(symbol(c"pam_acct_mgmt")?),
                end: std::mem::transmute::<*mut c_void, Call>(symbol(c"pam_end")?),
            }
        })
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// The PAM service: Queqiao's own where the package put it, else `login`'s.
fn service() -> &'static CStr {
    if std::path::Path::new("/etc/pam.d/queqiao").is_file()
        || std::path::Path::new("/usr/lib/pam.d/queqiao").is_file()
    {
        c"queqiao"
    } else {
        c"login"
    }
}

/// Whether `password` is `user`'s, and the account may sign in now (not locked or expired). Blocks while PAM works
/// (a wrong password can take a few seconds: PAM's own delay).
pub fn password_ok(user: &str, password: &str) -> Result<bool, String> {
    let pam = pam()?;
    let (Ok(user), Ok(password)) = (CString::new(user), CString::new(password)) else {
        return Ok(false);
    };
    if password.as_bytes().is_empty() {
        return Ok(false);
    }
    let mut answers = Answers { user, password };
    let conversation = Conv {
        conversation: converse,
        data: (&raw mut answers).cast(),
    };
    let mut handle = std::ptr::null_mut();
    let started = unsafe {
        (pam.start)(
            service().as_ptr(),
            answers.user.as_ptr(),
            &conversation,
            &mut handle,
        )
    };
    if started != PAM_SUCCESS || handle.is_null() {
        return Err(format!("pam_start: {started}"));
    }
    let mut status = unsafe { (pam.authenticate)(handle, PAM_SILENT | PAM_DISALLOW_NULL_AUTHTOK) };
    if status == PAM_SUCCESS {
        status = unsafe { (pam.account)(handle, PAM_SILENT) };
    }
    unsafe { (pam.end)(handle, status) };
    Ok(status == PAM_SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_or_empty_password_is_wrong() {
        // No account has these; PAM itself answers (or is missing, which also opens nothing).
        assert!(!password_ok("queqiao-no-such-user", "wrong").unwrap_or(false));
        assert!(!password_ok("root", "").unwrap_or(false));
        assert!(!password_ok("root\0x", "a").unwrap_or(false));
    }
}
