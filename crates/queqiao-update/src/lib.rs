//! Updates: a feed of two small files next to the installer of the newest release, `queqiao-update.json` (the
//! manifest: version, the installer's file name, size and SHA-256) and `queqiao-update.json.sig` (an Ed25519 signature
//! over the manifest's exact bytes, hex, by the release key). Every Queqiao build carries the release key's public
//! half ([`RELEASE_KEY`]); a manifest it does not verify is refused, and so is an installer whose size or SHA-256 is
//! not the manifest's. Only a later version than this build's is taken, and only from a manifest published no earlier
//! than the newest one this PC has seen (an old feed served again cannot hold back a later release once seen).
//!
//! The feed is the latest GitHub release's files ([`FEED`]); tests point the agent at their own (HTTPS, or HTTP on
//! this PC only). Downloads use WinHTTP: the system's proxy settings and certificate store, redirects followed but
//! never from HTTPS to HTTP, limited in size and time (each step, and the whole download).

#[cfg(windows)]
mod fetch;
pub mod release;

#[cfg(windows)]
pub use fetch::fetch;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The release key's public half (Ed25519, hex). Its private half stays with whoever publishes releases, never in
/// this repository. For now a test key, made on the build PC with `queqiao-setup release-key` (2026-11-22).
pub const RELEASE_KEY: &str = "bdc28c264efc9010627d737039537d129f763dd85f5339abb232bd9fadd742aa";

/// Where the newest release's files are: GitHub serves `releases/latest/download/<file>` from the latest release.
pub const FEED: &str = "https://github.com/GuoWQ222/Queqiao/releases/latest/download/";
pub const MANIFEST: &str = "queqiao-update.json";
pub const SIGNATURE: &str = "queqiao-update.json.sig";
/// The most an installer may be.
pub const MAX_INSTALLER: u64 = 256 << 20;
/// The most a manifest or signature may be.
// The limits of the downloads: Linux's downloader comes with self-update there (docs/LINUX.md step 6).
#[cfg_attr(not(windows), allow(dead_code))]
const MAX_SMALL: u64 = 64 * 1024;
/// How long fetching a manifest or signature, and an installer, may take in all (the security review of 2026-11-22:
/// a feed that sent a byte now and then held the update thread for ever).
#[cfg_attr(not(windows), allow(dead_code))]
const SMALL_TIME: std::time::Duration = std::time::Duration::from_secs(60);
#[cfg_attr(not(windows), allow(dead_code))]
const INSTALLER_TIME: std::time::Duration = std::time::Duration::from_secs(30 * 60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// major.minor.patch
    pub version: String,
    /// The installer's file name in the feed: `queqiao-setup-<version>.exe`.
    pub installer: String,
    pub size: u64,
    /// SHA-256 of the installer, hex (lower case).
    pub sha256: String,
    /// What changed, shown to the person (plain text).
    #[serde(default)]
    pub notes: String,
    /// When it was signed (Unix seconds; 0 in manifests from before this field).
    #[serde(default)]
    pub published: u64,
}

/// major.minor.patch as numbers; None for anything else.
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.');
    let mut number = || -> Option<u32> {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let version = (number()?, number()?, number()?);
    parts.next().is_none().then_some(version)
}

/// Whether `candidate` is a later version than `current` (both major.minor.patch).
pub fn newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

fn is_hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The manifest, if `signature` (hex) is `key`'s (hex) over `manifest`'s exact bytes and the manifest is well formed.
pub fn verify_manifest_with(
    key: &str,
    manifest: &[u8],
    signature: &str,
) -> Result<Manifest, String> {
    let signature = unhex(signature.trim()).ok_or("the update signature is not hex")?;
    let key = unhex(key)
        .filter(|key| key.len() == 32)
        .ok_or("no release key")?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(manifest, &signature)
        .map_err(|_| "the update is not signed by the release key: refused")?;
    let manifest: Manifest =
        serde_json::from_slice(manifest).map_err(|e| format!("the update manifest: {e}"))?;
    if parse_version(&manifest.version).is_none() {
        return Err(format!(
            "the update's version is not major.minor.patch: {}",
            manifest.version
        ));
    }
    if manifest.installer != format!("queqiao-setup-{}.exe", manifest.version) {
        return Err(format!(
            "the update's installer is not queqiao-setup-{}.exe",
            manifest.version
        ));
    }
    if manifest.size == 0 || manifest.size > MAX_INSTALLER {
        return Err(format!(
            "the update's installer size is out of range: {}",
            manifest.size
        ));
    }
    if !is_hex(&manifest.sha256, 64) {
        return Err("the update's SHA-256 is not 64 lower-case hex digits".into());
    }
    Ok(manifest)
}

