//! File transfer between viewer and agent, over the session's "files" data channel (reliable, ordered),
//! separate from input and control so a large file never delays them. Both directions use the same
//! [`Outbox`] (sending) and [`Inbox`] (receiving):
//!
//! 1. the sender offers a file (name, size); the receiver accepts or rejects it;
//! 2. the sender sends 16 kB chunks while the channel's buffer has room (the caller passes the room);
//! 3. the sender ends with the file's SHA-256; the receiver checks size and hash, then renames the file from
//!    its `.queqiao-part` name and confirms (or deletes it and cancels).
//!
//! The receiver keeps only the file name (no folders), replaces characters Windows does not allow, avoids
//! reserved device names, and never overwrites a file (`name (1).ext`, ...).

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
mod clip;

pub use clip::{
    ClipMessage, ImageReceiver, ImageSender, MAX_IMAGE, image_fingerprint, test_picture,
};

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Bytes per chunk message.
pub const CHUNK: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileMessage {
    Offer {
        id: u32,
        name: String,
        size: u64,
    },
    Accept {
        id: u32,
    },
    Reject {
        id: u32,
        reason: String,
    },
    Chunk {
        id: u32,
        offset: u64,
        data: Vec<u8>,
    },
    /// All chunks are sent; the receiver checks the file against this hash.
    Done {
        id: u32,
        sha256: [u8; 32],
    },
    /// The receiver has the whole file, checked, under its final name.
    Received {
        id: u32,
    },
    /// The sender stops sending this file (to the receiver).
    Cancel {
        id: u32,
        reason: String,
    },
    /// The receiver gives up on this file (to the sender). Separate from `Cancel` because each side numbers its
    /// own outgoing files: an id alone does not say which direction it means.
    Abort {
        id: u32,
        reason: String,
    },
}

const OFFER: u8 = 1;
const ACCEPT: u8 = 2;
const REJECT: u8 = 3;
const CHUNK_TAG: u8 = 4;
const DONE: u8 = 5;
const RECEIVED: u8 = 6;
const CANCEL: u8 = 7;
const ABORT: u8 = 8;

impl FileMessage {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let text = |out: &mut Vec<u8>, text: &str| {
            let bytes = &text.as_bytes()[..text.len().min(u16::MAX as usize)];
            out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(bytes);
        };
        match self {
            FileMessage::Offer { id, name, size } => {
                out.push(OFFER);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&size.to_le_bytes());
                text(&mut out, name);
            }
            FileMessage::Accept { id } => {
                out.push(ACCEPT);
                out.extend_from_slice(&id.to_le_bytes());
            }
            FileMessage::Reject { id, reason }
            | FileMessage::Cancel { id, reason }
            | FileMessage::Abort { id, reason } => {
                out.push(match self {
                    FileMessage::Reject { .. } => REJECT,
                    FileMessage::Cancel { .. } => CANCEL,
                    _ => ABORT,
                });
                out.extend_from_slice(&id.to_le_bytes());
                text(&mut out, reason);
            }
            FileMessage::Chunk { id, offset, data } => {
                out.push(CHUNK_TAG);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&offset.to_le_bytes());
                out.extend_from_slice(data);
            }
            FileMessage::Done { id, sha256 } => {
                out.push(DONE);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(sha256);
            }
            FileMessage::Received { id } => {
                out.push(RECEIVED);
                out.extend_from_slice(&id.to_le_bytes());
            }
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<FileMessage> {
        let (&tag, rest) = bytes.split_first()?;
        let id = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?);
        let after_id = &rest[4..];
        let text = |at: &[u8]| -> Option<String> {
            let length = usize::from(u16::from_le_bytes(at.get(..2)?.try_into().ok()?));
            String::from_utf8(at.get(2..2 + length)?.to_vec()).ok()
        };
        Some(match tag {
            OFFER => FileMessage::Offer {
                id,
                size: u64::from_le_bytes(after_id.get(..8)?.try_into().ok()?),
                name: text(after_id.get(8..)?)?,
            },
            ACCEPT => FileMessage::Accept { id },
            REJECT => FileMessage::Reject {
                id,
                reason: text(after_id)?,
            },
            CANCEL => FileMessage::Cancel {
                id,
                reason: text(after_id)?,
            },
            ABORT => FileMessage::Abort {
                id,
                reason: text(after_id)?,
            },
            CHUNK_TAG => FileMessage::Chunk {
                id,
                offset: u64::from_le_bytes(after_id.get(..8)?.try_into().ok()?),
                data: after_id.get(8..)?.to_vec(),
            },
            DONE => FileMessage::Done {
                id,
                sha256: after_id.get(..32)?.try_into().ok()?,
            },
            RECEIVED => FileMessage::Received { id },
            _ => return None,
        })
    }
}

