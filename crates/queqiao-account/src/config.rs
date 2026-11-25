//! What a device keeps between starts, in `%APPDATA%\Queqiao` (Linux: `$XDG_CONFIG_HOME/queqiao`, usually
//! `~/.config/queqiao`):
//! - `device.json`: the device's id and name, shared by its agent and viewer;
//! - `<role>.json`: the server, the pinned certificate fingerprint and the refresh token, which is
//!   encrypted with Windows' DPAPI for the current user (another user or PC cannot read it). On Linux with this
//!   person's own key (`protect.key`, readable by them only) and ChaCha20-Poly1305; the Secret Service (GNOME
//!   Keyring, KWallet) is to hold that key where there is one (docs/LINUX.md).

use std::path::PathBuf;

use queqiao_core::account::Role;
use queqiao_transport::Error;
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use windows::Win32::Foundation::{HLOCAL, LocalFree};
#[cfg(windows)]
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
#[cfg(windows)]
use windows::core::PCWSTR;

use crate::keys::DeviceKey;
use crate::tls::err;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub id: String,
    pub name: String,
    /// The device's own key (kept in its own DPAPI-protected file, not in device.json).
    #[serde(skip)]
    pub key: Option<DeviceKey>,
    /// Made for a test or probe: its key and its pins of other devices live in memory only.
    #[serde(skip)]
    pub in_memory: bool,
    /// For a device in memory: the pins it starts with (the agent at the sign-in screen gets the person's).
    #[serde(skip)]
    pub preset_pins: Option<Vec<(String, String)>>,
}

impl DeviceIdentity {
    /// A device that exists only in memory (tests, probes): a new id from `label`, a new key.
    pub fn in_memory(label: &str) -> Result<DeviceIdentity, Error> {
        Ok(DeviceIdentity {
            id: format!("{label}-{}", random_hex(8)),
            name: label.to_string(),
            key: Some(DeviceKey::generate()?),
            in_memory: true,
            preset_pins: None,
        })
    }

    /// This device as handed over by the Queqiao service (the agent at the sign-in screen): all in memory.
    pub fn handed_over(
        id: &str,
        name: &str,
        pkcs8: &[u8],
        pins: Vec<(String, String)>,
    ) -> Result<DeviceIdentity, Error> {
        Ok(DeviceIdentity {
            id: id.to_string(),
            name: name.to_string(),
            key: Some(DeviceKey::from_pkcs8(pkcs8)?),
            in_memory: true,
            preset_pins: Some(pins),
        })
    }

    /// This PC's identity: made once (random id, the computer's name), then reused; with its key.
    pub fn load_or_create() -> Result<DeviceIdentity, Error> {
        let mut identity = DeviceIdentity::load_or_create_file()?;
        identity.key = Some(DeviceKey::load_or_create()?);
        Ok(identity)
    }

