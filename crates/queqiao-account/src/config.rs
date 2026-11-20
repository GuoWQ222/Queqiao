//! What a device keeps between starts, in `%APPDATA%\Queqiao`:
//! - `device.json`: the device's id and name, shared by its agent and viewer;
//! - `<role>.json`: the server, the pinned certificate fingerprint and the refresh token, which is
//!   encrypted with Windows' DPAPI for the current user (another user or PC cannot read it).

use std::path::PathBuf;

use queqiao_core::account::Role;
use queqiao_transport::Error;
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
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
}

impl DeviceIdentity {
    /// A device that exists only in memory (tests, probes): a new id from `label`, a new key.
    pub fn in_memory(label: &str) -> Result<DeviceIdentity, Error> {
        Ok(DeviceIdentity {
            id: format!("{label}-{}", random_hex(8)),
            name: label.to_string(),
            key: Some(DeviceKey::generate()?),
            in_memory: true,
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
            name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into()),
            key: None,
            in_memory: false,
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

/// The login of one program (agent or viewer) on this PC.
#[derive(Clone, Debug, Default)]
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

fn file_name(role: Role) -> &'static str {
    match role {
        Role::Agent => "agent.json",
        Role::Viewer => "viewer.json",
    }
}

pub(crate) fn folder() -> Result<PathBuf, Error> {
    let base = std::env::var_os("APPDATA").ok_or_else(|| Error("APPDATA is not set".into()))?;
    let folder = PathBuf::from(base).join("Queqiao");
    std::fs::create_dir_all(&folder)?;
    Ok(folder)
}

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
        assert_eq!(unhex(&hex(&[0, 255, 16])), Some(vec![0, 255, 16]));
        assert_eq!(random_hex(16).len(), 32);
    }
}
