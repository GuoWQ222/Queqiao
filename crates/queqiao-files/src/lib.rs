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
pub mod history;

pub use clip::{
    ClipMessage, ImageReceiver, ImageSender, MAX_IMAGE, image_fingerprint, test_picture,
};

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
    /// File `id` (offered next) belongs to clipboard copy `batch`: once received it goes on the clipboard.
    Batch {
        id: u32,
        batch: u32,
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
const BATCH: u8 = 9;

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
            FileMessage::Batch { id, batch } => {
                out.push(BATCH);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&batch.to_le_bytes());
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
            BATCH if after_id.len() == 4 => FileMessage::Batch {
                id,
                batch: u32::from_le_bytes(after_id.try_into().ok()?),
            },
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
            | FileMessage::Batch { .. }
    )
}

/// What happened to a file, for the program's report and display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Sent {
        name: String,
        size: u64,
    },
    /// `batch` > 0: part of a clipboard copy; `top` is what goes on the clipboard (the file, or the folder it
    /// came in).
    Received {
        path: PathBuf,
        size: u64,
        batch: u32,
        top: PathBuf,
    },
    Failed {
        name: String,
        reason: String,
    },
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
    /// Paused by the person: no chunks until resumed.
    paused: bool,
}

/// One transfer, for a list of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferRow {
    pub id: u32,
    /// This side sends it (false: receives it).
    pub sending: bool,
    pub name: String,
    pub done: u64,
    pub size: u64,
    pub paused: bool,
}

/// Files this side sends.
#[derive(Default)]
pub struct Outbox {
    next_id: u32,
    files: VecDeque<Outgoing>,
    messages: Vec<FileMessage>,
    outcomes: Vec<Outcome>,
    /// The clipboard copy the files being added belong to (0: none).
    batch: u32,
}

impl Outbox {
    /// Queues `path` (a file) and offers it.
    pub fn add(&mut self, path: &Path) -> Result<(), String> {
        self.add_in(path, 0)
    }

    /// As `add`, the file (or every file of the folder) marked as part of clipboard copy `batch` (0: none).
    pub fn add_in(&mut self, path: &Path, batch: u32) -> Result<(), String> {
        let name = path
            .file_name()
            .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
        self.batch = batch;
        let result = if path.is_dir() {
            self.add_folder(path, &name)
        } else {
            self.add_named(path, name)
        };
        self.batch = 0;
        result
    }