/// Which side handles `message`: true for the receiving side (`Inbox`), false for the sending side (`Outbox`).
pub fn for_receiver(message: &FileMessage) -> bool {
    matches!(
        message,
        FileMessage::Offer { .. }
            | FileMessage::Chunk { .. }
            | FileMessage::Done { .. }
            | FileMessage::Cancel { .. }
    )
}

/// What happened to a file, for the program's report and display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Sent { name: String, size: u64 },
    Received { path: PathBuf, size: u64 },
    Failed { name: String, reason: String },
}

#[derive(PartialEq, Eq)]
enum Stage {
    Offered,
    Sending,
    /// All chunks and the hash are sent; waiting for the receiver's confirmation.
    Finished,
}

struct Outgoing {
    id: u32,
    name: String,
    size: u64,
    file: File,
    sent: u64,
    hasher: Sha256,
    stage: Stage,
}

/// Files this side sends.
#[derive(Default)]
pub struct Outbox {
    next_id: u32,
    files: VecDeque<Outgoing>,
    messages: Vec<FileMessage>,
    outcomes: Vec<Outcome>,
}

impl Outbox {
    /// Queues `path` (a file) and offers it.
    pub fn add(&mut self, path: &Path) -> Result<(), String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err(format!(
                "{}: not a file (folders are not sent yet)",
                path.display()
            ));
        }
        let name = path
            .file_name()
            .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
        self.next_id += 1;
        let id = self.next_id;
        self.messages.push(FileMessage::Offer {
            id,
            name: name.clone(),
            size: meta.len(),
        });
        self.files.push_back(Outgoing {
            id,
            name,
            size: meta.len(),
            file,
            sent: 0,
            hasher: Sha256::new(),
            stage: Stage::Offered,
        });
        Ok(())
    }

    /// A message from the receiver.
    pub fn handle(&mut self, message: &FileMessage) {
        let find = |files: &mut VecDeque<Outgoing>, id: u32| files.iter().position(|f| f.id == id);
        match message {
            FileMessage::Accept { id } => {
                if let Some(index) = find(&mut self.files, *id) {
                    self.files[index].stage = Stage::Sending;
                }
            }
            FileMessage::Reject { id, reason } | FileMessage::Abort { id, reason } => {
                if let Some(index) = find(&mut self.files, *id)
                    && let Some(file) = self.files.remove(index)
                {
                    self.outcomes.push(Outcome::Failed {
                        name: file.name,
                        reason: reason.clone(),
                    });
                }
            }
            FileMessage::Received { id } => {
                if let Some(index) = find(&mut self.files, *id)
                    && let Some(file) = self.files.remove(index)
                {
                    self.outcomes.push(Outcome::Sent {
                        name: file.name,
                        size: file.size,
                    });
                }
            }
            _ => {}
        }
    }

    /// Messages to send now: offers, and chunks (one file at a time) up to about `room` bytes.
    pub fn take(&mut self, room: usize) -> Vec<FileMessage> {
        let mut out = std::mem::take(&mut self.messages);
        let mut budget = room;
        if let Some(file) = self.files.iter_mut().find(|f| f.stage == Stage::Sending) {
            let mut buffer = vec![0u8; CHUNK];
            while budget >= CHUNK && file.sent < file.size {
                let want = CHUNK.min((file.size - file.sent) as usize);
                if let Err(error) = file.file.read_exact(&mut buffer[..want]) {
                    out.push(FileMessage::Cancel {
                        id: file.id,
                        reason: format!("reading failed: {error}"),
                    });
                    file.stage = Stage::Finished;
                    break;
                }
                file.hasher.update(&buffer[..want]);
                out.push(FileMessage::Chunk {
                    id: file.id,
                    offset: file.sent,
                    data: buffer[..want].to_vec(),
                });
                file.sent += want as u64;
                budget -= want;
            }
            if file.sent >= file.size && file.stage == Stage::Sending {
                out.push(FileMessage::Done {
                    id: file.id,
                    sha256: file.hasher.clone().finalize().into(),
                });
                file.stage = Stage::Finished;
            }
        }
        out
    }

    /// (name, bytes sent, size) of each file not yet confirmed.
    pub fn progress(&self) -> Vec<(String, u64, u64)> {
        self.files
            .iter()
            .map(|f| (f.name.clone(), f.sent, f.size))
            .collect()
    }

    pub fn take_outcomes(&mut self) -> Vec<Outcome> {
        std::mem::take(&mut self.outcomes)
    }
}

