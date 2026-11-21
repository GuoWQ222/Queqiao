//! The files an installer carries, appended to its own program: the program, then the entries, then a footer.
//! Entry: the name's length (u16), the name (UTF-8: a plain file name), the size (u64), its SHA-256 (32 bytes), the
//! bytes. Footer: "QQSETUP1", where the entries start (u64), how long they are (u64). The installer is signed after
//! the files are appended, so its signature covers them; each file's SHA-256 is checked again before it is written.

use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"QQSETUP1";
const FOOTER: usize = 8 + 8 + 8;
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

/// `program` (an installer without files, or with files, which are dropped) with `entries` appended.
pub fn pack(program: &[u8], entries: &[Entry]) -> Result<Vec<u8>, String> {
    let base = &program[..program_length(program)];
    let mut out = base.to_vec();
    let start = out.len() as u64;
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
    let length = out.len() as u64 - start;
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&length.to_le_bytes());
    Ok(out)
}

/// Where the program ends and the files begin (the whole length without files or a signature).
fn program_length(data: &[u8]) -> usize {
    let data = without_signature(data);
    match footer(data) {
        Some((start, _)) => start,
        None => data.len(),
    }
}

/// The footer, at the end or before up to 7 zero bytes (signing pads the file to 8 bytes before the signature).
fn footer(data: &[u8]) -> Option<(usize, usize)> {
    (0..8).find_map(|padding| {
        let end = data.len().checked_sub(padding)?;
        if data[end..].iter().any(|byte| *byte != 0) {
            return None;
        }
        let tail = end.checked_sub(FOOTER)?;
        if &data[tail..tail + 8] != MAGIC {
            return None;
        }
        let start = u64::from_le_bytes(data[tail + 8..tail + 16].try_into().ok()?) as usize;
        let length = u64::from_le_bytes(data[tail + 16..tail + 24].try_into().ok()?) as usize;
        (start.checked_add(length)? == tail).then_some((start, length))
    })
}

/// The files in an installer, each checked against its SHA-256.
pub fn read(data: &[u8]) -> Result<Vec<Entry>, String> {
    let data = without_signature(data);
    let (start, length) = footer(data).ok_or("this installer carries no files")?;
    let mut rest = &data[start..start + length];
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

/// The file without an Authenticode signature at its end: the PE header's certificate table says where it is
/// (signing appends it after everything else, so the footer is just before it).
fn without_signature(data: &[u8]) -> &[u8] {
    let pe = data
        .get(0x3C..0x40)
        .and_then(|b| b.try_into().ok())
        .map(|b: [u8; 4]| u32::from_le_bytes(b) as usize);
    let Some(pe) = pe.filter(|pe| data.get(*pe..pe + 4) == Some(b"PE\0\0")) else {
        return data;
    };
    let optional = pe + 24;
    let magic = data
        .get(optional..optional + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]));
    // The data directories start at 96 (PE32) or 112 (PE32+) into the optional header; the certificate table is the
    // fifth (index 4), 8 bytes each.
    let directories = match magic {
        Some(0x10b) => optional + 96,
        Some(0x20b) => optional + 112,
        _ => return data,
    };
    let entry = directories + 4 * 8;
    let Some(table) = data.get(entry..entry + 8) else {
        return data;
    };
    let offset = u32::from_le_bytes(table[0..4].try_into().unwrap_or_default()) as usize;
    let size = u32::from_le_bytes(table[4..8].try_into().unwrap_or_default()) as usize;
    if offset == 0 || size == 0 || offset.checked_add(size) != Some(data.len()) {
        return data;
    }
    &data[..offset]
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn packed_files_come_back_and_damage_is_refused() {
        let program = b"MZ fake program".to_vec();
        let packed = pack(&program, &entries()).unwrap();
        let read_back = read(&packed).unwrap();
        assert_eq!(read_back.len(), 2);
        assert_eq!(read_back[1].bytes, (0..=255).collect::<Vec<u8>>());
        // Packing again replaces the files instead of adding to them.
        let again = pack(&packed, &entries()[..1]).unwrap();
        assert_eq!(read(&again).unwrap().len(), 1);
        assert_eq!(&again[..program.len()], &program[..]);
        // One byte of a file changed: refused.
        let mut damaged = packed.clone();
        let at = program.len() + 2 + "queqiao-agent.exe".len() + 8 + 32;
        damaged[at] ^= 1;
        assert!(read(&damaged).unwrap_err().contains("damaged"));
        // Padding after the footer (signing aligns the signature): still found.
        let mut padded = packed.clone();
        padded.extend([0u8; 5]);
        assert_eq!(read(&padded).unwrap().len(), 2);
        // Cut short, or no footer at all.
        assert!(read(&packed[..packed.len() - 3]).is_err());
        assert!(read(&program).is_err());
    }

    #[test]
    fn only_plain_file_names() {
        for name in ["queqiao-agent.exe", "README.txt", "a_b-c.dll"] {
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
        assert!(pack(b"MZ", &bad).is_err());
    }
}
