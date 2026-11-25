//! The device for the sign-in screen: what the person's agent hands over (this device's id, name and key, the
//! account login, the pins) while the person lets the account's devices in without asking. Kept in
//! `%ProgramData%\Queqiao\logon.bin`, encrypted with DPAPI for SYSTEM (no other account can decrypt it) and in a
//! file only SYSTEM may open; given to the agent for the sign-in screen over its channel, in memory only.

use std::path::PathBuf;

use queqiao_local::{FromService, LogonIdentity};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};
use windows::core::PCWSTR;

use crate::launch::wide;

fn path() -> PathBuf {
    crate::log::folder().join("logon.bin")
}

pub fn stored() -> bool {
    path().is_file()
}

pub fn save(identity: &LogonIdentity) -> Result<(), String> {
    // The same encoding as on the channel.
    let plain = FromService::Identity(identity.clone()).encode();
    let sealed = protect(&plain)?;
    let file = path();
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    }
    // Made empty, closed to all but SYSTEM, then filled.
    std::fs::write(&file, b"").map_err(|e| format!("{}: {e}", file.display()))?;
    system_only(&file)?;
    std::fs::write(&file, sealed).map_err(|e| format!("{}: {e}", file.display()))
}

pub fn load() -> Option<LogonIdentity> {
    let sealed = std::fs::read(path()).ok()?;
    let plain = unprotect(&sealed).ok()?;
    match FromService::decode(&plain)? {
        FromService::Identity(identity) => Some(identity),
        _ => None,
    }
}

/// Whether the Windows user `offering` (a SID) may hand over or take back the device kept for the sign-in screen:
/// only the one who handed it over (any user when none is kept, or it was kept before owners were noted). The
/// security review of 2026-11-22: a second user of the PC could replace the owner's device with one of their own
/// account and so control the sign-in screen from it.
pub fn may_hand_over(kept_owner: Option<&str>, offering: &str) -> bool {
    match kept_owner {
        None | Some("") => true,
        Some(owner) => owner == offering,
    }
}

pub fn forget() {
    let _ = std::fs::remove_file(path());
}

/// Only SYSTEM may open the file (no inherited access).
fn system_only(file: &std::path::Path) -> Result<(), String> {
    let sddl = wide("D:P(A;;FA;;;SY)");
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(|e| format!("security descriptor: {e}"))?;
    let mut present = false.into();
    let mut defaulted = false.into();
    let mut dacl = std::ptr::null_mut();
    let result = unsafe {
        GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted).and_then(
            |()| {
                let name = wide(&file.display().to_string());
                SetNamedSecurityInfoW(
                    PCWSTR(name.as_ptr()),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    Some(dacl),
                    None,
                )
                .ok()
            },
        )
    };
    unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    result.map_err(|e| format!("securing {}: {e}", file.display()))
}

fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(|e| format!("CryptProtectData: {e}"))?;
    let sealed =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(Some(HLOCAL(output.pbData.cast()))) };
    Ok(sealed)
}

fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(|e| format!("CryptUnprotectData: {e}"))?;
    let plain =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(Some(HLOCAL(output.pbData.cast()))) };
    Ok(plain)
}

#[cfg(test)]
mod owner_tests {
    use super::may_hand_over;

    #[test]
    fn only_the_one_who_set_it_up_may_change_it() {
        let anna = "S-1-5-21-1-2-3-1001";
        let bert = "S-1-5-21-1-2-3-1002";
        assert!(may_hand_over(None, bert), "nothing kept: anyone");
        assert!(
            may_hand_over(Some(""), bert),
            "kept before owners were noted: anyone"
        );
        assert!(may_hand_over(Some(anna), anna));
        assert!(
            !may_hand_over(Some(anna), bert),
            "another user of the PC: refused"
        );
    }
}