/// Files coming at once, at most (each holds an open part file).
const MAX_INCOMING: usize = 16;
/// Disk space a received file must leave free.
const DISK_RESERVE: u64 = 1 << 30;

/// Bytes free for this user on the disk of `folder`.
fn free_space(folder: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = folder.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    unsafe {
        windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            windows::core::PCWSTR(wide.as_ptr()),
            Some(&mut free),
            None,
            None,
        )
    }
    .ok()?;
    Some(free)
}

struct Incoming {
    part: PathBuf,
    target: PathBuf,
    name: String,
    size: u64,
    file: File,
    received: u64,
    hasher: Sha256,
}

/// Files this side receives, into `folder`.
pub struct Inbox {
    folder: PathBuf,
    allow: bool,
    files: HashMap<u32, Incoming>,
    outcomes: Vec<Outcome>,
}

impl Inbox {
    /// Files go into `folder` (created when the first file comes); with `allow` false every offer is refused.
    pub fn new(folder: PathBuf, allow: bool) -> Inbox {
        Inbox {
            folder,
            allow,
            files: HashMap::new(),
            outcomes: Vec::new(),
        }
    }

    /// A message from the sender; returns the replies to send back.
    pub fn handle(&mut self, message: FileMessage) -> Vec<FileMessage> {
        match message {
            FileMessage::Offer { id, name, size } => match self.accept(id, &name, size) {
                Ok(()) => vec![FileMessage::Accept { id }],
                Err(reason) => {
                    self.outcomes.push(Outcome::Failed {
                        name,
                        reason: reason.clone(),
                    });
                    vec![FileMessage::Reject { id, reason }]
                }
            },
            FileMessage::Chunk { id, offset, data } => {
                let Some(incoming) = self.files.get_mut(&id) else {
                    return Vec::new();
                };
                let fits = offset == incoming.received
                    && incoming.received + data.len() as u64 <= incoming.size;
                let written = fits && incoming.file.write_all(&data).is_ok();
                if !written {
                    return self.fail(id, "a chunk did not fit the file (or could not be written)");
                }
                incoming.hasher.update(&data);
                incoming.received += data.len() as u64;
                Vec::new()
            }
            FileMessage::Done { id, sha256 } => {
                let Some(incoming) = self.files.get_mut(&id) else {
                    return Vec::new();
                };
                let hash: [u8; 32] = incoming.hasher.clone().finalize().into();
                if incoming.received != incoming.size || hash != sha256 {
                    return self.fail(
                        id,
                        "the file arrived incomplete or changed (size or SHA-256 differ)",
                    );
                }
                let Some(incoming) = self.files.remove(&id) else {
                    return Vec::new();
                };
                drop(incoming.file);
                let target = unique(&incoming.target);
                match std::fs::rename(&incoming.part, &target) {
                    Ok(()) => {
                        self.outcomes.push(Outcome::Received {
                            path: target,
                            size: incoming.size,
                        });
                        vec![FileMessage::Received { id }]
                    }
                    Err(error) => {
                        let _ = std::fs::remove_file(&incoming.part);
                        self.outcomes.push(Outcome::Failed {
                            name: incoming.name,
                            reason: error.to_string(),
                        });
                        vec![FileMessage::Abort {
                            id,
                            reason: format!("saving failed: {error}"),
                        }]
                    }
                }
            }
            FileMessage::Cancel { id, reason } => {
                if let Some(incoming) = self.files.remove(&id) {
                    drop(incoming.file);
                    let _ = std::fs::remove_file(&incoming.part);
                    self.outcomes.push(Outcome::Failed {
                        name: incoming.name,
                        reason,
                    });
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn accept(&mut self, id: u32, name: &str, size: u64) -> Result<(), String> {
        if !self.allow {
            return Err("receiving files is turned off on that PC".into());
        }
        // A number already on its way: the transfer under it goes on (replacing it would lose its part file).
        if self.files.contains_key(&id) {
            return Err(format!("file {id} is on its way already"));
        }
        if self.files.len() >= MAX_INCOMING {
            return Err(format!("at most {MAX_INCOMING} files can come at once"));
        }
        std::fs::create_dir_all(&self.folder).map_err(|e| e.to_string())?;
        if let Some(free) = free_space(&self.folder)
            && size.saturating_add(DISK_RESERVE) > free
        {
            return Err("there is not enough free disk space on that PC".into());
        }
        let target = self.folder.join(safe_name(name));
        let part = unique(&target.with_file_name(format!(
            "{}.queqiao-part",
            target.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        )));
        let file = File::create(&part).map_err(|e| e.to_string())?;
        self.files.insert(
            id,
            Incoming {
                part,
                target,
                name: name.to_string(),
                size,
                file,
                received: 0,
                hasher: Sha256::new(),
            },
        );
        Ok(())
    }

    fn fail(&mut self, id: u32, reason: &str) -> Vec<FileMessage> {
        if let Some(incoming) = self.files.remove(&id) {
            drop(incoming.file);
            let _ = std::fs::remove_file(&incoming.part);
            self.outcomes.push(Outcome::Failed {
                name: incoming.name,
                reason: reason.to_string(),
            });
        }
        vec![FileMessage::Abort {
            id,
            reason: reason.to_string(),
        }]
    }

    /// (name, bytes received, size) of each file still coming.
    pub fn progress(&self) -> Vec<(String, u64, u64)> {
        self.files
            .values()
            .map(|f| (f.name.clone(), f.received, f.size))
            .collect()
    }

    pub fn take_outcomes(&mut self) -> Vec<Outcome> {
        std::mem::take(&mut self.outcomes)
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        // Files that did not finish leave nothing behind.
        for (_, incoming) in self.files.drain() {
            drop(incoming.file);
            let _ = std::fs::remove_file(&incoming.part);
        }
    }
}

/// Both directions of one session: what the program needs from the "files" channel.
pub struct Transfers {
    outbox: Outbox,
    inbox: Inbox,
    open: bool,
    queued: Vec<PathBuf>,
}

impl Transfers {
    /// Received files go into `folder`; with `allow_receive` false every offer is refused.
    pub fn new(folder: PathBuf, allow_receive: bool) -> Transfers {
        Transfers {
            outbox: Outbox::default(),
            inbox: Inbox::new(folder, allow_receive),
            open: false,
            queued: Vec::new(),
        }
    }

    /// The channel is open: files queued before are offered now.
    pub fn opened(&mut self) -> Vec<Outcome> {
        self.open = true;
        let mut failed = Vec::new();
        for path in std::mem::take(&mut self.queued) {
            if let Err(reason) = self.outbox.add(&path) {
                failed.push(Outcome::Failed {
                    name: path.display().to_string(),
                    reason,
                });
            }
        }
        failed
    }

    /// Sends `path` (now, or once the channel is open).
    pub fn send(&mut self, path: &Path) -> Result<(), String> {
        if self.open {
            self.outbox.add(path)
        } else {
            self.queued.push(path.to_path_buf());
            Ok(())
        }
    }

    /// A message from the other side; returns the replies to send (encoded).
    pub fn on_message(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let Some(message) = FileMessage::decode(bytes) else {
            return Vec::new();
        };
        if for_receiver(&message) {
            self.inbox
                .handle(message)
                .iter()
                .map(FileMessage::encode)
                .collect()
        } else {
            self.outbox.handle(&message);
            Vec::new()
        }
    }

    /// Messages to send now (encoded), at most about `room` bytes of file data.
    pub fn take(&mut self, room: usize) -> Vec<Vec<u8>> {
        if !self.open {
            return Vec::new();
        }
        self.outbox
            .take(room)
            .iter()
            .map(FileMessage::encode)
            .collect()
    }

    /// What finished or failed since the last call, both directions.
    pub fn outcomes(&mut self) -> Vec<Outcome> {
        let mut all = self.outbox.take_outcomes();
        all.extend(self.inbox.take_outcomes());
        all
    }

    /// Files still on their way (either direction).
    pub fn busy(&self) -> bool {
        !self.outbox.progress().is_empty()
            || !self.inbox.progress().is_empty()
            || !self.queued.is_empty()
    }
}

/// A file name that is safe on Windows: the last component only, no forbidden or control characters, no
/// trailing dots or spaces, not a reserved device name, at most 200 characters.
pub fn safe_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let mut clean: String = last
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(200)
        .collect();
    while clean.ends_with(['.', ' ']) {
        clean.pop();
    }
    let stem = clean
        .split('.')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if clean.is_empty() || clean == "." || clean == ".." {
        "file".into()
    } else if reserved {
        format!("_{clean}")
    } else {
        clean
    }
}

/// `path`, or `name (1).ext`, `name (2).ext`, ... if it exists.
fn unique(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let extension = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (1..)
        .map(|n| path.with_file_name(format!("{stem} ({n}){extension}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

/// The person's Downloads folder (where Windows has it, also when moved), then `Queqiao` inside it.
pub fn downloads_folder() -> PathBuf {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    let known = unsafe { SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None) }
        .ok()
        .map(|path| {
            let text = unsafe { path.to_string() }.unwrap_or_default();
            unsafe { CoTaskMemFree(Some(path.0.cast())) };
            PathBuf::from(text)
        })
        .filter(|path| !path.as_os_str().is_empty());
    let downloads = known.unwrap_or_else(|| {
        PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into())).join("Downloads")
    });
    downloads.join("Queqiao")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("queqiao-files-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Runs messages between an outbox and an inbox until both are idle; `tamper` may change chunks.
    fn exchange(outbox: &mut Outbox, inbox: &mut Inbox, mut tamper: impl FnMut(&mut FileMessage)) {
        for _ in 0..10_000 {
            let mut to_receiver = outbox.take(256 * 1024);
            if to_receiver.is_empty() && inbox.progress().is_empty() && outbox.progress().is_empty()
            {
                return;
            }
            let mut replies = Vec::new();
            for message in &mut to_receiver {
                tamper(message);
                let decoded = FileMessage::decode(&message.encode()).expect("round trip");
                assert_eq!(&decoded, message);
                replies.extend(inbox.handle(decoded));
            }
            for reply in replies {
                outbox.handle(&FileMessage::decode(&reply.encode()).unwrap());
            }
        }
        panic!("the transfer did not finish");
    }

    #[test]
    fn a_file_arrives_whole_under_a_safe_unique_name() {
        let dir = temp("send");
        let source = dir.join("report: final?.bin");
        let source = if std::fs::write(&source, b"x").is_ok() {
            source
        } else {
            dir.join("report.bin")
        };
        let content: Vec<u8> = (0..1_000_003u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        std::fs::write(&source, &content).unwrap();
        let inbox_dir = dir.join("in");
        let mut outbox = Outbox::default();
        let mut inbox = Inbox::new(inbox_dir.clone(), true);
        outbox.add(&source).unwrap();
        outbox.add(&source).unwrap();
        exchange(&mut outbox, &mut inbox, |_| {});
        let outcomes = inbox.take_outcomes();
        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        for outcome in &outcomes {
            let Outcome::Received { path, size } = outcome else {
                panic!("{outcome:?}");
            };
            assert_eq!(*size, content.len() as u64);
            assert_eq!(std::fs::read(path).unwrap(), content);
        }
        let names: Vec<String> = std::fs::read_dir(&inbox_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|n| !n.ends_with(".queqiao-part")),
            "{names:?}"
        );
        assert!(
            names.iter().any(|n| n.contains("(1)")),
            "the second copy gets its own name: {names:?}"
        );
        assert_eq!(outbox.take_outcomes().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_chunk_leaves_nothing_behind() {
        let dir = temp("tamper");
        let source = dir.join("data.bin");
        std::fs::write(&source, vec![7u8; 100_000]).unwrap();
        let mut outbox = Outbox::default();
        let mut inbox = Inbox::new(dir.join("in"), true);
        outbox.add(&source).unwrap();
        exchange(&mut outbox, &mut inbox, |message| {
            if let FileMessage::Chunk {
                offset: 0, data, ..
            } = message
            {
                data[0] ^= 1;
            }
        });
        assert!(matches!(
            inbox.take_outcomes()[..],
            [Outcome::Failed { .. }]
        ));
        assert!(matches!(
            outbox.take_outcomes()[..],
            [Outcome::Failed { .. }]
        ));
        assert_eq!(std::fs::read_dir(dir.join("in")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refused_when_receiving_is_off() {
        let dir = temp("off");
        let source = dir.join("a.txt");
        std::fs::write(&source, b"hello").unwrap();
        let mut outbox = Outbox::default();
        let mut inbox = Inbox::new(dir.join("in"), false);
        outbox.add(&source).unwrap();
        exchange(&mut outbox, &mut inbox, |_| {});
        assert!(
            matches!(&outbox.take_outcomes()[..], [Outcome::Failed { reason, .. }] if reason.contains("turned off"))
        );
        assert!(!dir.join("in").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_name("..\\..\\Windows\\System32\\evil.dll"), "evil.dll");
        assert_eq!(safe_name("a/b/c.txt"), "c.txt");
        assert_eq!(safe_name("what?.txt"), "what_.txt");
        assert_eq!(safe_name("CON.txt"), "_CON.txt");
        assert_eq!(safe_name("com1"), "_com1");
        assert_eq!(safe_name("trailing. . "), "trailing");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name("名字.docx"), "名字.docx");
    }
}

/// Robustness: the other side's messages are not trusted. Random and mutated messages must never panic, and a
/// random stream of offers, chunks and endings must never leave anything outside the folder or a part file.
#[cfg(test)]
mod robustness {
    use super::*;

    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
    }

    fn random_name(random: &mut Random) -> String {
        let names = [
            "a.txt",
            r"..\..\evil.exe",
            "../up.txt",
            r"C:\Windows\x.dll",
            "CON",
            "nul.txt",
            "",
            ".",
            "..",
            "name\u{0}zero",
            "trailing. ",
            "ä 中文.bin",
        ];
        if random.below(4) == 0 {
            let length = random.below(300);
            String::from_utf8_lossy(&random.bytes(length)).into_owned()
        } else {
            names[random.below(names.len())].to_string()
        }
    }

    fn random_message(random: &mut Random) -> FileMessage {
        let id = random.below(4) as u32;
        match random.below(5) {
            0 => FileMessage::Offer {
                id,
                name: random_name(random),
                size: if random.below(4) == 0 {
                    random.next()
                } else {
                    random.below(3 * CHUNK) as u64
                },
            },
            1 | 2 => {
                let length = random.below(2 * CHUNK);
                FileMessage::Chunk {
                    id,
                    offset: if random.below(4) == 0 {
                        random.next()
                    } else {
                        (random.below(4) * CHUNK) as u64
                    },
                    data: random.bytes(length),
                }
            }
            3 => FileMessage::Done {
                id,
                sha256: random.bytes(32).try_into().unwrap(),
            },
            _ => FileMessage::Cancel {
                id,
                reason: "stop".into(),
            },
        }
    }

    #[test]
    fn random_and_mutated_messages_never_panic() {
        let mut random = Random(0x2545_F491_4F6C_DD1D);
        for round in 0..200_000 {
            let bytes = if round % 2 == 0 {
                let length = random.below(64);
                random.bytes(length)
            } else {
                let mut bytes = random_message(&mut random).encode();
                if !bytes.is_empty() {
                    let at = random.below(bytes.len());
                    bytes[at] ^= 1 << random.below(8);
                    bytes.truncate(bytes.len() - random.below(bytes.len().min(8) + 1));
                }
                bytes
            };
            if let Some(message) = FileMessage::decode(&bytes) {
                assert_eq!(FileMessage::decode(&message.encode()), Some(message));
            }
        }
    }

    #[test]
    fn a_number_on_its_way_and_a_file_too_big_are_refused() {
        let folder =
            std::env::temp_dir().join(format!("queqiao-files-limits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let mut inbox = Inbox::new(folder.clone(), true);
        let offer = |id, size| FileMessage::Offer {
            id,
            name: "a.txt".into(),
            size,
        };
        assert_eq!(
            inbox.handle(offer(1, 10)),
            vec![FileMessage::Accept { id: 1 }]
        );
        assert!(matches!(
            inbox.handle(offer(1, 10)).as_slice(),
            [FileMessage::Reject { id: 1, .. }]
        ));
        // The first transfer is still there and finishes.
        inbox.handle(FileMessage::Chunk {
            id: 1,
            offset: 0,
            data: b"0123456789".to_vec(),
        });
        let sha256: [u8; 32] = Sha256::digest(b"0123456789").into();
        assert_eq!(
            inbox.handle(FileMessage::Done { id: 1, sha256 }),
            vec![FileMessage::Received { id: 1 }]
        );
        assert!(matches!(
            inbox.handle(offer(2, u64::MAX / 2)).as_slice(),
            [FileMessage::Reject { id: 2, .. }]
        ));
        drop(inbox);
        let names: Vec<_> = std::fs::read_dir(&folder)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("a.txt")]);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn random_streams_stay_in_the_folder() {
        let parent =
            std::env::temp_dir().join(format!("queqiao-files-robust-{}", std::process::id()));
        let folder = parent.join("inbox");
        let _ = std::fs::remove_dir_all(&parent);
        let mut random = Random(0x8BAD_F00D_DEAD_BEEF);
        for _ in 0..40 {
            let mut inbox = Inbox::new(folder.clone(), true);
            for _ in 0..200 {
                let _ = inbox.handle(random_message(&mut random));
            }
            drop(inbox);
            // Nothing beside the folder, and no part files left behind.
            let beside: Vec<_> = std::fs::read_dir(&parent)
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect();
            assert!(beside.iter().all(|name| name == "inbox"), "{beside:?}");
            if let Ok(entries) = std::fs::read_dir(&folder) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    assert!(!name.ends_with(".queqiao-part"), "left behind: {name}");
                    assert!(entry.path().is_file(), "not a plain file: {name}");
                }
            }
        }
        let _ = std::fs::remove_dir_all(&parent);
    }
}
