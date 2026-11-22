//! The device's own Ed25519 key, and the keys of the account's other devices as this PC first saw them.
//!
//! - `device_key.json` in `%APPDATA%\Queqiao`: the private key (PKCS#8), DPAPI-encrypted for the Windows user,
//!   shared by the device's agent and viewer.
//! - `known_devices.json`: device id -> public key, pinned on first contact (as SSH does with host keys). A
//!   device whose key later differs is refused until the pin is removed on purpose (`forget`): the server, or
//!   someone who took it over, cannot slip in its own key. The agent and the viewer share it: each reads and
//!   changes it under `known_devices.lock` (pins kept in memory and written whole were lost: the last one to
//!   write dropped the other's new pins, which the next contact then pinned again, from the server's word).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use queqiao_transport::Error;
use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

use crate::config::{folder, hex, protect, unhex, unprotect};
use crate::tls::err;

#[derive(Clone)]
pub struct DeviceKey {
    pair: Arc<Ed25519KeyPair>,
    /// The private key as stored (PKCS#8), for the service's agent at the sign-in screen.
    pkcs8: Arc<Vec<u8>>,
    public_hex: String,
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeviceKey({})", self.public_hex)
    }
}

impl DeviceKey {
    /// This PC's key: made once, then reused.
    pub fn load_or_create() -> Result<DeviceKey, Error> {
        let path = folder()?.join("device_key.json");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let sealed: String = serde_json::from_str(&text).map_err(err)?;
            let pkcs8 =
                unprotect(&unhex(&sealed).ok_or_else(|| Error("damaged device key file".into()))?)?;
            return DeviceKey::from_pkcs8(&pkcs8);
        }
        let pkcs8 = generate_pkcs8()?;
        let key = DeviceKey::from_pkcs8(&pkcs8)?;
        let sealed = hex(&protect(&pkcs8)?);
        // The agent and the viewer may start together: the first file written wins.
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(serde_json::to_string(&sealed)?.as_bytes())
            });
        match created {
            Ok(()) => Ok(key),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                DeviceKey::load_or_create()
            }
            Err(error) => Err(error.into()),
        }
    }

    /// A key that lives only in memory (tests, probes).
    pub fn generate() -> Result<DeviceKey, Error> {
        DeviceKey::from_pkcs8(&generate_pkcs8()?)
    }

    /// A key from its PKCS#8 bytes (the agent at the sign-in screen gets them from the service).
    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<DeviceKey, Error> {
        let pair =
            Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|e| Error(format!("device key: {e}")))?;
        let public_hex = hex(pair.public_key().as_ref());
        Ok(DeviceKey {
            pair: Arc::new(pair),
            pkcs8: Arc::new(pkcs8.to_vec()),
            public_hex,
        })
    }

    /// The private key (PKCS#8): only for handing this device to the Queqiao service.
    pub fn pkcs8(&self) -> &[u8] {
        &self.pkcs8
    }

    /// The public key, hex (what the server and the other devices see).
    pub fn public_hex(&self) -> &str {
        &self.public_hex
    }

    pub fn sign(&self, message: &[u8]) -> String {
        hex(self.pair.sign(message).as_ref())
    }
}

fn generate_pkcs8() -> Result<Vec<u8>, Error> {
    let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|e| Error(format!("device key: {e}")))?;
    Ok(document.as_ref().to_vec())
}

/// True if `signature` (hex) is `public_key`'s (hex) signature over `message`.
pub fn verify(public_key: &str, message: &[u8], signature: &str) -> bool {
    match (unhex(public_key), unhex(signature)) {
        (Some(key), Some(signature)) => UnparsedPublicKey::new(&ED25519, key)
            .verify(message, &signature)
            .is_ok(),
        _ => false,
    }
}

/// The keys of other devices as first seen from this PC.
pub struct KnownDevices {
    /// As last read or written here (the file is what counts: the other program may have changed it).
    keys: BTreeMap<String, String>,
    /// None: in memory only (tests, probes).
    path: Option<PathBuf>,
}

/// What a device's key is compared with its pin.
#[derive(Debug, PartialEq, Eq)]
pub enum Pin {
    /// Seen for the first time: now pinned.
    New,
    Same,
    /// Different from the pinned key (hex): refused.
    Changed(String),
}

impl KnownDevices {
    pub fn load() -> Result<KnownDevices, Error> {
        Ok(KnownDevices::at(folder()?.join("known_devices.json")))
    }

    /// The pins in `path` (another folder: tests).
    fn at(path: PathBuf) -> KnownDevices {
        let keys = {
            let _lock = lock(&path);
            read_pins(&path).unwrap_or_default()
        };
        KnownDevices {
            keys,
            path: Some(path),
        }
    }

    pub fn in_memory() -> KnownDevices {
        KnownDevices::with_pins(Vec::new())
    }

    /// In memory, starting from these pins (device id, public key hex).
    pub fn with_pins(pins: Vec<(String, String)>) -> KnownDevices {
        KnownDevices {
            keys: pins
                .into_iter()
                .map(|(device, key)| (device, key.to_ascii_lowercase()))
                .collect(),
            path: None,
        }
    }

