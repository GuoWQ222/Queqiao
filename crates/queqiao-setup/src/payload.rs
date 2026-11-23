//! The files an installer carries: a resource of its own program (RCDATA 101), so they are part of the image that
//! Windows maps (and the signature covers), not bytes appended to the file. Read from the mapped image, they are
//! exactly what Windows checked when the installer started, also as an administrator (UAC) or as SYSTEM: renaming
//! or replacing the installer's file afterwards changes nothing. (Until the security review of 2026-11-22 the files
//! were appended and the installer read its own file again, a moment in which another program could swap it.)
//!
//! The resource: for each file the name's length (u16), the name (UTF-8: a plain file name), the size (u64), its
//! SHA-256 (32 bytes), the bytes. Each file's SHA-256 is checked again when it is read.

use sha2::{Digest, Sha256};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    BeginUpdateResourceW, EndUpdateResourceW, FindResourceW, LoadResource, LockResource,
    SizeofResource, UpdateResourceW,
};
use windows::core::PCWSTR;

/// RT_RCDATA (MAKEINTRESOURCE(10)): raw data.
const RT_RCDATA: PCWSTR = PCWSTR(10 as *const u16);

/// The resource's id (of type RT_RCDATA).
const RESOURCE: u16 = 101;
/// The most a carried file may be (Queqiao's programs are a few MB each).
const MAX_FILE: u64 = 256 << 20;

pub struct Entry {
    pub name: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({} bytes)", self.name, self.bytes.len())
    }
}

/// A plain file name: letters, digits, dot, dash and underscore, not starting with a dot, at most 64 bytes.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