    fn load_or_create_file() -> Result<DeviceIdentity, Error> {
        let path = folder()?.join("device.json");
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(identity) = serde_json::from_str::<DeviceIdentity>(&text)
        {
            return Ok(identity);
        }
        let identity = DeviceIdentity {
            id: random_hex(16),
            name: this_pc_name(),
            key: None,
            in_memory: false,
            preset_pins: None,
        };
        // The agent and the viewer may start at the same moment: the first file written wins.
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(serde_json::to_string_pretty(&identity)?.as_bytes())
            });
        match created {
            Ok(()) => Ok(identity),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let text = std::fs::read_to_string(&path)?;
                serde_json::from_str(&text).map_err(err)
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// The account server a new installation's login form offers, and its certificate's fingerprint: given when Queqiao
/// is built for a server (QUEQIAO_DEFAULT_SERVER as host:port, QUEQIAO_DEFAULT_PIN), else empty (typed in the form).
/// The person can still type another server there.
pub const DEFAULT_SERVER: &str = match option_env!("QUEQIAO_DEFAULT_SERVER") {
    Some(server) => server,
    None => "",
};
pub const DEFAULT_PIN: &str = match option_env!("QUEQIAO_DEFAULT_PIN") {
    Some(pin) => pin,
    None => "",
};

/// The login of one program (agent or viewer) on this PC.
#[derive(Clone, Debug)]
pub struct SavedLogin {
    /// host:port of queqiao-server.
    pub server: String,
    /// SHA-256 of the server's certificate (hex).
    pub pin: String,
    pub refresh_token: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct StoredLogin {
    server: String,
    pin: String,
    /// The refresh token, DPAPI-encrypted, hex.
    refresh_token: Option<String>,
}

/// No login yet: the server Queqiao was built for, if any.
impl Default for SavedLogin {
    fn default() -> Self {
        SavedLogin {
            server: DEFAULT_SERVER.to_string(),
            pin: DEFAULT_PIN.to_string(),
            refresh_token: None,
        }
    }
}

impl SavedLogin {
    pub fn load(role: Role) -> Option<SavedLogin> {
        let text = std::fs::read_to_string(folder().ok()?.join(file_name(role))).ok()?;
        let stored: StoredLogin = serde_json::from_str(&text).ok()?;
        let refresh_token = stored
            .refresh_token
            .and_then(|hex| unhex(&hex))
            .and_then(|sealed| unprotect(&sealed).ok())
            .and_then(|plain| String::from_utf8(plain).ok());
        Some(SavedLogin {
            server: stored.server,
            pin: stored.pin,
            refresh_token,
        })
    }

    pub fn save(&self, role: Role) -> Result<(), Error> {
        let refresh_token = match &self.refresh_token {
            Some(token) => Some(hex(&protect(token.as_bytes())?)),
            None => None,
        };
        let stored = StoredLogin {
            server: self.server.clone(),
            pin: self.pin.clone(),
            refresh_token,
        };
        let path = folder()?.join(file_name(role));
        std::fs::write(path, serde_json::to_string_pretty(&stored).map_err(err)?)?;
        Ok(())
    }
}

/// The saved login of each program. A test build (the agent's and viewer's test-switches) keeps its own, so tests on a
/// PC never touch the login of the person there: they did, and the login form then offered a test server and its
/// fingerprint (2026-11-23).
fn file_name(role: Role) -> &'static str {
    match (role, cfg!(feature = "test-files")) {
        (Role::Agent, false) => "agent.json",
        (Role::Viewer, false) => "viewer.json",
        (Role::Agent, true) => "agent.test.json",
        (Role::Viewer, true) => "viewer.test.json",
    }
}

/// This PC's name as the account lists it: Windows' computer name.
#[cfg(windows)]
fn this_pc_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into())
}

/// This PC's name as the account lists it: the host name (the kernel's, as `hostname` prints it).
#[cfg(unix)]
fn this_pc_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|name| name.trim().to_string())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Linux PC".into())
}
/// `%APPDATA%\Queqiao`, where this device keeps its files.
#[cfg(windows)]
pub fn folder() -> Result<PathBuf, Error> {
    let base = std::env::var_os("APPDATA").ok_or_else(|| Error("APPDATA is not set".into()))?;
    let folder = PathBuf::from(base).join("Queqiao");
    std::fs::create_dir_all(&folder)?;
    Ok(folder)
}

/// `$XDG_CONFIG_HOME/queqiao` (`~/.config/queqiao`), where this device keeps its files: made for this person only
/// (0700), as `%APPDATA%` is.
#[cfg(unix)]
pub fn folder() -> Result<PathBuf, Error> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| Error("neither XDG_CONFIG_HOME nor HOME is set".into()))?;
    let folder = base.join("queqiao");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)?;
    // Made earlier by something else with wider rights: this person's only.
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))?;
    Ok(folder)
}

/// Sealed data on Linux: "QQS1", a 12-byte nonce, then ChaCha20-Poly1305 of the data with this person's key.
#[cfg(unix)]
const SEALED: &[u8; 4] = b"QQS1";