    /// All pins (device id, public key hex), as the file has them now.
    pub fn pins(&self) -> Vec<(String, String)> {
        let keys = match &self.path {
            Some(path) => {
                let _lock = lock(path);
                read_pins(path).unwrap_or_else(|| self.keys.clone())
            }
            None => self.keys.clone(),
        };
        keys.into_iter().collect()
    }

    /// Compares `public_key` with the pin for `device`, pinning it the first time.
    pub fn check(&mut self, device: &str, public_key: &str) -> Pin {
        let key = public_key.to_ascii_lowercase();
        // Not written (the folder refused it): it holds for this contact, the next one pins again.
        let (pin, _written) = self.change(|keys| match keys.get(device) {
            Some(pinned) if *pinned == key => (Pin::Same, false),
            Some(pinned) => (Pin::Changed(pinned.clone()), false),
            None => {
                keys.insert(device.to_string(), key);
                (Pin::New, true)
            }
        });
        pin
    }

    /// Removes the pin of `device` (the person confirmed its new key); the next contact pins again.
    pub fn forget(&mut self, device: &str) -> Result<(), Error> {
        self.change(|keys| ((), keys.remove(device).is_some())).1
    }

    /// Applies `edit` (which says whether it changed anything) to the pins as the file has them now, and
    /// writes them back if it did, all under the file's lock: the other program's pins and removals stay.
    fn change<R>(
        &mut self,
        edit: impl FnOnce(&mut BTreeMap<String, String>) -> (R, bool),
    ) -> (R, Result<(), Error>) {
        let Some(path) = self.path.clone() else {
            return (edit(&mut self.keys).0, Ok(()));
        };
        let _lock = lock(&path);
        // Missing or damaged: what this program knows.
        if let Some(keys) = read_pins(&path) {
            self.keys = keys;
        }
        let (result, changed) = edit(&mut self.keys);
        let written = if changed {
            write_pins(&path, &self.keys)
        } else {
            Ok(())
        };
        (result, written)
    }
}

/// The pin file's lock, held while the value lives (released when its handle closes). None if the folder refused
/// the lock file: then unlocked, as before the lock.
fn lock(path: &Path) -> Option<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("lock"))
        .ok()?;
    file.lock().ok()?;
    Some(file)
}

fn read_pins(path: &Path) -> Option<BTreeMap<String, String>> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn write_pins(path: &Path, keys: &BTreeMap<String, String>) -> Result<(), Error> {
    // Written whole to a temporary file and renamed: a half-written pin file would lose pins.
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_string_pretty(keys).map_err(err)?)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_and_pins() {
        let key = DeviceKey::generate().unwrap();
        let other = DeviceKey::generate().unwrap();
        let signature = key.sign(b"offer");
        assert!(verify(key.public_hex(), b"offer", &signature));
        assert!(!verify(
            key.public_hex(),
            b"offer with another fingerprint",
            &signature
        ));
        assert!(!verify(other.public_hex(), b"offer", &signature));
        assert!(!verify(key.public_hex(), b"offer", "00"));

        let mut known = KnownDevices::in_memory();
        assert_eq!(known.check("dev", key.public_hex()), Pin::New);
        assert_eq!(
            known.check("dev", &key.public_hex().to_uppercase()),
            Pin::Same
        );
        assert_eq!(
            known.check("dev", other.public_hex()),
            Pin::Changed(key.public_hex().to_string())
        );
        known.forget("dev").unwrap();
        assert_eq!(known.check("dev", other.public_hex()), Pin::New);
    }

    /// The agent and the viewer of one PC: both loaded before either pinned anything.
    #[test]
    fn two_programs_keep_each_others_pins() {
        let folder = std::env::temp_dir().join(format!("queqiao_pins_{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("known_devices.json");
        let _ = std::fs::remove_file(&path);
        let (one, two, three) = ("1".repeat(64), "2".repeat(64), "3".repeat(64));
        let mut agent = KnownDevices::at(path.clone());
        let mut viewer = KnownDevices::at(path.clone());
        assert_eq!(agent.check("a", &one), Pin::New);
        assert_eq!(viewer.check("b", &two), Pin::New);
        // The viewer sees the agent's pin: another key for "a" is refused there too.
        assert_eq!(viewer.check("a", &three), Pin::Changed(one.clone()));
        let pins = KnownDevices::at(path.clone()).pins();
        assert_eq!(
            pins,
            vec![
                ("a".to_string(), one.clone()),
                ("b".to_string(), two.clone())
            ]
        );
        // A pin removed in one stays removed when the other pins next.
        agent.forget("a").unwrap();
        assert_eq!(viewer.check("c", &three), Pin::New);
        assert_eq!(agent.pins(), viewer.pins());
        assert_eq!(viewer.check("a", &three), Pin::New);
        assert_eq!(agent.check("a", &one), Pin::Changed(three.clone()));

        // Four programs pinning at once: all their pins are in the file.
        std::fs::remove_file(&path).unwrap();
        let threads: Vec<_> = (0..4)
            .map(|program| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut known = KnownDevices::at(path);
                    for device in 0..20 {
                        let key = format!("{program:02}{device:062}");
                        assert_eq!(known.check(&format!("{program}-{device}"), &key), Pin::New);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(KnownDevices::at(path.clone()).pins().len(), 80);
        let _ = std::fs::remove_dir_all(&folder);
    }
}