/// [`verify_manifest_with`] the release key this build carries.
pub fn verify_manifest(manifest: &[u8], signature: &str) -> Result<Manifest, String> {
    verify_manifest_with(RELEASE_KEY, manifest, signature)
}

/// Whether `installer` is the one `manifest` names (size and SHA-256).
pub fn check_installer(manifest: &Manifest, installer: &[u8]) -> Result<(), String> {
    if installer.len() as u64 != manifest.size {
        return Err(format!(
            "the downloaded installer is {} bytes, the update says {}",
            installer.len(),
            manifest.size
        ));
    }
    if hex(&Sha256::digest(installer)) != manifest.sha256 {
        return Err("the downloaded installer's SHA-256 is not the update's: refused".into());
    }
    Ok(())
}

/// A feed address: HTTPS, or HTTP to this PC only (tests); ends with '/'.
pub fn check_feed(feed: &str) -> Result<(), String> {
    let lower = feed.to_ascii_lowercase();
    let local = [
        "http://127.0.0.1:",
        "http://127.0.0.1/",
        "http://localhost:",
        "http://localhost/",
    ];
    if !(lower.starts_with("https://") || local.iter().any(|prefix| lower.starts_with(prefix))) {
        return Err(format!(
            "an update feed must be HTTPS (or HTTP on this PC): {feed}"
        ));
    }
    if !feed.ends_with('/') {
        return Err(format!("an update feed address ends with /: {feed}"));
    }
    Ok(())
}

/// How long a version that was tried and put back is not tried again. Then once more: the person may only have quit
/// Queqiao while the new version was watched (the security review of 2026-11-22: a failed update was never retried).
pub const RETRY_AFTER: u64 = 7 * 24 * 3600;

/// Seconds since 1970.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The note kept of a version tried: the version and the time (Unix seconds), a line each.
pub fn tried_note(version: &str, at: u64) -> String {
    format!("{version}\r\n{at}\r\n")
}

/// Whether `note` says `version` was tried less than RETRY_AFTER before `now`.
pub fn tried_recently(note: &str, version: &str, now: u64) -> bool {
    let mut lines = note.lines();
    let tried = lines.next().map(str::trim);
    let at = lines.next().and_then(|at| at.trim().parse::<u64>().ok());
    tried == Some(version) && at.is_some_and(|at| now.saturating_sub(at) < RETRY_AFTER)
}

/// What the feed has: a later version, if any, and when its manifest was published.
#[derive(Clone, Debug)]
pub struct Checked {
    pub offer: Option<Offer>,
    pub published: u64,
}

/// A later version on the feed: the manifest as verified, and its exact bytes and signature (the service checks
/// them again itself).
#[derive(Clone, Debug)]
pub struct Offer {
    pub manifest: Manifest,
    pub raw: Vec<u8>,
    pub signature: String,
}

/// Refuses a manifest published before `newest_seen` (the newest manifest this PC has seen): an old feed served again
/// (the security review of 2026-11-22: a signed manifest never expired, so any old one could be replayed).
pub fn check_fresh(manifest: &Manifest, newest_seen: u64) -> Result<(), String> {
    if manifest.published < newest_seen {
        return Err(format!(
            "the feed's update ({}) is older than one this PC has seen: refused",
            manifest.version
        ));
    }
    Ok(())
}

/// What the feed offers, if it is later than `current`, verified, and published no earlier than `newest_seen`.
#[cfg(windows)]
pub fn check(feed: &str, current: &str, newest_seen: u64) -> Result<Checked, String> {
    check_feed(feed)?;
    let raw = fetch(&format!("{feed}{MANIFEST}"), MAX_SMALL, SMALL_TIME)?;
    let signature = fetch(&format!("{feed}{SIGNATURE}"), MAX_SMALL, SMALL_TIME)?;
    let signature = String::from_utf8(signature)
        .map_err(|_| "the update signature is not text")?
        .trim()
        .to_string();
    let manifest = verify_manifest(&raw, &signature)?;
    check_fresh(&manifest, newest_seen)?;
    let published = manifest.published;
    let offer = newer(&manifest.version, current).then_some(Offer {
        manifest,
        raw,
        signature,
    });
    Ok(Checked { offer, published })
}