/// This person's key for `protect` on Linux: 32 random bytes in `protect.key` (0600 in the 0700 folder), made the
/// first time. Another user cannot read it; root and a copy of the home folder can, which DPAPI's tie to the
/// Windows password avoids: the Secret Service is to hold it where there is one (docs/LINUX.md).
#[cfg(unix)]
fn protect_key() -> Result<ring::aead::LessSafeKey, Error> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = folder()?.join("protect.key");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) if bytes.len() == 32 => bytes,
        Ok(_) => return Err(Error(format!("{} is not a key", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut fresh = vec![0u8; 32];
            getrandom::fill(&mut fresh).map_err(|e| Error(format!("random: {e}")))?;
            // create_new: two programs starting at once do not both write one; the second reads the first's.
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(mut file) => {
                    file.write_all(&fresh)?;
                    file.sync_all()?;
                    fresh
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::fs::read(&path)?
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    };
    let key = ring::aead::UnboundKey::new(&ring::aead::CHACHA20_POLY1305, &bytes)
        .map_err(|_| Error(format!("{} is not a key", path.display())))?;
    Ok(ring::aead::LessSafeKey::new(key))
}

#[cfg(unix)]
pub(crate) fn protect(data: &[u8]) -> Result<Vec<u8>, Error> {
    let key = protect_key()?;
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).map_err(|e| Error(format!("random: {e}")))?;
    let mut sealed = data.to_vec();
    key.seal_in_place_append_tag(
        ring::aead::Nonce::assume_unique_for_key(nonce),
        ring::aead::Aad::from(SEALED),
        &mut sealed,
    )
    .map_err(|_| Error("sealing failed".into()))?;
    let mut out = SEALED.to_vec();
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

#[cfg(unix)]
pub(crate) fn unprotect(data: &[u8]) -> Result<Vec<u8>, Error> {
    let rest = data
        .strip_prefix(SEALED)
        .filter(|rest| rest.len() >= 12 + 16)
        .ok_or_else(|| Error("not sealed data".into()))?;
    let (nonce, sealed) = rest.split_at(12);
    let nonce = ring::aead::Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| Error("not sealed data".into()))?;
    let mut opened = sealed.to_vec();
    let plain = protect_key()?
        .open_in_place(nonce, ring::aead::Aad::from(SEALED), &mut opened)
        .map_err(|_| Error("sealed with another key, or changed".into()))?;
    Ok(plain.to_vec())
}

#[cfg(windows)]
pub(crate) fn protect(data: &[u8]) -> Result<Vec<u8>, Error> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr().cast_mut(),
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
    .map_err(|e| Error(format!("DPAPI: {}", e.message())))?;
    Ok(take_blob(output))
}

#[cfg(windows)]
pub(crate) fn unprotect(data: &[u8]) -> Result<Vec<u8>, Error> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr().cast_mut(),
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
    .map_err(|e| Error(format!("DPAPI: {}", e.message())))?;
    Ok(take_blob(output))
}

/// Copies a blob that Windows allocated and frees it.
#[cfg(windows)]
fn take_blob(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let bytes = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
    unsafe { LocalFree(Some(HLOCAL(blob.pbData.cast()))) };
    bytes
}

pub(crate) fn random_hex(count: usize) -> String {
    let mut bytes = vec![0u8; count];
    getrandom::fill(&mut bytes).expect("the system random number generator");
    hex(&bytes)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpapi_round_trip_and_hex() {
        let sealed = protect(b"refresh token").unwrap();
        assert_ne!(sealed, b"refresh token");
        assert_eq!(unprotect(&sealed).unwrap(), b"refresh token");
        // Changed or cut short: refused, not opened.
        let mut changed = sealed.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert!(unprotect(&changed).is_err());
        assert!(unprotect(&sealed[..sealed.len() / 2]).is_err());
        assert_eq!(unhex(&hex(&[0, 255, 16])), Some(vec![0, 255, 16]));
        assert_eq!(random_hex(16).len(), 32);
    }
}