    /// Every file in `folder` (and below), offered as "<folder name>/<path inside>"; links are not followed.
    fn add_folder(&mut self, folder: &Path, name: &str) -> Result<(), String> {
        let mut pending = vec![(folder.to_path_buf(), name.to_string())];
        let mut count = 0usize;
        while let Some((dir, prefix)) = pending.pop() {
            let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let inside = format!("{prefix}/{}", entry.file_name().to_string_lossy());
                if kind.is_symlink() {
                    continue;
                } else if kind.is_dir() {
                    pending.push((entry.path(), inside));
                } else if kind.is_file() {
                    count += 1;
                    if count > MAX_FOLDER_FILES {
                        return Err(format!(
                            "{}: more than {MAX_FOLDER_FILES} files",
                            folder.display()
                        ));
                    }
                    self.add_named(&entry.path(), inside)?;
                }
            }
        }
        Ok(())
    }

    fn add_named(&mut self, path: &Path, name: String) -> Result<(), String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() {
            return Err(format!("{}: not a file", path.display()));
        }
        self.next_id += 1;
        let id = self.next_id;
        if self.batch > 0 {
            self.messages.push(FileMessage::Batch {
                id,
                batch: self.batch,
            });
        }
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
            paused: false,
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
        if let Some(file) = self
            .files
            .iter_mut()
            .find(|f| f.stage == Stage::Sending && !f.paused)
        {
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

    /// Pauses or resumes sending file `id`; false if there is no such file.
    pub fn pause(&mut self, id: u32, paused: bool) -> bool {
        match self.files.iter_mut().find(|f| f.id == id) {
            Some(file) => {
                file.paused = paused;
                true
            }
            None => false,
        }
    }

    /// Stops sending file `id` and tells the receiver (who removes what arrived).
    pub fn cancel(&mut self, id: u32) -> bool {
        let Some(index) = self.files.iter().position(|f| f.id == id) else {
            return false;
        };
        if let Some(file) = self.files.remove(index) {
            self.messages.push(FileMessage::Cancel {
                id,
                reason: "cancelled by the sender".into(),
            });
            self.outcomes.push(Outcome::Failed {
                name: file.name,
                reason: "cancelled".into(),
            });
        }
        true
    }

    pub fn rows(&self) -> Vec<TransferRow> {
        self.files
            .iter()
            .map(|f| TransferRow {
                id: f.id,
                sending: true,
                name: f.name.clone(),
                done: f.sent,
                size: f.size,
                paused: f.paused,
            })
            .collect()
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
/// The mark Windows puts on downloaded files (the Internet zone): SmartScreen and Office's Protected View then treat
/// a received file as one from elsewhere (the security review of 2026-11-22). Left out on drives without alternate
/// data streams (FAT).
fn mark_from_elsewhere(path: &Path) {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    let _ = std::fs::write(PathBuf::from(stream), "[ZoneTransfer]\r\nZoneId=3\r\n");
}

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
    /// Clipboard copy of each announced file id (Batch messages).
    batches: HashMap<u32, u32>,
}

impl Inbox {
    /// Files go into `folder` (created when the first file comes); with `allow` false every offer is refused.
    pub fn new(folder: PathBuf, allow: bool) -> Inbox {
        Inbox {
            folder,
            allow,
            files: HashMap::new(),
            outcomes: Vec::new(),
            batches: HashMap::new(),
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
                        mark_from_elsewhere(&target);
                        // What goes on the clipboard: the file itself, or the folder it came in.
                        let relative = safe_relative_path(&incoming.name);
                        let top = match relative.components().next() {
                            Some(first) if relative.components().count() > 1 => {
                                self.folder.join(first)
                            }
                            _ => target.clone(),
                        };
                        self.outcomes.push(Outcome::Received {
                            path: target,
                            size: incoming.size,
                            batch: self.batches.remove(&id).unwrap_or(0),
                            top,
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
            FileMessage::Batch { id, batch } => {
                // Kept for a bounded number of files (the other side's word).
                if self.batches.len() < 10_000 {
                    self.batches.insert(id, batch);
                }
                Vec::new()
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
        // The files still arriving count too: many at once could otherwise fill the disk (the security review of
        // 2026-11-22: the check was per file).
        let arriving: u64 = self
            .files
            .values()
            .map(|file| file.size.saturating_sub(file.received))
            .sum();
        if let Some(free) = free_space(&self.folder)
            && size.saturating_add(arriving).saturating_add(DISK_RESERVE) > free
        {
            return Err("there is not enough free disk space on that PC".into());
        }
        // A name may be a path inside a sent folder: each part made safe, never leaving the folder.
        let target = self.folder.join(safe_relative_path(name));
        if !target.starts_with(&self.folder) {
            return Err("a name that leaves the folder".into());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
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

    /// Stops receiving file `id` (what arrived is removed) and tells the sender.
    pub fn cancel(&mut self, id: u32) -> Vec<FileMessage> {
        if self.files.contains_key(&id) {
            self.fail(id, "cancelled by the receiver")
        } else {
            Vec::new()
        }
    }

    pub fn rows(&self) -> Vec<TransferRow> {
        let mut rows: Vec<TransferRow> = self
            .files
            .iter()
            .map(|(id, f)| TransferRow {
                id: *id,
                sending: false,
                name: f.name.clone(),
                done: f.received,
                size: f.size,
                paused: false,
            })
            .collect();
        rows.sort_by_key(|row| row.id);
        rows
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
/// File data over time, from its first byte (for the test reports: where the time of a transfer goes, steady or
/// in bursts with pauses).
#[derive(Default)]
struct Course {
    start: Option<Instant>,
    last: Option<Instant>,
    /// Bytes per 100 ms, at most the first two minutes.
    tenths: Vec<u64>,
    /// Pauses between two pieces of data of 150 ms or more.
    pauses: Vec<Duration>,
}

impl Course {
    const MOST: usize = 1200;

    fn add(&mut self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let now = Instant::now();
        let start = *self.start.get_or_insert(now);
        if let Some(last) = self.last
            && now - last >= Duration::from_millis(150)
        {
            self.pauses.push(now - last);
        }
        self.last = Some(now);
        let index = ((now - start).as_millis() / 100) as usize;
        if index < Self::MOST {
            if self.tenths.len() <= index {
                self.tenths.resize(index + 1, 0);
            }
            self.tenths[index] += bytes as u64;
        }
    }

    /// "MB each second: ...; pauses of 150 ms or more: N (longest X ms, Y s in all); kB each 100 ms: ...".
    fn describe(&self) -> Option<String> {
        self.start?;
        let seconds = self
            .tenths
            .chunks(10)
            .map(|tenths| format!("{:.1}", tenths.iter().sum::<u64>() as f64 / 1e6))
            .collect::<Vec<_>>()
            .join(" ");
        let longest = self.pauses.iter().max().copied().unwrap_or_default();
        let paused: Duration = self.pauses.iter().sum();
        let tenths = self
            .tenths
            .iter()
            .take(300)
            .map(|bytes| (bytes / 1000).to_string())
            .collect::<Vec<_>>()
            .join(" ");
        Some(format!(
            "MB each second: {seconds}; pauses of 150 ms or more: {} (longest {} ms, {:.1} s in all); kB each 100 ms (first 30 s): {tenths}",
            self.pauses.len(),
            longest.as_millis(),
            paused.as_secs_f64()
        ))
    }
}

pub struct Transfers {
    outbox: Outbox,
    inbox: Inbox,
    sent_course: Course,
    arrival_course: Course,
    open: bool,
    /// Files to send once the channel is open, with their clipboard copy (0: none).
    queued: Vec<(PathBuf, u32)>,
    last_batch: u32,
}

impl Transfers {
    /// Received files go into `folder`; with `allow_receive` false every offer is refused.
    pub fn new(folder: PathBuf, allow_receive: bool) -> Transfers {
        Transfers {
            outbox: Outbox::default(),
            inbox: Inbox::new(folder, allow_receive),
            sent_course: Course::default(),
            arrival_course: Course::default(),
            open: false,
            queued: Vec::new(),
            last_batch: 0,
        }
    }

    /// The channel is open: files queued before are offered now.
    pub fn opened(&mut self) -> Vec<Outcome> {
        self.open = true;
        let mut failed = Vec::new();
        for (path, batch) in std::mem::take(&mut self.queued) {
            if let Err(reason) = self.outbox.add_in(&path, batch) {
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
        self.send_in(path, 0)
    }

    /// Sends `paths` as one clipboard copy: once they arrived, the other side's clipboard holds them.
    pub fn send_clipboard(&mut self, paths: &[PathBuf]) -> Result<(), String> {
        self.last_batch = self.last_batch.wrapping_add(1).max(1);
        let batch = self.last_batch;
        for path in paths {
            self.send_in(path, batch)?;
        }
        Ok(())
    }

    fn send_in(&mut self, path: &Path, batch: u32) -> Result<(), String> {
        if self.open {
            self.outbox.add_in(path, batch)
        } else {
            self.queued.push((path.to_path_buf(), batch));
            Ok(())
        }
    }

    /// A message from the other side; returns the replies to send (encoded).
    pub fn on_message(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let Some(message) = FileMessage::decode(bytes) else {
            return Vec::new();
        };
        if let FileMessage::Chunk { data, .. } = &message {
            self.arrival_course.add(data.len());
        }
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
        let messages = self.outbox.take(room);
        let data: usize = messages
            .iter()
            .map(|message| match message {
                FileMessage::Chunk { data, .. } => data.len(),
                _ => 0,
            })
            .sum();
        self.sent_course.add(data);
        messages.iter().map(FileMessage::encode).collect()
    }

    /// For the test reports: the course of the file data sent and received so far ("file data sent: MB each
    /// second ..."), then both start over.
    pub fn take_courses(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(course) = std::mem::take(&mut self.sent_course).describe() {
            lines.push(format!("file data sent: {course}"));
        }
        if let Some(course) = std::mem::take(&mut self.arrival_course).describe() {
            lines.push(format!("file data received: {course}"));
        }
        lines
    }

    /// What finished or failed since the last call, both directions.
    pub fn outcomes(&mut self) -> Vec<Outcome> {
        let mut all = self.outbox.take_outcomes();
        all.extend(self.inbox.take_outcomes());
        all
    }

    /// Every transfer on its way: sent ones first.
    pub fn rows(&self) -> Vec<TransferRow> {
        let mut rows = self.outbox.rows();
        rows.extend(self.inbox.rows());
        rows
    }

    /// Pauses or resumes a file this side sends (the receiving side cannot pause).
    pub fn pause(&mut self, id: u32, paused: bool) -> bool {
        self.outbox.pause(id, paused)
    }

    /// Cancels a transfer; returns the messages to send (encoded).
    pub fn cancel(&mut self, sending: bool, id: u32) -> Vec<Vec<u8>> {
        if sending {
            self.outbox.cancel(id);
            // The Cancel message goes with the next take.
            Vec::new()
        } else {
            self.inbox
                .cancel(id)
                .iter()
                .map(FileMessage::encode)
                .collect()
        }
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
/// Files sent from one folder, at most.
const MAX_FOLDER_FILES: usize = 10_000;

/// A path inside a sent folder ("folder/sub/file.txt", either slash), made safe part by part: no empty, "." or
/// ".." parts, no drive letters, each part a safe file name, at most 16 parts deep. Never leaves the folder it
/// is joined to.
pub fn safe_relative_path(name: &str) -> PathBuf {
    let parts: Vec<String> = name
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|part| !part.is_empty() && *part != "." && *part != "..")
        .map(safe_name)
        .collect();
    let keep = parts.len().saturating_sub(16);
    let path: PathBuf = parts[keep..].iter().collect();
    if path.as_os_str().is_empty() {
        PathBuf::from("file")
    } else {
        path
    }
}

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
            let Outcome::Received { path, size, .. } = outcome else {
                panic!("{outcome:?}");
            };
            assert_eq!(*size, content.len() as u64);
            assert_eq!(std::fs::read(path).unwrap(), content);
            // Marked as from elsewhere, as a download is.
            let mut stream = path.as_os_str().to_owned();
            stream.push(":Zone.Identifier");
            let mark =
                std::fs::read_to_string(std::path::PathBuf::from(stream)).unwrap_or_default();
            assert!(mark.contains("ZoneId=3"), "{mark:?}");
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

    /// Messages between an outbox and an inbox until nothing more moves.
    fn pump(outbox: &mut Outbox, inbox: &mut Inbox) {
        for _ in 0..10_000 {
            let messages = outbox.take(usize::MAX);
            if messages.is_empty() {
                return;
            }
            for message in messages {
                for reply in inbox.handle(message) {
                    outbox.handle(&reply);
                }
            }
        }
    }

    #[test]
    fn a_paused_file_waits_and_a_cancelled_one_leaves_nothing() {
        let root = std::env::temp_dir().join(format!("queqiao-files-pause-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("big.bin");
        std::fs::write(&source, vec![3u8; 5 * CHUNK]).unwrap();
        let mut outbox = Outbox::default();
        outbox.add(&source).unwrap();
        let mut inbox = Inbox::new(root.join("inbox"), true);
        // The offer and its acceptance.
        for message in outbox.take(usize::MAX) {
            for reply in inbox.handle(message) {
                outbox.handle(&reply);
            }
        }
        let id = outbox.rows()[0].id;
        assert!(outbox.pause(id, true));
        assert!(outbox.take(usize::MAX).is_empty(), "nothing while paused");
        outbox.pause(id, false);
        let pieces = outbox.take(2 * CHUNK);
        assert!(!pieces.is_empty(), "pieces again once resumed");
        for message in pieces {
            inbox.handle(message);
        }
        assert!(outbox.cancel(id));
        for message in outbox.take(usize::MAX) {
            inbox.handle(message);
        }
        assert!(inbox.rows().is_empty());
        let left: Vec<_> = std::fs::read_dir(root.join("inbox"))
            .unwrap()
            .flatten()
            .collect();
        assert!(left.is_empty(), "nothing kept of a cancelled file");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_arrives_with_its_inside() {
        let root =
            std::env::temp_dir().join(format!("queqiao-files-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("photos");
        std::fs::create_dir_all(source.join("2026").join("may")).unwrap();
        std::fs::write(source.join("top.txt"), b"top").unwrap();
        std::fs::write(
            source.join("2026").join("may").join("deep.bin"),
            vec![7u8; 40_000],
        )
        .unwrap();
        let target = root.join("inbox");
        let mut outbox = Outbox::default();
        outbox.add(&source).unwrap();
        let mut inbox = Inbox::new(target.clone(), true);
        pump(&mut outbox, &mut inbox);
        assert_eq!(
            std::fs::read(target.join("photos").join("top.txt")).unwrap(),
            b"top"
        );
        assert_eq!(
            std::fs::read(
                target
                    .join("photos")
                    .join("2026")
                    .join("may")
                    .join("deep.bin")
            )
            .unwrap(),
            vec![7u8; 40_000]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn paths_from_the_other_side_stay_inside() {
        for (name, expected) in [
            ("../../evil.txt", "evil.txt"),
            ("a/../../b.txt", "a/b.txt"),
            (r"C:\Windows\system32\x.dll", "C_/Windows/system32/x.dll"),
            ("folder/CON/nul.txt", "folder/_CON/_nul.txt"),
            ("/./", "file"),
        ] {
            let path = safe_relative_path(name);
            let joined: Vec<String> = path
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            assert_eq!(joined.join("/"), expected, "{name}");
            assert!(path.is_relative(), "{name}");
        }
        let deep: String = (0..40).map(|i| format!("d{i}/")).collect::<String>() + "f.txt";
        assert_eq!(safe_relative_path(&deep).components().count(), 16);
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
            // Everything below the folder (names may be paths now): files and folders, no part files.
            let mut pending = vec![folder.clone()];
            while let Some(dir) = pending.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    assert!(!name.ends_with(".queqiao-part"), "left behind: {name}");
                    assert!(entry.path().starts_with(&folder));
                    if entry.path().is_dir() {
                        pending.push(entry.path());
                    } else {
                        assert!(entry.path().is_file(), "not a plain file: {name}");
                    }
                }
            }
        }
        let _ = std::fs::remove_dir_all(&parent);
    }
}