/// Downloads the installer `manifest` names into `folder` and checks it; its path.
#[cfg(windows)]
pub fn download(
    feed: &str,
    manifest: &Manifest,
    folder: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    check_feed(feed)?;
    let bytes = fetch(
        &format!("{feed}{}", manifest.installer),
        manifest.size,
        INSTALLER_TIME,
    )?;
    check_installer(manifest, &bytes)?;
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(&manifest.installer);
    std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    fn key() -> (Ed25519KeyPair, String) {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(document.as_ref()).unwrap();
        let public = hex(pair.public_key().as_ref());
        (pair, public)
    }

    fn manifest(installer: &[u8]) -> Manifest {
        Manifest {
            version: "1.2.3".into(),
            installer: "queqiao-setup-1.2.3.exe".into(),
            size: installer.len() as u64,
            sha256: hex(&Sha256::digest(installer)),
            notes: "test".into(),
            published: 1_800_000_000,
        }
    }

    #[test]
    fn a_version_tried_is_tried_again_after_a_while() {
        let note = tried_note("1.2.3", 1_000_000);
        assert!(tried_recently(&note, "1.2.3", 1_000_000 + 3600));
        assert!(!tried_recently(&note, "1.2.4", 1_000_000 + 3600));
        assert!(!tried_recently(&note, "1.2.3", 1_000_000 + RETRY_AFTER));
        assert!(
            !tried_recently("1.2.3", "1.2.3", 1_000_000),
            "no time: not counted"
        );
    }

    #[test]
    fn an_older_manifest_than_one_seen_is_refused() {
        let seen = manifest(b"MZ");
        assert!(check_fresh(&seen, seen.published).is_ok());
        assert!(check_fresh(&seen, seen.published - 1).is_ok());
        assert!(check_fresh(&seen, seen.published + 1).is_err());
        // From before the field: fine until a newer one was seen.
        let old = Manifest {
            published: 0,
            ..seen
        };
        assert!(check_fresh(&old, 0).is_ok());
        assert!(check_fresh(&old, 1).is_err());
    }

    #[test]
    fn versions() {
        assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
        assert!(newer("0.1.1", "0.1.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
        for bad in [
            "", "1", "1.2", "1.2.3.4", "1.2.x", "1.-2.3", "v1.2.3", "1.2.3 ", "1..3",
        ] {
            assert_eq!(parse_version(bad), None, "{bad:?}");
            assert!(!newer(bad, "0.0.0"), "{bad:?}");
        }
    }

    #[test]
    fn only_manifests_signed_by_the_key() {
        let (pair, public) = key();
        let installer = b"MZ an installer".to_vec();
        let bytes = serde_json::to_vec(&manifest(&installer)).unwrap();
        let signature = hex(pair.sign(&bytes).as_ref());
        let verified = verify_manifest_with(&public, &bytes, &signature).unwrap();
        assert_eq!(verified, manifest(&installer));
        assert!(check_installer(&verified, &installer).is_ok());
        // Another key, a changed byte, a cut signature: refused.
        let (_, other) = key();
        assert!(verify_manifest_with(&other, &bytes, &signature).is_err());
        let mut changed = bytes.clone();
        let at = changed.iter().position(|b| *b == b'3').unwrap();
        changed[at] = b'4';
        assert!(verify_manifest_with(&public, &changed, &signature).is_err());
        assert!(verify_manifest_with(&public, &bytes, &signature[..64]).is_err());
        assert!(verify_manifest_with(&public, &bytes, "zz").is_err());
        // The build's own key is a placeholder until a release key exists: nothing verifies against it.
        if RELEASE_KEY.bytes().all(|b| b == b'0') {
            assert!(verify_manifest(&bytes, &signature).is_err());
        }
        // A different installer: refused.
        assert!(check_installer(&verified, b"MZ another one!").is_err());
        assert!(check_installer(&verified, b"MZ").is_err());
    }

    #[test]
    fn signed_but_malformed_manifests_are_refused() {
        let (pair, public) = key();
        let good = manifest(b"MZ");
        let cases = [
            Manifest {
                version: "1.2".into(),
                ..good.clone()
            },
            Manifest {
                installer: "..\\evil.exe".into(),
                ..good.clone()
            },
            Manifest {
                installer: "queqiao-setup-9.9.9.exe".into(),
                ..good.clone()
            },
            Manifest {
                size: 0,
                ..good.clone()
            },
            Manifest {
                size: MAX_INSTALLER + 1,
                ..good.clone()
            },
            Manifest {
                sha256: "ABC".into(),
                ..good.clone()
            },
            Manifest {
                sha256: "G".repeat(64),
                ..good.clone()
            },
        ];
        for case in cases {
            let bytes = serde_json::to_vec(&case).unwrap();
            let signature = hex(pair.sign(&bytes).as_ref());
            assert!(
                verify_manifest_with(&public, &bytes, &signature).is_err(),
                "{case:?}"
            );
        }
    }

    #[test]
    fn feeds_are_https_or_this_pc() {
        assert!(check_feed(FEED).is_ok());
        assert!(check_feed("http://127.0.0.1:8080/feed/").is_ok());
        assert!(check_feed("http://localhost/").is_ok());
        for bad in [
            "http://example.com/",
            "http://127.0.0.1.example.com/",
            "ftp://x/",
            "https://x/feed",
            "file:///C:/feed/",
        ] {
            assert!(check_feed(bad).is_err(), "{bad}");
        }
    }
}