/// `entries` as the resource's bytes.
pub fn encode(entries: &[Entry]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for entry in entries {
        if !valid_name(&entry.name) {
            return Err(format!("not a plain file name: {}", entry.name));
        }
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(entry.name.as_bytes());
        out.extend_from_slice(&(entry.bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(&Sha256::digest(&entry.bytes));
        out.extend_from_slice(&entry.bytes);
    }
    Ok(out)
}

/// The files in the resource's bytes, each checked against its SHA-256.
pub fn decode(data: &[u8]) -> Result<Vec<Entry>, String> {
    if data.is_empty() {
        return Err("this installer carries no files".into());
    }
    let mut rest = data;
    let mut entries = Vec::new();
    while !rest.is_empty() {
        let take = |rest: &mut &[u8], count: usize| -> Result<Vec<u8>, String> {
            if rest.len() < count {
                return Err("the installer's files are cut short".into());
            }
            let (head, tail) = rest.split_at(count);
            *rest = tail;
            Ok(head.to_vec())
        };
        let name_length = u16::from_le_bytes(take(&mut rest, 2)?.try_into().unwrap_or_default());
        let name = String::from_utf8(take(&mut rest, usize::from(name_length))?)
            .map_err(|_| "a file name is not UTF-8")?;
        if !valid_name(&name) {
            return Err(format!("not a plain file name: {name}"));
        }
        let size = u64::from_le_bytes(take(&mut rest, 8)?.try_into().unwrap_or_default());
        if size > MAX_FILE {
            return Err(format!("{name} is too large"));
        }
        let hash = take(&mut rest, 32)?;
        let bytes = take(&mut rest, size as usize)?;
        if Sha256::digest(&bytes).as_slice() != hash.as_slice() {
            return Err(format!("{name} is damaged (its SHA-256 does not match)"));
        }
        if entries
            .iter()
            .any(|entry: &Entry| entry.name.eq_ignore_ascii_case(&name))
        {
            return Err(format!("{name} twice"));
        }
        entries.push(Entry { name, bytes });
    }
    Ok(entries)
}

/// The files this installer carries, from its own mapped image.
pub fn own() -> Result<Vec<Entry>, String> {
    decode(resource(None)?)
}

/// The resource's bytes in `module` (None: this program's own image). They stay mapped while the module is loaded.
fn resource<'a>(module: Option<HMODULE>) -> Result<&'a [u8], String> {
    let name = PCWSTR(usize::from(RESOURCE) as *const u16);
    let found = unsafe { FindResourceW(module, name, RT_RCDATA) };
    if found.is_invalid() {
        return Err("this installer carries no files".into());
    }
    let size = unsafe { SizeofResource(module, found) } as usize;
    let loaded = unsafe { LoadResource(module, found) }
        .map_err(|e| format!("the installer's files: {}", e.message()))?;
    let data = unsafe { LockResource(loaded) } as *const u8;
    if data.is_null() {
        return Err("the installer's files could not be read".into());
    }
    Ok(unsafe { std::slice::from_raw_parts(data, size) })
}

/// Writes `program` (an installer without files, or with files, which are replaced) to `output` with `entries` as
/// its resource. Returns the size of `output`.
pub fn pack(
    program: &std::path::Path,
    output: &std::path::Path,
    entries: &[Entry],
) -> Result<u64, String> {
    let data = encode(entries)?;
    std::fs::copy(program, output).map_err(|e| format!("{}: {e}", output.display()))?;
    let path: Vec<u16> = output
        .display()
        .to_string()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let update = unsafe { BeginUpdateResourceW(PCWSTR(path.as_ptr()), false) }
        .map_err(|e| format!("{}: {}", output.display(), e.message()))?;
    let name = PCWSTR(usize::from(RESOURCE) as *const u16);
    let written = unsafe {
        UpdateResourceW(
            update,
            RT_RCDATA,
            name,
            0,
            Some(data.as_ptr().cast()),
            data.len() as u32,
        )
    };
    let ended = unsafe { EndUpdateResourceW(update, written.is_err()) };
    written.map_err(|e| format!("the files into {}: {}", output.display(), e.message()))?;
    ended.map_err(|e| format!("writing {}: {}", output.display(), e.message()))?;
    std::fs::metadata(output)
        .map(|meta| meta.len())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::FreeLibrary;
    use windows::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE, LoadLibraryExW,
    };

    fn entries() -> Vec<Entry> {
        vec![
            Entry {
                name: "queqiao-agent.exe".into(),
                bytes: vec![1, 2, 3, 4],
            },
            Entry {
                name: "queqiao-viewer.exe".into(),
                bytes: (0..=255).collect(),
            },
        ]
    }

    #[test]
    fn encoded_files_come_back_and_damage_is_refused() {
        let data = encode(&entries()).unwrap();
        let read_back = decode(&data).unwrap();
        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[1].bytes, (0..=255).collect::<Vec<u8>>());
        // One byte of a file changed: refused.
        let mut damaged = data.clone();
        damaged[2 + "queqiao-agent.exe".len() + 8 + 32] ^= 1;
        assert!(decode(&damaged).unwrap_err().contains("damaged"));
        // Cut short, or nothing at all.
        assert!(decode(&data[..data.len() - 3]).is_err());
        assert!(decode(&[]).is_err());
    }

    #[test]
    fn packed_into_a_program_as_its_resource() {
        // This test's own program stands in for the installer.
        let program = std::env::current_exe().unwrap();
        let output =
            std::env::temp_dir().join(format!("queqiao-pack-test-{}.exe", std::process::id()));
        pack(&program, &output, &entries()).unwrap();
        // Packing again replaces the files instead of adding to them.
        pack(
            &output.clone(),
            &output.with_extension("2.exe"),
            &entries()[..1],
        )
        .unwrap();
        for (path, count) in [(output.clone(), 2), (output.with_extension("2.exe"), 1)] {
            let wide: Vec<u16> = path
                .display()
                .to_string()
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let module = unsafe {
                LoadLibraryExW(
                    PCWSTR(wide.as_ptr()),
                    None,
                    LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
                )
            }
            .unwrap();
            let read_back = decode(resource(Some(module)).unwrap()).unwrap();
            assert_eq!(read_back.len(), count);
            assert_eq!(read_back[0].bytes, vec![1, 2, 3, 4]);
            let _ = unsafe { FreeLibrary(module) };
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn only_plain_file_names() {
        // LICENSE and THIRD_PARTY_NOTICES.md: packed beside the programs (scripts\package.ps1).
        for name in [
            "queqiao-agent.exe",
            "README.txt",
            "a_b-c.dll",
            "LICENSE",
            "THIRD_PARTY_NOTICES.md",
        ] {
            assert!(valid_name(name), "{name}");
        }
        for name in [
            "",
            "..\\evil.exe",
            "../evil.exe",
            "dir/x.exe",
            "C:x.exe",
            ".hidden",
            "a b.exe",
        ] {
            assert!(!valid_name(name), "{name}");
        }
        let bad = [Entry {
            name: "..\\evil.exe".into(),
            bytes: vec![0],
        }];
        assert!(encode(&bad).is_err());
    }
}
