//! File transfer between viewer and agent, over the session's "files" data channel (reliable, ordered),
//! separate from input and control so a large file never delays them. Both directions use the same
//! [`Outbox`] (sending) and [`Inbox`] (receiving):
//!
//! 1. the sender offers a file (name, size), a few at a time (a folder of many files waits its turn); the receiver
//!    accepts or rejects it;
//! 2. the sender sends 16 kB chunks while the channel's buffer has room (the caller passes the room);
//! 3. the sender ends with the file's SHA-256; the receiver checks size and hash, then renames the file from
//!    its `.queqiao-part` name and confirms (or deletes it and cancels).
//!
//! The file transfer window (UU Remote's two panes, browse.rs) adds browsing: the controlling side lists the other
//! side's folders (`List`, answered by `Listing`), fetches files from there into a folder of its own (`Fetch`: the
//! files then come, each after a `From` naming the request) and sends files into a folder there (`Into` before the
//! offer). Only the controlled side answers these, and only while it allows files ([`Transfers::serve_browsing`]);
//! the controlling side never lets the other side choose a folder on it. [`Transfers`] keeps what the person sent or
//! fetched as jobs, for the window's transfer list.
//!
//! The receiver makes each name safe (no folders above, characters Windows does not allow replaced, no reserved
//! device names) and never overwrites a file (`name (1).ext`, ...).

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{Read, Write};
pub mod browse;
mod clip;
pub mod history;

pub use browse::{Change, Entry, EntryKind, Found, Lister, Listing, Size};
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
    /// The controlling side asks what folder `path` holds ("" : the places); `id` numbers the request.
    List {
        id: u32,
        path: String,
    },
    /// A part of the answer to List `id`; `more`: another part follows.
    Listing {
        id: u32,
        listing: Listing,
        more: bool,
    },
    /// The controlling side asks for the file or folder `path`: its files come as sent ones do, each after a `From`.
    Fetch {
        id: u32,
        path: String,
    },
    /// File `id` (offered next) answers Fetch `request`.
    From {
        id: u32,
        request: u32,
    },
    /// File `id` (offered next) goes into `folder` on the receiving (controlled) side, not its downloads folder.
    Into {
        id: u32,
        folder: String,
    },
    /// What Fetch `id` sends: this many files and bytes.
    Plan {
        id: u32,
        files: u32,
        bytes: u64,
    },
    /// List or Fetch `id` refused or failed.
    Refused {
        id: u32,
        reason: String,
    },
    /// The files that answer Fetch `id` wait (true) or go on.
    Pause {
        id: u32,
        paused: bool,
    },
    /// The files that answer Fetch `id` are not wanted any more.
    Drop {
        id: u32,
    },
    /// The controlling side asks for a change in its folders there (new folder, rename, delete); answered by
    /// `Changed`.
    Change {
        id: u32,
        change: Change,
    },
    /// Change `id` is made (`error` empty), or why not.
    Changed {
        id: u32,
        error: String,
    },
    /// The controlling side asks what `path` holds, all levels down; answered by `Measured`.
    Measure {
        id: u32,
        path: String,
    },
    /// Measure `id`: the count, or why there is none (`error`).
    Measured {
        id: u32,
        size: Size,
        error: String,
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
const LIST: u8 = 10;
const LISTING: u8 = 11;
const FETCH: u8 = 12;
const FROM: u8 = 13;
const INTO: u8 = 14;
const PLAN: u8 = 15;
const REFUSED: u8 = 16;
const PAUSE: u8 = 17;
const DROP: u8 = 18;
const CHANGE: u8 = 19;
const CHANGED: u8 = 20;
const MEASURE: u8 = 21;
const MEASURED: u8 = 22;

/// The kinds of a Change.
const MAKE_FOLDER: u8 = 1;
const RENAME: u8 = 2;
const DELETE: u8 = 3;

/// Entries in one Listing message at most, and their names' bytes (a listing comes in parts below the channel's
/// message size).
const LISTING_PART_ENTRIES: usize = 500;
const LISTING_PART_BYTES: usize = 48 * 1024;

fn put_text(out: &mut Vec<u8>, text: &str) {
    // At most 65535 bytes, cut at a character's start.
    let mut end = text.len().min(u16::MAX as usize);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    out.extend_from_slice(&(end as u16).to_le_bytes());
    out.extend_from_slice(&text.as_bytes()[..end]);
}

/// Reads a message's fields in order; None when it is too short.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (head, rest) = (self.0.get(..count)?, self.0.get(count..)?);
        self.0 = rest;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn text(&mut self) -> Option<String> {
        let length = usize::from(u16::from_le_bytes(self.take(2)?.try_into().ok()?));
        String::from_utf8(self.take(length)?.to_vec()).ok()
    }
    fn rest(self) -> &'a [u8] {
        self.0
    }
}

impl FileMessage {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            FileMessage::Offer { id, name, size } => {
                out.push(OFFER);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&size.to_le_bytes());
                put_text(&mut out, name);
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
            | FileMessage::Abort { id, reason }
            | FileMessage::Refused { id, reason } => {
                out.push(match self {
                    FileMessage::Reject { .. } => REJECT,
                    FileMessage::Cancel { .. } => CANCEL,
                    FileMessage::Refused { .. } => REFUSED,
                    _ => ABORT,
                });
                out.extend_from_slice(&id.to_le_bytes());
                put_text(&mut out, reason);
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
            FileMessage::List { id, path }
            | FileMessage::Fetch { id, path }
            | FileMessage::Into { id, folder: path } => {
                out.push(match self {
                    FileMessage::List { .. } => LIST,
                    FileMessage::Fetch { .. } => FETCH,
                    _ => INTO,
                });
                out.extend_from_slice(&id.to_le_bytes());
                put_text(&mut out, path);
            }
            FileMessage::Change { id, change } => {
                out.push(CHANGE);
                out.extend_from_slice(&id.to_le_bytes());
                match change {
                    Change::MakeFolder { folder, name } => {
                        out.push(MAKE_FOLDER);
                        put_text(&mut out, folder);
                        put_text(&mut out, name);
                    }
                    Change::Rename { path, name } => {
                        out.push(RENAME);
                        put_text(&mut out, path);
                        put_text(&mut out, name);
                    }
                    Change::Delete { path } => {
                        out.push(DELETE);
                        put_text(&mut out, path);
                    }
                }
            }
            FileMessage::Changed { id, error } => {
                out.push(CHANGED);
                out.extend_from_slice(&id.to_le_bytes());
                put_text(&mut out, error);
            }
            FileMessage::Measure { id, path } => {
                out.push(MEASURE);
                out.extend_from_slice(&id.to_le_bytes());
                put_text(&mut out, path);
            }
            FileMessage::Measured { id, size, error } => {
                out.push(MEASURED);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&size.files.to_le_bytes());
                out.extend_from_slice(&size.folders.to_le_bytes());
                out.extend_from_slice(&size.bytes.to_le_bytes());
                out.push(u8::from(size.complete));
                put_text(&mut out, error);
            }
            FileMessage::Listing { id, listing, more } => {
                out.push(LISTING);
                out.extend_from_slice(&id.to_le_bytes());
                out.push(u8::from(*more));
                put_text(&mut out, &listing.path);
                put_text(&mut out, &listing.parent);
                put_text(&mut out, &listing.error);
                out.extend_from_slice(&(listing.entries.len() as u32).to_le_bytes());
                for entry in &listing.entries {
                    out.push(entry.kind.code());
                    out.extend_from_slice(&entry.size.to_le_bytes());
                    out.extend_from_slice(&entry.modified.to_le_bytes());
                    put_text(&mut out, &entry.name);
                }
            }
            FileMessage::From { id, request } => {
                out.push(FROM);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&request.to_le_bytes());
            }
            FileMessage::Plan { id, files, bytes } => {
                out.push(PLAN);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&files.to_le_bytes());
                out.extend_from_slice(&bytes.to_le_bytes());
            }
            FileMessage::Pause { id, paused } => {
                out.push(PAUSE);
                out.extend_from_slice(&id.to_le_bytes());
                out.push(u8::from(*paused));
            }
            FileMessage::Drop { id } => {
                out.push(DROP);
                out.extend_from_slice(&id.to_le_bytes());
            }
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<FileMessage> {
        let mut reader = Reader(bytes);
        let tag = reader.u8()?;
        let id = reader.u32()?;
        Some(match tag {
            OFFER => FileMessage::Offer {
                id,
                size: reader.u64()?,
                name: reader.text()?,
            },
            ACCEPT => FileMessage::Accept { id },
            BATCH if reader.0.len() == 4 => FileMessage::Batch {
                id,
                batch: reader.u32()?,
            },
            REJECT => FileMessage::Reject {
                id,
                reason: reader.text()?,
            },
            CANCEL => FileMessage::Cancel {
                id,
                reason: reader.text()?,
            },
            ABORT => FileMessage::Abort {
                id,
                reason: reader.text()?,
            },
            REFUSED => FileMessage::Refused {
                id,
                reason: reader.text()?,
            },
            CHUNK_TAG => FileMessage::Chunk {
                id,
                offset: reader.u64()?,
                data: reader.rest().to_vec(),
            },
            DONE => FileMessage::Done {
                id,
                sha256: reader.take(32)?.try_into().ok()?,
            },
            RECEIVED => FileMessage::Received { id },
            LIST => FileMessage::List {
                id,
                path: reader.text()?,
            },
            FETCH => FileMessage::Fetch {
                id,
                path: reader.text()?,
            },
            INTO => FileMessage::Into {
                id,
                folder: reader.text()?,
            },
            LISTING => {
                let more = reader.u8()? != 0;
                let path = reader.text()?;
                let parent = reader.text()?;
                let error = reader.text()?;
                let count = reader.u32()? as usize;
                // The other side's count is not trusted: entries are read as long as they are there.
                let mut entries = Vec::with_capacity(count.min(LISTING_PART_ENTRIES));
                for _ in 0..count {
                    let kind = EntryKind::from_code(reader.u8()?)?;
                    let size = reader.u64()?;
                    let modified = reader.i64()?;
                    let name = reader.text()?;
                    entries.push(Entry {
                        name,
                        kind,
                        size,
                        modified,
                    });
                }
                FileMessage::Listing {
                    id,
                    listing: Listing {
                        path,
                        parent,
                        entries,
                        error,
                    },
                    more,
                }
            }
            FROM => FileMessage::From {
                id,
                request: reader.u32()?,
            },
            PLAN => FileMessage::Plan {
                id,
                files: reader.u32()?,
                bytes: reader.u64()?,
            },
            PAUSE => FileMessage::Pause {
                id,
                paused: reader.u8()? != 0,
            },
            DROP => FileMessage::Drop { id },
            CHANGE => FileMessage::Change {
                id,
                change: match reader.u8()? {
                    MAKE_FOLDER => Change::MakeFolder {
                        folder: reader.text()?,
                        name: reader.text()?,
                    },
                    RENAME => Change::Rename {
                        path: reader.text()?,
                        name: reader.text()?,
                    },
                    DELETE => Change::Delete {
                        path: reader.text()?,
                    },
                    _ => return None,
                },
            },
            CHANGED => FileMessage::Changed {
                id,
                error: reader.text()?,
            },
            MEASURE => FileMessage::Measure {
                id,
                path: reader.text()?,
            },
            MEASURED => FileMessage::Measured {
                id,
                size: Size {
                    files: reader.u64()?,
                    folders: reader.u64()?,
                    bytes: reader.u64()?,
                    complete: reader.u8()? != 0,
                },
                error: reader.text()?,
            },
            _ => return None,
        })
    }
}

/// Which side handles `message`: true for the receiving side (`Inbox`), false for the sending side (`Outbox`).
/// (Browsing's requests and answers are the `Transfers`' own.)
pub fn for_receiver(message: &FileMessage) -> bool {
    matches!(
        message,
        FileMessage::Offer { .. }
            | FileMessage::Chunk { .. }
            | FileMessage::Done { .. }
            | FileMessage::Cancel { .. }
            | FileMessage::Batch { .. }
            | FileMessage::From { .. }
            | FileMessage::Into { .. }
    )
}

/// What happened to a file, for the program's report and display. `id` is the file's number on the side that sends
/// it (0: it never got one, a file that could not be read).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Sent {
        id: u32,
        name: String,
        size: u64,
    },
    /// `batch` > 0: part of a clipboard copy; `top` is what goes on the clipboard (the file, or the folder it
    /// came in).
    Received {
        id: u32,
        path: PathBuf,
        size: u64,
        batch: u32,
        top: PathBuf,
    },
    Failed {
        id: u32,
        /// This side sent it (false: received it).
        sending: bool,
        name: String,
        reason: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Not offered yet: waits for its turn.
    Queued,
    Offered,
    Sending,
    /// All chunks and the hash are sent; waiting for the receiver's confirmation.
    Finished,
}

struct Outgoing {
    id: u32,
    name: String,
    size: u64,
    path: PathBuf,
    /// Opened when offered.
    file: Option<File>,
    sent: u64,
    hasher: Sha256,
    stage: Stage,
    /// Paused by the person: no chunks until resumed.
    paused: bool,
    /// Messages that go just before the offer (Batch, From, Into).
    before: Vec<FileMessage>,
    /// The Fetch this file answers (0: none).
    request: u32,
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

/// Files offered and not yet confirmed, at most: the rest wait their turn (a folder of many files met the receiver's
/// limit on files coming at once, MAX_INCOMING).
const OFFER_AHEAD: usize = 8;

/// Files this side sends.
#[derive(Default)]
pub struct Outbox {
    next_id: u32,
    files: VecDeque<Outgoing>,
    messages: Vec<FileMessage>,
    outcomes: Vec<Outcome>,
}

impl Outbox {
    /// Queues `path` (a file, or a folder with what is in it) to offer.
    pub fn add(&mut self, path: &Path) -> Result<(), String> {
        self.add_in(path, 0)
    }

    /// As `add`, the file (or every file of the folder) marked as part of clipboard copy `batch` (0: none).
    pub fn add_in(&mut self, path: &Path, batch: u32) -> Result<(), String> {
        let files = browse::files_of(path)?;
        self.add_files(files, 0, |id| {
            if batch > 0 {
                vec![FileMessage::Batch { id, batch }]
            } else {
                Vec::new()
            }
        });
        Ok(())
    }

    /// Queues `files` (path, name to send, size) to offer in turn, answering Fetch `request` (0: none); `before`
    /// gives a file's messages to go just before its offer, by its id. Returns their ids.
    pub fn add_files(
        &mut self,
        files: Vec<(PathBuf, String, u64)>,
        request: u32,
        before: impl Fn(u32) -> Vec<FileMessage>,
    ) -> Vec<u32> {
        files
            .into_iter()
            .map(|(path, name, size)| {
                self.next_id += 1;
                let id = self.next_id;
                self.files.push_back(Outgoing {
                    id,
                    name,
                    size,
                    path,
                    file: None,
                    sent: 0,
                    hasher: Sha256::new(),
                    stage: Stage::Queued,
                    paused: false,
                    before: before(id),
                    request,
                });
                id
            })
            .collect()
    }

    /// A message from the receiver.
    pub fn handle(&mut self, message: &FileMessage) {
        let find = |files: &mut VecDeque<Outgoing>, id: u32| files.iter().position(|f| f.id == id);
        match message {
            FileMessage::Accept { id } => {
                if let Some(index) = find(&mut self.files, *id)
                    && self.files[index].stage == Stage::Offered
                {
                    self.files[index].stage = Stage::Sending;
                }
            }
            FileMessage::Reject { id, reason } | FileMessage::Abort { id, reason } => {
                if let Some(index) = find(&mut self.files, *id)
                    && let Some(file) = self.files.remove(index)
                {
                    self.outcomes.push(Outcome::Failed {
                        id: file.id,
                        sending: true,
                        name: file.name,
                        reason: reason.clone(),
                    });
                }
            }
            FileMessage::Received { id } => {
                if let Some(index) = find(&mut self.files, *id)
                    && self.files[index].stage == Stage::Finished
                    && let Some(file) = self.files.remove(index)
                {
                    self.outcomes.push(Outcome::Sent {
                        id: file.id,
                        name: file.name,
                        size: file.size,
                    });
                }
            }
            FileMessage::Pause { id, paused } if *id > 0 => {
                for file in self.files.iter_mut().filter(|f| f.request == *id) {
                    file.paused = *paused;
                }
            }
            FileMessage::Drop { id } if *id > 0 => {
                let ids: Vec<u32> = self
                    .files
                    .iter()
                    .filter(|f| f.request == *id)
                    .map(|f| f.id)
                    .collect();
                for file in ids {
                    self.cancel(file);
                }
            }
            _ => {}
        }
    }

    /// Messages to send now: offers (a few files ahead), and chunks (one file at a time) up to about `room` bytes.
    pub fn take(&mut self, room: usize) -> Vec<FileMessage> {
        let mut out = std::mem::take(&mut self.messages);
        let mut active = self
            .files
            .iter()
            .filter(|f| f.stage != Stage::Queued)
            .count();
        let mut unreadable = Vec::new();
        for file in self.files.iter_mut() {
            if active >= OFFER_AHEAD {
                break;
            }
            if file.stage != Stage::Queued || file.paused {
                continue;
            }
            // Opened now (at most OFFER_AHEAD at once): its size as it is now.
            let opened = File::open(&file.path)
                .and_then(|opened| Ok((opened.metadata()?.len(), opened)))
                .map_err(|e| format!("{}: {e}", file.path.display()));
            match opened {
                Ok((size, opened)) => {
                    file.file = Some(opened);
                    file.size = size;
                    out.append(&mut file.before);
                    out.push(FileMessage::Offer {
                        id: file.id,
                        name: file.name.clone(),
                        size,
                    });
                    file.stage = Stage::Offered;
                    active += 1;
                }
                Err(reason) => unreadable.push((file.id, reason)),
            }
        }
        for (id, reason) in unreadable {
            if let Some(index) = self.files.iter().position(|f| f.id == id)
                && let Some(file) = self.files.remove(index)
            {
                self.outcomes.push(Outcome::Failed {
                    id,
                    sending: true,
                    name: file.name,
                    reason,
                });
            }
        }
        let mut budget = room;
        if let Some(file) = self
            .files
            .iter_mut()
            .find(|f| f.stage == Stage::Sending && !f.paused)
        {
            let mut buffer = vec![0u8; CHUNK];
            while budget >= CHUNK && file.sent < file.size {
                let want = CHUNK.min((file.size - file.sent) as usize);
                let read = match file.file.as_mut() {
                    Some(opened) => opened.read_exact(&mut buffer[..want]),
                    None => Err(std::io::Error::other("not open")),
                };
                if let Err(error) = read {
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
                // Nothing more to read: the handle goes (a folder's files are not all held open).
                file.file = None;
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
            if file.stage != Stage::Queued {
                self.messages.push(FileMessage::Cancel {
                    id,
                    reason: "cancelled by the sender".into(),
                });
            }
            self.outcomes.push(Outcome::Failed {
                id,
                sending: true,
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

    /// (name, bytes sent, size) of each file not yet confirmed (also those waiting their turn).
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
/// Notes kept about files announced by the other side (Batch, From, Into) at most.
const MAX_NOTES: usize = 10_000;

/// The mark Windows puts on downloaded files (the Internet zone): SmartScreen and Office's Protected View then treat
/// a received file as one from elsewhere (the security review of 2026-11-22). Left out on drives without alternate
/// data streams (FAT).
#[cfg(windows)]
fn mark_from_elsewhere(path: &Path) {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    let _ = std::fs::write(PathBuf::from(stream), "[ZoneTransfer]\r\nZoneId=3\r\n");
}

/// Linux has no zone a program must honour; the origin is noted where browsers note theirs (the extended attribute
/// `user.xdg.origin.url`, which file managers show). Left out where the file system has no user attributes.
#[cfg(unix)]
fn mark_from_elsewhere(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return;
    };
    let value = b"queqiao://transfer";
    unsafe {
        libc::setxattr(
            name.as_ptr(),
            c"user.xdg.origin.url".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
}

/// Bytes free for this user on the disk of `folder`.
#[cfg(windows)]
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

/// Bytes free for this user on the disk of `folder` (blocks available to unprivileged users).
#[cfg(unix)]
fn free_space(folder: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(folder.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(name.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    Some(stats.f_bavail as u64 * stats.f_frsize as u64)
}

struct Incoming {
    part: PathBuf,
    target: PathBuf,
    /// The folder it goes into (the downloads folder, a fetch's folder, or the folder the other side named).
    base: PathBuf,
    name: String,
    size: u64,
    file: File,
    received: u64,
    hasher: Sha256,
}

/// Files this side receives, into `folder` (or a fetch's folder, or one the other side names where that is allowed).
pub struct Inbox {
    folder: PathBuf,
    allow: bool,
    /// The other side may name a folder of this PC (`Into`): only on the controlled side, while it allows files.
    allow_into: bool,
    files: HashMap<u32, Incoming>,
    outcomes: Vec<Outcome>,
    /// Clipboard copy of each announced file id (Batch messages).
    batches: HashMap<u32, u32>,
    /// The folder each announced file id is to go into there (Into), checked when it is offered.
    into: HashMap<u32, String>,
    /// The Fetch each announced file id answers (From), and each Fetch's folder here.
    from: HashMap<u32, u32>,
    fetch_folders: HashMap<u32, PathBuf>,
}

impl Inbox {
    /// Files go into `folder` (created when the first file comes); with `allow` false every offer is refused.
    pub fn new(folder: PathBuf, allow: bool) -> Inbox {
        Inbox {
            folder,
            allow,
            allow_into: false,
            files: HashMap::new(),
            outcomes: Vec::new(),
            batches: HashMap::new(),
            into: HashMap::new(),
            from: HashMap::new(),
            fetch_folders: HashMap::new(),
        }
    }

    /// Lets the other side name the folder a file goes into (the controlled side, while it allows files).
    pub fn allow_into(&mut self, on: bool) {
        self.allow_into = on;
    }

    /// The files that answer this side's Fetch `request` go into `folder`.
    pub fn fetch_into(&mut self, request: u32, folder: PathBuf) {
        self.fetch_folders.insert(request, folder);
    }

    /// A message from the sender; returns the replies to send back.
    pub fn handle(&mut self, message: FileMessage) -> Vec<FileMessage> {
        match message {
            FileMessage::Offer { id, name, size } => match self.accept(id, &name, size) {
                Ok(()) => vec![FileMessage::Accept { id }],
                Err(reason) => {
                    self.outcomes.push(Outcome::Failed {
                        id,
                        sending: false,
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
                                incoming.base.join(first)
                            }
                            _ => target.clone(),
                        };
                        self.outcomes.push(Outcome::Received {
                            id,
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
                            id,
                            sending: false,
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
            // Kept for a bounded number of files (the other side's word).
            FileMessage::Batch { id, batch } => {
                if self.batches.len() < MAX_NOTES {
                    self.batches.insert(id, batch);
                }
                Vec::new()
            }
            FileMessage::Into { id, folder } => {
                if self.into.len() < MAX_NOTES {
                    self.into.insert(id, folder);
                }
                Vec::new()
            }
            FileMessage::From { id, request } => {
                if self.from.len() < MAX_NOTES {
                    self.from.insert(id, request);
                }
                Vec::new()
            }
            FileMessage::Cancel { id, reason } => {
                if let Some(incoming) = self.files.remove(&id) {
                    drop(incoming.file);
                    let _ = std::fs::remove_file(&incoming.part);
                    self.outcomes.push(Outcome::Failed {
                        id,
                        sending: false,
                        name: incoming.name,
                        reason,
                    });
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The folder file `id` goes into: its fetch's, the one the other side named (where allowed), else the downloads
    /// folder.
    fn base_for(&mut self, id: u32) -> Result<PathBuf, String> {
        let named = self.into.remove(&id);
        if let Some(request) = self.from.remove(&id) {
            return Ok(self
                .fetch_folders
                .get(&request)
                .cloned()
                .unwrap_or_else(|| self.folder.clone()));
        }
        let Some(folder) = named else {
            return Ok(self.folder.clone());
        };
        if !self.allow_into {
            return Err("this PC does not let the other side choose where files go".into());
        }
        let path = PathBuf::from(&folder);
        if !path.is_absolute() || !path.is_dir() {
            return Err(format!("{folder}: no such folder on that PC"));
        }
        Ok(path)
    }

    fn accept(&mut self, id: u32, name: &str, size: u64) -> Result<(), String> {
        let base = self.base_for(id);
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
        let base = base?;
        if base == self.folder {
            std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
        } else if !base.is_dir() {
            return Err(format!("{}: the folder is not there", base.display()));
        }
        // The files still arriving count too: many at once could otherwise fill the disk (the security review of
        // 2026-11-22: the check was per file).
        let arriving: u64 = self
            .files
            .values()
            .map(|file| file.size.saturating_sub(file.received))
            .sum();
        if let Some(free) = free_space(&base)
            && size.saturating_add(arriving).saturating_add(DISK_RESERVE) > free
        {
            return Err("there is not enough free disk space on that PC".into());
        }
        // A name may be a path inside a sent folder: each part made safe, never leaving the folder.
        let target = base.join(safe_relative_path(name));
        if !target.starts_with(&base) {
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
                base,
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
                id,
                sending: false,
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

/// Where a job stands, for the transfer list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    /// Nothing moved yet (its files are being found, or wait their turn).
    Waiting,
    Going,
    Paused,
    Done,
    Failed(String),
    Cancelled,
}

/// One thing the person sent or fetched (a file, or a folder with what is in it), or files the other side sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub id: u32,
    /// The file's or folder's name.
    pub name: String,
    /// This side sends it (false: receives it).
    pub sending: bool,
    /// Where it comes from and goes to, as each side writes paths; "" : that side's Queqiao downloads folder (or, for
    /// what the other side sent by itself, its own unknown place).
    pub from: String,
    pub to: String,
    pub files: u32,
    pub files_done: u32,
    pub bytes: u64,
    pub bytes_done: u64,
    pub state: JobState,
    /// Received here: what arrived (the file, or the folder it came in), to show in its folder.
    pub arrived: Option<PathBuf>,
    /// The person can pause it (this side sends it, or it answers this side's fetch).
    pub can_pause: bool,
}

impl Job {
    pub fn finished(&self) -> bool {
        matches!(
            self.state,
            JobState::Done | JobState::Failed(_) | JobState::Cancelled
        )
    }
}

struct JobEntry {
    job: Job,
    /// Its files (this side's numbers when sending, the other side's when receiving).
    ids: HashSet<u32>,
    failed: u32,
    reason: String,
    /// Bytes of its files that finished (done or failed).
    finished_bytes: u64,
    /// Its files are all known (found here, or the other side's Plan came).
    known: bool,
    paused: bool,
    cancelled: bool,
}

/// What the worker thread was asked for.
enum Purpose {
    /// Files to send: job, the folder there ("" : the downloads folder), the clipboard copy (0: none).
    Send(u32, String, u32),
    /// The other side's List and Fetch, answered here (the controlled side).
    ServeList(u32),
    ServeFetch(u32),
    /// The other side's Change and Measure, answered here (the change described for the session's log).
    ServeChange(u32, String),
    ServeMeasure(u32),
}

/// The answer to this side's Change or Measure of the other side's folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Changed(Result<(), String>),
    Measured(Result<Size, String>),
}

/// Requests of the other side waiting for the worker at most (more are refused as busy).
const MAX_SERVING: usize = 64;

/// Both directions of one session: what the program needs from the "files" channel.
pub struct Transfers {
    outbox: Outbox,
    inbox: Inbox,
    sent_course: Course,
    arrival_course: Course,
    open: bool,
    last_batch: u32,
    /// Reads folders and finds files on a thread of its own (made when first needed).
    lister: Option<Lister>,
    work: HashMap<u32, Purpose>,
    next_work: u32,
    /// Requests and answers to send with the next `take`.
    pending: Vec<FileMessage>,
    /// This side answers the other's List and Fetch (the controlled side, while it allows files).
    serving: bool,
    jobs: Vec<JobEntry>,
    next_job: u32,
    /// Jobs of files the other side sent by itself, by the first part of their names, while they come.
    loose: HashMap<String, u32>,
    /// Listings this side asked for: the parts so far; and those complete.
    listing_parts: HashMap<u32, Listing>,
    listings: Vec<(u32, Listing)>,
    /// List requests of this side not yet answered (numbered with the jobs: a number is a job's or a list's).
    lists: HashSet<u32>,
    /// Fetch requests of this side: the job of each.
    fetches: HashMap<u32, u32>,
    /// Outcomes not yet taken by the program.
    outcomes: Vec<Outcome>,
    /// Changes and counts this side asked of the other (numbered with the jobs), and their answers so far.
    asks: HashSet<u32>,
    answers: Vec<(u32, Answer)>,
    /// The changes made here at the other side's request, for the session's log.
    served: Vec<String>,
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
            last_batch: 0,
            lister: None,
            work: HashMap::new(),
            next_work: 0,
            pending: Vec::new(),
            serving: false,
            jobs: Vec::new(),
            next_job: 0,
            loose: HashMap::new(),
            listing_parts: HashMap::new(),
            listings: Vec::new(),
            lists: HashSet::new(),
            fetches: HashMap::new(),
            outcomes: Vec::new(),
            asks: HashSet::new(),
            answers: Vec::new(),
            served: Vec::new(),
        }
    }

    /// The controlled side, while it allows files: the other side may list its folders, fetch from them and send
    /// into them.
    pub fn serve_browsing(&mut self, on: bool) {
        self.serving = on;
        self.inbox.allow_into(on);
    }

    fn lister(&mut self) -> &Lister {
        self.lister.get_or_insert_with(Lister::new)
    }

    fn ask_files(&mut self, path: &Path, purpose: Purpose) {
        self.next_work += 1;
        let id = self.next_work;
        self.work.insert(id, purpose);
        self.lister().files(id, path);
    }

    /// The channel is open: what was queued goes now. (Kept for the programs' reports: nothing fails here now.)
    pub fn opened(&mut self) -> Vec<Outcome> {
        self.open = true;
        Vec::new()
    }

    fn new_job(&mut self, name: String, sending: bool, from: String, to: String) -> u32 {
        self.next_job += 1;
        let id = self.next_job;
        self.jobs.push(JobEntry {
            job: Job {
                id,
                name,
                sending,
                from,
                to,
                files: 0,
                files_done: 0,
                bytes: 0,
                bytes_done: 0,
                state: JobState::Waiting,
                arrived: None,
                can_pause: true,
            },
            ids: HashSet::new(),
            failed: 0,
            reason: String::new(),
            finished_bytes: 0,
            known: false,
            paused: false,
            cancelled: false,
        });
        id
    }

    fn job_mut(&mut self, id: u32) -> Option<&mut JobEntry> {
        self.jobs.iter_mut().find(|entry| entry.job.id == id)
    }

    /// Sends `path` (a file or a folder) into the other side's downloads folder.
    pub fn send(&mut self, path: &Path) -> Result<(), String> {
        self.send_job(path, "", 0);
        Ok(())
    }

    /// Sends `path` into the folder `folder` of the other side (as it writes paths); returns the job.
    pub fn send_into(&mut self, path: &Path, folder: &str) -> u32 {
        self.send_job(path, folder, 0)
    }

    /// Sends `paths` as one clipboard copy: once they arrived, the other side's clipboard holds them.
    pub fn send_clipboard(&mut self, paths: &[PathBuf]) -> Result<(), String> {
        self.last_batch = self.last_batch.wrapping_add(1).max(1);
        let batch = self.last_batch;
        for path in paths {
            self.send_job(path, "", batch);
        }
        Ok(())
    }

    fn send_job(&mut self, path: &Path, folder: &str, batch: u32) -> u32 {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let job = self.new_job(name, true, path.display().to_string(), folder.to_string());
        self.ask_files(path, Purpose::Send(job, folder.to_string(), batch));
        job
    }

    /// Fetches the other side's file or folder `path` into `folder` here; returns the job.
    pub fn fetch(&mut self, path: &str, folder: &Path) -> u32 {
        let job = self.new_job(
            browse::last_part(path).to_string(),
            false,
            path.to_string(),
            folder.display().to_string(),
        );
        self.fetches.insert(job, job);
        self.inbox.fetch_into(job, folder.to_path_buf());
        self.pending.push(FileMessage::Fetch {
            id: job,
            path: path.to_string(),
        });
        job
    }

    /// Asks the other side what its folder `path` holds ("" : its places); the answer comes from `listings`.
    pub fn list_remote(&mut self, path: &str) -> u32 {
        self.next_job += 1;
        let id = self.next_job;
        self.lists.insert(id);
        self.pending.push(FileMessage::List {
            id,
            path: path.to_string(),
        });
        id
    }

    /// Asks the other side for `change` in its folders; the answer comes from `answers`.
    pub fn change_remote(&mut self, change: Change) -> u32 {
        self.next_job += 1;
        let id = self.next_job;
        self.asks.insert(id);
        self.pending.push(FileMessage::Change { id, change });
        id
    }

    /// Asks the other side what its `path` holds, all levels down; the answer comes from `answers`.
    pub fn measure_remote(&mut self, path: &str) -> u32 {
        self.next_job += 1;
        let id = self.next_job;
        self.asks.insert(id);
        self.pending.push(FileMessage::Measure {
            id,
            path: path.to_string(),
        });
        id
    }

    /// The changes made here at the other side's request since the last call (lines for the session's log).
    pub fn served(&mut self) -> Vec<String> {
        std::mem::take(&mut self.served)
    }

    /// The other side's answers to this side's changes and counts since the last call (with the request's number).
    pub fn answers(&mut self) -> Vec<(u32, Answer)> {
        std::mem::take(&mut self.answers)
    }

    /// The other side's folders that arrived since the last call (with the request's number).
    pub fn listings(&mut self) -> Vec<(u32, Listing)> {
        std::mem::take(&mut self.listings)
    }

    /// A message from the other side; returns the replies to send (encoded).
    pub fn on_message(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let Some(message) = FileMessage::decode(bytes) else {
            return Vec::new();
        };
        match message {
            FileMessage::List { id, path } => self.serve_list(id, path),
            FileMessage::Fetch { id, path } => self.serve_fetch(id, path),
            FileMessage::Change { id, change } => self.serve_change(id, change),
            FileMessage::Measure { id, path } => self.serve_measure(id, path),
            FileMessage::Changed { id, error } => {
                if self.asks.remove(&id) {
                    let result = if error.is_empty() { Ok(()) } else { Err(error) };
                    self.answers.push((id, Answer::Changed(result)));
                }
                Vec::new()
            }
            FileMessage::Measured { id, size, error } => {
                if self.asks.remove(&id) {
                    let result = if error.is_empty() {
                        Ok(size)
                    } else {
                        Err(error)
                    };
                    self.answers.push((id, Answer::Measured(result)));
                }
                Vec::new()
            }
            FileMessage::Listing { id, listing, more } => {
                self.add_listing(id, listing, more);
                Vec::new()
            }
            FileMessage::Plan { id, files, bytes } => {
                if let Some(&job) = self.fetches.get(&id)
                    && let Some(entry) = self.job_mut(job)
                {
                    entry.job.files = entry.job.files.max(files);
                    entry.job.bytes = entry.job.bytes.max(bytes);
                    entry.known = true;
                }
                Vec::new()
            }
            FileMessage::Refused { id, reason } => {
                if let Some(&job) = self.fetches.get(&id)
                    && let Some(entry) = self.job_mut(job)
                {
                    entry.reason = reason;
                    entry.failed = entry.failed.max(1);
                    entry.known = true;
                } else if self.lists.contains(&id) {
                    self.add_listing(
                        id,
                        Listing {
                            error: reason,
                            ..Listing::default()
                        },
                        false,
                    );
                }
                Vec::new()
            }
            message => {
                if let FileMessage::Chunk { data, .. } = &message {
                    self.arrival_course.add(data.len());
                }
                if for_receiver(&message) {
                    self.note_incoming(&message);
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
        }
    }

    /// An announcement or offer of the other side: the job its file belongs to.
    fn note_incoming(&mut self, message: &FileMessage) {
        match message {
            FileMessage::From { id, request } => {
                if let Some(&job) = self.fetches.get(request)
                    && let Some(entry) = self.job_mut(job)
                    && entry.ids.len() < MAX_NOTES
                {
                    entry.ids.insert(*id);
                }
            }
            FileMessage::Offer { id, name, size } => {
                let known = self
                    .jobs
                    .iter()
                    .position(|entry| !entry.job.sending && entry.ids.contains(id));
                let index = match known {
                    Some(index) => index,
                    None => {
                        // Sent by the other side by itself: one job per file or folder while its files come.
                        let first = name
                            .split(['/', '\\'])
                            .find(|part| !part.is_empty())
                            .unwrap_or(name)
                            .to_string();
                        let open_job = self.loose.get(&first).copied().filter(|job| {
                            self.jobs
                                .iter()
                                .any(|e| e.job.id == *job && !e.job.finished())
                        });
                        let job = match open_job {
                            Some(job) => job,
                            None => {
                                let job = self.new_job(
                                    first.clone(),
                                    false,
                                    String::new(),
                                    String::new(),
                                );
                                if let Some(entry) = self.job_mut(job) {
                                    entry.known = true;
                                    entry.job.can_pause = false;
                                }
                                self.loose.insert(first, job);
                                job
                            }
                        };
                        let Some(index) = self.jobs.iter().position(|e| e.job.id == job) else {
                            return;
                        };
                        if self.jobs[index].ids.len() < MAX_NOTES {
                            self.jobs[index].ids.insert(*id);
                        }
                        index
                    }
                };
                let entry = &mut self.jobs[index];
                if entry.ids.contains(id) {
                    // Counted once per file (a Plan may have counted them already).
                    let offered = entry.ids.len() as u32;
                    entry.job.files = entry.job.files.max(offered);
                    if self.fetches.contains_key(&entry.job.id)
                        && entry.job.bytes > 0
                        && entry.known
                    {
                        // The Plan's total stands.
                    } else {
                        entry.job.bytes += size;
                    }
                }
            }
            _ => {}
        }
    }

    fn add_listing(&mut self, id: u32, part: Listing, more: bool) {
        // Only answers to this side's own requests.
        if !self.lists.contains(&id) {
            return;
        }
        let whole = self.listing_parts.entry(id).or_default();
        if whole.entries.is_empty() {
            whole.path = part.path;
            whole.parent = part.parent;
        }
        if !part.error.is_empty() {
            whole.error = part.error;
        }
        if whole.entries.len() < browse::MAX_ENTRIES {
            whole.entries.extend(part.entries);
        }
        if !more && let Some(done) = self.listing_parts.remove(&id) {
            self.lists.remove(&id);
            self.listings.push((id, done));
        }
    }

    /// The other side's List: answered by the worker (or refused here).
    fn serve_list(&mut self, id: u32, path: String) -> Vec<Vec<u8>> {
        if let Some(reason) = self.refusal(&path) {
            return vec![FileMessage::Refused { id, reason }.encode()];
        }
        self.next_work += 1;
        let work = self.next_work;
        self.work.insert(work, Purpose::ServeList(id));
        self.lister().list(work, &path);
        Vec::new()
    }

    /// The other side's Fetch: its files found by the worker, then sent (or refused here).
    fn serve_fetch(&mut self, id: u32, path: String) -> Vec<Vec<u8>> {
        let refusal = self.refusal(&path).or_else(|| {
            path.is_empty()
                .then(|| "choose files or folders to fetch".to_string())
        });
        if let Some(reason) = refusal {
            return vec![FileMessage::Refused { id, reason }.encode()];
        }
        self.ask_files(Path::new(&path), Purpose::ServeFetch(id));
        Vec::new()
    }

    /// The other side's Change: made by the worker (or refused here).
    fn serve_change(&mut self, id: u32, change: Change) -> Vec<Vec<u8>> {
        let path = match &change {
            Change::MakeFolder { folder, .. } => folder,
            Change::Rename { path, .. } | Change::Delete { path } => path,
        };
        let refusal = self.refusal(path).or_else(|| {
            path.is_empty()
                .then(|| "choose a folder of that PC first".to_string())
        });
        if let Some(error) = refusal {
            return vec![FileMessage::Changed { id, error }.encode()];
        }
        self.next_work += 1;
        let work = self.next_work;
        let described = match &change {
            Change::MakeFolder { folder, name } => {
                format!("new folder {}", browse::child(folder, name))
            }
            Change::Rename { path, name } => format!("renamed {path} to {name}"),
            Change::Delete { path } => format!("deleted {path}"),
        };
        self.work.insert(work, Purpose::ServeChange(id, described));
        self.lister().change(work, change);
        Vec::new()
    }

    /// The other side's Measure: counted by the worker (or refused here).
    fn serve_measure(&mut self, id: u32, path: String) -> Vec<Vec<u8>> {
        let refusal = self.refusal(&path).or_else(|| {
            path.is_empty()
                .then(|| "choose files or folders to count".to_string())
        });
        if let Some(error) = refusal {
            let size = Size::default();
            return vec![FileMessage::Measured { id, size, error }.encode()];
        }
        self.next_work += 1;
        let work = self.next_work;
        self.work.insert(work, Purpose::ServeMeasure(id));
        self.lister().measure(work, &path);
        Vec::new()
    }

    /// Why a List or Fetch of the other side is not answered, if it is not.
    fn refusal(&self, path: &str) -> Option<String> {
        if !self.serving {
            return Some("that PC does not share its files in this session".into());
        }
        if self.work.len() >= MAX_SERVING {
            return Some("that PC is busy: try again in a moment".into());
        }
        if !path.is_empty() && !Path::new(path).is_absolute() {
            return Some(format!("{path}: not a full path"));
        }
        None
    }

    /// What the worker found: listings to send, files to send.
    fn poll(&mut self) {
        let found = match &self.lister {
            Some(lister) => lister.found(),
            None => return,
        };
        for found in found {
            match found {
                Found::Listing(work, listing) => {
                    if let Some(Purpose::ServeList(request)) = self.work.remove(&work) {
                        self.pending.extend(listing_parts(request, listing));
                    }
                }
                Found::Changed(work, result) => {
                    if let Some(Purpose::ServeChange(request, described)) = self.work.remove(&work)
                    {
                        self.served.push(match &result {
                            Ok(()) => format!("files: the viewer's change made: {described}"),
                            Err(error) => {
                                format!("files: the viewer's change refused: {described}: {error}")
                            }
                        });
                        self.pending.push(FileMessage::Changed {
                            id: request,
                            error: result.err().unwrap_or_default(),
                        });
                    }
                }
                Found::Measured(work, result) => {
                    if let Some(Purpose::ServeMeasure(request)) = self.work.remove(&work) {
                        let (size, error) = match result {
                            Ok(size) => (size, String::new()),
                            Err(error) => (Size::default(), error),
                        };
                        self.pending.push(FileMessage::Measured {
                            id: request,
                            size,
                            error,
                        });
                    }
                }
                Found::Files(work, files) => match (self.work.remove(&work), files) {
                    (Some(Purpose::Send(job, folder, batch)), Ok(files)) => {
                        let count = files.len() as u32;
                        let bytes = files.iter().map(|f| f.2).sum();
                        let into = folder.clone();
                        let ids = self.outbox.add_files(files, 0, |id| {
                            let mut before = Vec::new();
                            if batch > 0 {
                                before.push(FileMessage::Batch { id, batch });
                            }
                            if !into.is_empty() {
                                before.push(FileMessage::Into {
                                    id,
                                    folder: into.clone(),
                                });
                            }
                            before
                        });
                        if let Some(entry) = self.job_mut(job) {
                            entry.ids.extend(ids);
                            entry.job.files = count;
                            entry.job.bytes = bytes;
                            entry.known = true;
                            let paused = entry.paused;
                            if paused {
                                let ids: Vec<u32> = entry.ids.iter().copied().collect();
                                for id in ids {
                                    self.outbox.pause(id, true);
                                }
                            }
                        }
                    }
                    (Some(Purpose::Send(job, _, _)), Err(reason)) => {
                        let name = self
                            .job_mut(job)
                            .map(|entry| {
                                entry.reason = reason.clone();
                                entry.failed = 1;
                                entry.known = true;
                                entry.job.files = 1;
                                entry.job.from.clone()
                            })
                            .unwrap_or_default();
                        self.outcomes.push(Outcome::Failed {
                            id: 0,
                            sending: true,
                            name,
                            reason,
                        });
                    }
                    (Some(Purpose::ServeFetch(request)), Ok(files)) => {
                        let count = files.len() as u32;
                        let bytes = files.iter().map(|f| f.2).sum();
                        self.pending.push(FileMessage::Plan {
                            id: request,
                            files: count,
                            bytes,
                        });
                        self.outbox.add_files(files, request, |id| {
                            vec![FileMessage::From { id, request }]
                        });
                    }
                    (Some(Purpose::ServeFetch(request)), Err(reason)) => {
                        self.pending.push(FileMessage::Refused {
                            id: request,
                            reason,
                        });
                    }
                    _ => {}
                },
            }
        }
    }

    /// Messages to send now (encoded), at most about `room` bytes of file data.
    pub fn take(&mut self, room: usize) -> Vec<Vec<u8>> {
        self.poll();
        if !self.open {
            return Vec::new();
        }
        let mut messages = std::mem::take(&mut self.pending);
        messages.extend(self.outbox.take(room));
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

    /// What finished or failed since the last call, both directions (the jobs follow).
    pub fn outcomes(&mut self) -> Vec<Outcome> {
        let mut all = std::mem::take(&mut self.outcomes);
        all.extend(self.outbox.take_outcomes());
        all.extend(self.inbox.take_outcomes());
        for outcome in &all {
            let (sending, id, size, failed, arrived) = match outcome {
                Outcome::Sent { id, size, .. } => (true, *id, *size, None, None),
                Outcome::Received { id, size, top, .. } => {
                    (false, *id, *size, None, Some(top.clone()))
                }
                Outcome::Failed {
                    id,
                    sending,
                    reason,
                    ..
                } => (*sending, *id, 0, Some(reason.clone()), None),
            };
            if id == 0 {
                continue;
            }
            if let Some(entry) = self
                .jobs
                .iter_mut()
                .find(|entry| entry.job.sending == sending && entry.ids.contains(&id))
            {
                entry.ids.remove(&id);
                match failed {
                    Some(reason) => {
                        entry.failed += 1;
                        if entry.reason.is_empty() {
                            entry.reason = reason;
                        }
                    }
                    None => {
                        entry.job.files_done += 1;
                        entry.finished_bytes += size;
                    }
                }
                if arrived.is_some() && entry.job.arrived.is_none() {
                    entry.job.arrived = arrived;
                }
            }
        }
        all
    }

    /// The jobs, oldest first, with where each stands now.
    pub fn jobs(&self) -> Vec<Job> {
        let mut going: HashMap<(bool, u32), (u64, bool)> = HashMap::new();
        for row in self.outbox.rows().into_iter().chain(self.inbox.rows()) {
            going.insert((row.sending, row.id), (row.done, row.paused));
        }
        self.jobs
            .iter()
            .map(|entry| {
                let mut job = entry.job.clone();
                let moving: u64 = entry
                    .ids
                    .iter()
                    .filter_map(|id| going.get(&(job.sending, *id)).map(|(done, _)| *done))
                    .sum();
                job.bytes_done = (entry.finished_bytes + moving)
                    .min(job.bytes.max(entry.finished_bytes + moving));
                let settled = job.files_done + entry.failed;
                job.state = if entry.cancelled {
                    JobState::Cancelled
                } else if entry.known && settled >= job.files && entry.ids.is_empty() {
                    if entry.failed > 0 {
                        JobState::Failed(entry.reason.clone())
                    } else {
                        JobState::Done
                    }
                } else if entry.paused {
                    JobState::Paused
                } else if job.bytes_done > 0 {
                    JobState::Going
                } else {
                    JobState::Waiting
                };
                job
            })
            .collect()
    }

    /// Pauses or resumes job `id` (its files this side sends, or asks the other side to hold those it sends).
    pub fn pause_job(&mut self, id: u32, paused: bool) {
        let Some(entry) = self.jobs.iter_mut().find(|entry| entry.job.id == id) else {
            return;
        };
        if !entry.job.can_pause || entry.job.finished() || entry.cancelled {
            return;
        }
        entry.paused = paused;
        if entry.job.sending {
            let ids: Vec<u32> = entry.ids.iter().copied().collect();
            for file in ids {
                self.outbox.pause(file, paused);
            }
        } else if self.fetches.contains_key(&id) {
            self.pending.push(FileMessage::Pause { id, paused });
        }
    }

    /// Stops job `id`: what arrived of its files is removed on the receiving side.
    pub fn cancel_job(&mut self, id: u32) {
        let Some(index) = self.jobs.iter().position(|entry| entry.job.id == id) else {
            return;
        };
        if self.jobs[index].cancelled {
            return;
        }
        self.jobs[index].cancelled = true;
        let ids: Vec<u32> = self.jobs[index].ids.iter().copied().collect();
        if self.jobs[index].job.sending {
            for file in ids {
                self.outbox.cancel(file);
            }
            // Files still being found are dropped when found.
            self.work
                .retain(|_, purpose| !matches!(purpose, Purpose::Send(job, _, _) if *job == id));
        } else {
            if self.fetches.contains_key(&id) {
                self.pending.push(FileMessage::Drop { id });
            }
            for file in ids {
                let replies = self.inbox.cancel(file);
                self.pending.extend(replies);
            }
        }
    }

    pub fn pause_all(&mut self, paused: bool) {
        let ids: Vec<u32> = self.jobs.iter().map(|entry| entry.job.id).collect();
        for id in ids {
            self.pause_job(id, paused);
        }
    }

    pub fn cancel_all(&mut self) {
        let unfinished: Vec<u32> = self
            .jobs()
            .into_iter()
            .filter(|job| !job.finished())
            .map(|job| job.id)
            .collect();
        for id in unfinished {
            self.cancel_job(id);
        }
    }

    /// Takes the finished jobs off the list (job `only`, or all).
    pub fn clear_finished(&mut self, only: Option<u32>) {
        let finished: HashSet<u32> = self
            .jobs()
            .into_iter()
            .filter(|job| job.finished() && only.is_none_or(|id| id == job.id))
            .map(|job| job.id)
            .collect();
        self.jobs.retain(|entry| !finished.contains(&entry.job.id));
        self.loose.retain(|_, job| !finished.contains(job));
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

    /// Files still on their way (either direction), or being found.
    pub fn busy(&self) -> bool {
        !self.outbox.progress().is_empty()
            || !self.inbox.progress().is_empty()
            || self
                .work
                .values()
                .any(|purpose| matches!(purpose, Purpose::Send(..) | Purpose::ServeFetch(_)))
    }
}

/// A listing as messages small enough for the channel (the last one with `more` false).
fn listing_parts(id: u32, listing: Listing) -> Vec<FileMessage> {
    let mut parts = Vec::new();
    let mut current = Vec::new();
    let mut bytes = 0usize;
    let Listing {
        path,
        parent,
        entries,
        error,
    } = listing;
    let make = |entries: Vec<Entry>, more: bool| FileMessage::Listing {
        id,
        listing: Listing {
            path: path.clone(),
            parent: parent.clone(),
            entries,
            error: if more { String::new() } else { error.clone() },
        },
        more,
    };
    for entry in entries {
        bytes += entry.name.len() + 20;
        current.push(entry);
        if current.len() >= LISTING_PART_ENTRIES || bytes >= LISTING_PART_BYTES {
            parts.push(make(std::mem::take(&mut current), true));
            bytes = 0;
        }
    }
    parts.push(make(current, false));
    parts
}

/// The person's folder `key` from the text of `user-dirs.dirs` (`XDG_DOWNLOAD_DIR` and the like): a quoted path,
/// absolute or starting with `$HOME/`; None for none of its own ("$HOME").
#[cfg(unix)]
pub(crate) fn xdg_dir(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let prefix = format!("{key}=");
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with(&prefix))?;
    let value = line[prefix.len()..].trim().trim_matches('"');
    let path = if let Some(rest) = value.strip_prefix("$HOME/") {
        home.join(rest)
    } else if value == "$HOME" {
        // The desktop's way of saying it has no such folder of its own.
        return None;
    } else {
        PathBuf::from(value)
    };
    path.is_absolute().then_some(path)
}
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
#[cfg(windows)]
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

/// The person's download folder as the desktop names it (`XDG_DOWNLOAD_DIR` in `user-dirs.dirs`, translated names
/// included), else `~/Downloads`; then `Queqiao` inside it.
#[cfg(unix)]
pub fn downloads_folder() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()));
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let named = std::fs::read_to_string(config.join("user-dirs.dirs"))
        .ok()
        .and_then(|text| xdg_download_dir(&text, &home));
    named
        .unwrap_or_else(|| home.join("Downloads"))
        .join("Queqiao")
}

/// `XDG_DOWNLOAD_DIR` from the text of `user-dirs.dirs`: a quoted path, absolute or starting with `$HOME/`.
#[cfg(unix)]
fn xdg_download_dir(text: &str, home: &Path) -> Option<PathBuf> {
    xdg_dir(text, "XDG_DOWNLOAD_DIR", home)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The origin mark a received file got on Linux (`user.xdg.origin.url`), if any.
    #[cfg(unix)]
    fn origin_mark(path: &Path) -> Option<String> {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut buffer = [0u8; 64];
        let read = unsafe {
            libc::getxattr(
                name.as_ptr(),
                c"user.xdg.origin.url".as_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        (read >= 0).then(|| String::from_utf8_lossy(&buffer[..read as usize]).into_owned())
    }

    /// Whether the file system of `path` keeps user attributes: one set and read back on it.
    #[cfg(unix)]
    fn user_attributes_kept(path: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let set = unsafe {
            libc::setxattr(
                name.as_ptr(),
                c"user.queqiao.probe".as_ptr(),
                b"1".as_ptr().cast(),
                1,
                0,
            )
        };
        set == 0
    }

    #[cfg(unix)]
    #[test]
    fn download_folder_as_the_desktop_names_it() {
        let home = Path::new("/home/someone");
        let text = "# written by xdg-user-dirs-update\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/下载\"\n";
        assert_eq!(xdg_download_dir(text, home), Some(home.join("下载")));
        assert_eq!(
            xdg_download_dir("XDG_DOWNLOAD_DIR=\"/data/dl\"", home),
            Some(PathBuf::from("/data/dl"))
        );
        // No download folder of its own, a relative path, none at all: the fallback.
        assert_eq!(xdg_download_dir("XDG_DOWNLOAD_DIR=\"$HOME\"", home), None);
        assert_eq!(xdg_download_dir("XDG_DOWNLOAD_DIR=\"dl\"", home), None);
        assert_eq!(xdg_download_dir("", home), None);
        assert!(downloads_folder().ends_with("Queqiao"));
    }

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
            #[cfg(windows)]
            {
                let mut stream = path.as_os_str().to_owned();
                stream.push(":Zone.Identifier");
                let mark =
                    std::fs::read_to_string(std::path::PathBuf::from(stream)).unwrap_or_default();
                assert!(mark.contains("ZoneId=3"), "{mark:?}");
            }
            // Where the file system keeps user attributes (ext4, xfs, tmpfs do), the origin must be there.
            #[cfg(unix)]
            if user_attributes_kept(&source) {
                assert_eq!(origin_mark(path).as_deref(), Some("queqiao://transfer"));
            }
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

/// Browsing and jobs (the file transfer window): two sides' `Transfers` talking as over the files channel.
#[cfg(test)]
mod browsing {
    use super::*;

    /// Messages both ways until nothing moves for a while (the worker threads answer on their own time).
    fn pump(viewer: &mut Transfers, agent: &mut Transfers) {
        let (mut to_agent, mut to_viewer): (VecDeque<Vec<u8>>, VecDeque<Vec<u8>>) =
            (VecDeque::new(), VecDeque::new());
        let mut quiet = 0;
        for _ in 0..50_000 {
            to_agent.extend(viewer.take(256 * 1024));
            to_viewer.extend(agent.take(256 * 1024));
            let moved = !to_agent.is_empty() || !to_viewer.is_empty();
            while let Some(message) = to_agent.pop_front() {
                to_viewer.extend(agent.on_message(&message));
            }
            while let Some(message) = to_viewer.pop_front() {
                to_agent.extend(viewer.on_message(&message));
            }
            let _ = (viewer.outcomes(), agent.outcomes());
            if moved || viewer.busy() || agent.busy() || !agent.work.is_empty() {
                quiet = 0;
            } else {
                quiet += 1;
                if quiet > 40 {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("the sides did not settle");
    }

    fn sides(root: &Path, serving: bool) -> (Transfers, Transfers) {
        let mut viewer = Transfers::new(root.join("viewer_downloads"), true);
        let mut agent = Transfers::new(root.join("agent_downloads"), true);
        agent.serve_browsing(serving);
        viewer.opened();
        agent.opened();
        (viewer, agent)
    }

    #[test]
    fn the_controller_lists_fetches_and_sends_into_a_folder_there() {
        let root = std::env::temp_dir().join(format!("queqiao-browsing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let share = root.join("there").join("share");
        std::fs::create_dir_all(share.join("sub")).unwrap();
        std::fs::write(share.join("a.txt"), b"alpha").unwrap();
        std::fs::write(share.join("sub").join("b.bin"), vec![9u8; 70_000]).unwrap();
        let target = root.join("there").join("target");
        std::fs::create_dir_all(&target).unwrap();
        let local = root.join("here").join("in");
        std::fs::create_dir_all(&local).unwrap();
        let upload = root.join("here").join("up.txt");
        std::fs::write(&upload, b"upload").unwrap();
        let (mut viewer, mut agent) = sides(&root, true);

        let asked = viewer.list_remote(&share.to_string_lossy());
        pump(&mut viewer, &mut agent);
        let listings = viewer.listings();
        let (id, listing) = listings.first().expect("a listing");
        assert_eq!(*id, asked);
        assert!(listing.error.is_empty(), "{}", listing.error);
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["sub", "a.txt"]);
        assert_eq!(Path::new(&listing.parent), share.parent().unwrap());

        let fetched = viewer.fetch(&share.to_string_lossy(), &local);
        pump(&mut viewer, &mut agent);
        assert_eq!(
            std::fs::read(local.join("share").join("a.txt")).unwrap(),
            b"alpha"
        );
        assert_eq!(
            std::fs::read(local.join("share").join("sub").join("b.bin"))
                .unwrap()
                .len(),
            70_000
        );
        let job = viewer
            .jobs()
            .into_iter()
            .find(|job| job.id == fetched)
            .unwrap();
        assert_eq!(job.state, JobState::Done, "{job:?}");
        assert_eq!((job.files, job.files_done, job.bytes), (2, 2, 70_005));
        assert_eq!(job.bytes_done, 70_005);
        assert_eq!(job.arrived.as_deref(), Some(local.join("share").as_path()));

        let sent = viewer.send_into(&upload, &target.to_string_lossy());
        pump(&mut viewer, &mut agent);
        assert_eq!(std::fs::read(target.join("up.txt")).unwrap(), b"upload");
        let job = viewer
            .jobs()
            .into_iter()
            .find(|job| job.id == sent)
            .unwrap();
        assert_eq!(job.state, JobState::Done, "{job:?}");
        // Nothing went into the downloads folders.
        assert!(!root.join("viewer_downloads").exists());
        assert!(!root.join("agent_downloads").exists());
        // Finished jobs leave the list when cleared.
        viewer.clear_finished(None);
        assert!(viewer.jobs().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nothing_is_browsed_without_leave_and_the_controller_is_never_browsed() {
        let root = std::env::temp_dir().join(format!("queqiao-browsing-no-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("folder")).unwrap();
        let file = root.join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        let (mut viewer, mut agent) = sides(&root, false);
        viewer.list_remote(&root.to_string_lossy());
        let fetched = viewer.fetch(&file.to_string_lossy(), &root.join("folder"));
        let sent = viewer.send_into(&file, &root.join("folder").to_string_lossy());
        pump(&mut viewer, &mut agent);
        let listings = viewer.listings();
        assert!(
            listings[0].1.error.contains("does not share"),
            "{listings:?}"
        );
        assert!(listings[0].1.entries.is_empty());
        let jobs = viewer.jobs();
        for id in [fetched, sent] {
            let job = jobs.iter().find(|job| job.id == id).unwrap();
            assert!(matches!(job.state, JobState::Failed(_)), "{job:?}");
        }
        assert!(!root.join("folder").join("f.txt").exists());
        // The controlled side asking the controller: refused, and its Into is refused too.
        agent.list_remote(&root.to_string_lossy());
        let into = agent.send_into(&file, &root.join("folder").to_string_lossy());
        pump(&mut viewer, &mut agent);
        assert!(!agent.listings()[0].1.error.is_empty());
        let job = agent.jobs().into_iter().find(|job| job.id == into).unwrap();
        assert!(matches!(job.state, JobState::Failed(_)), "{job:?}");
        assert!(!root.join("folder").join("f.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_controller_changes_and_counts_folders_there_only_with_leave() {
        let root =
            std::env::temp_dir().join(format!("queqiao-browsing-change-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"abc").unwrap();
        let folder = root.to_string_lossy().into_owned();
        let at = |name: &str| root.join(name).to_string_lossy().into_owned();
        let (mut viewer, mut agent) = sides(&root, true);
        let made = viewer.change_remote(Change::MakeFolder {
            folder: folder.clone(),
            name: "new".into(),
        });
        let renamed = viewer.change_remote(Change::Rename {
            path: at("a.txt"),
            name: "b.txt".into(),
        });
        let twice = viewer.change_remote(Change::MakeFolder {
            folder: folder.clone(),
            name: "new".into(),
        });
        pump(&mut viewer, &mut agent);
        let answers = viewer.answers();
        assert_eq!(answers.len(), 3, "{answers:?}");
        assert!(
            answers.contains(&(made, Answer::Changed(Ok(())))),
            "{answers:?}"
        );
        assert!(
            answers.contains(&(renamed, Answer::Changed(Ok(())))),
            "{answers:?}"
        );
        assert!(
            answers
                .iter()
                .any(|(id, answer)| *id == twice && matches!(answer, Answer::Changed(Err(_)))),
            "{answers:?}"
        );
        assert!(root.join("new").is_dir() && root.join("b.txt").is_file());
        let counted = viewer.measure_remote(&folder);
        pump(&mut viewer, &mut agent);
        let answers = viewer.answers();
        let Some((id, Answer::Measured(Ok(size)))) = answers.first() else {
            panic!("{answers:?}");
        };
        assert_eq!(
            (*id, size.files, size.folders, size.bytes),
            (counted, 1, 1, 3)
        );
        let deleted = viewer.change_remote(Change::Delete { path: at("new") });
        pump(&mut viewer, &mut agent);
        assert_eq!(viewer.answers(), [(deleted, Answer::Changed(Ok(())))]);
        assert!(!root.join("new").exists());
        // Without leave, nothing changes; and the controlled side never changes the controller's files.
        let (mut viewer, mut agent) = sides(&root, false);
        let refused = viewer.change_remote(Change::Delete { path: at("b.txt") });
        let uncounted = viewer.measure_remote(&folder);
        pump(&mut viewer, &mut agent);
        let answers = viewer.answers();
        assert!(answers.iter().any(|(id, a)| *id == refused
            && matches!(a, Answer::Changed(Err(e)) if e.contains("does not share"))));
        assert!(
            answers
                .iter()
                .any(|(id, a)| *id == uncounted && matches!(a, Answer::Measured(Err(_))))
        );
        agent.change_remote(Change::Delete { path: at("b.txt") });
        pump(&mut viewer, &mut agent);
        assert!(matches!(
            agent.answers()[..],
            [(_, Answer::Changed(Err(_)))]
        ));
        assert!(root.join("b.txt").is_file());
        // A relative path is refused before it reaches the disk.
        let (mut viewer, mut agent) = sides(&root, true);
        viewer.change_remote(Change::Delete {
            path: "b.txt".into(),
        });
        pump(&mut viewer, &mut agent);
        assert!(matches!(
            viewer.answers()[..],
            [(_, Answer::Changed(Err(_)))]
        ));
        assert!(root.join("b.txt").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn change_messages_survive_the_wire() {
        let size = Size {
            files: 7,
            folders: 2,
            bytes: 1 << 40,
            complete: false,
        };
        for message in [
            FileMessage::Change {
                id: 1,
                change: Change::MakeFolder {
                    folder: "C:\\x".into(),
                    name: "新建".into(),
                },
            },
            FileMessage::Change {
                id: 2,
                change: Change::Rename {
                    path: "/a/b".into(),
                    name: "c".into(),
                },
            },
            FileMessage::Change {
                id: 3,
                change: Change::Delete {
                    path: "/a/b".into(),
                },
            },
            FileMessage::Changed {
                id: 4,
                error: String::new(),
            },
            FileMessage::Changed {
                id: 5,
                error: "no".into(),
            },
            FileMessage::Measure {
                id: 6,
                path: "/".into(),
            },
            FileMessage::Measured {
                id: 7,
                size,
                error: String::new(),
            },
        ] {
            assert_eq!(
                FileMessage::decode(&message.encode()),
                Some(message.clone())
            );
        }
        // An unknown kind of change is not a message.
        let mut bytes = FileMessage::Change {
            id: 1,
            change: Change::Delete { path: "/x".into() },
        }
        .encode();
        bytes[5] = 99;
        assert_eq!(FileMessage::decode(&bytes), None);
    }

    #[test]
    fn a_folder_of_many_files_arrives_whole_and_a_job_can_be_cancelled() {
        let root =
            std::env::temp_dir().join(format!("queqiao-browsing-many-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let folder = root.join("many");
        std::fs::create_dir_all(&folder).unwrap();
        for index in 0..40 {
            std::fs::write(folder.join(format!("{index}.txt")), format!("file {index}")).unwrap();
        }
        let target = root.join("target");
        std::fs::create_dir_all(&target).unwrap();
        let (mut viewer, mut agent) = sides(&root, true);
        let job = viewer.send_into(&folder, &target.to_string_lossy());
        pump(&mut viewer, &mut agent);
        let state = viewer.jobs().into_iter().find(|j| j.id == job).unwrap();
        assert_eq!(state.state, JobState::Done, "{state:?}");
        assert_eq!(state.files_done, 40);
        assert_eq!(std::fs::read_dir(target.join("many")).unwrap().count(), 40);
        // A cancelled fetch leaves nothing behind.
        let big = root.join("big.bin");
        std::fs::write(&big, vec![1u8; 4 * 1024 * 1024]).unwrap();
        let local = root.join("local");
        std::fs::create_dir_all(&local).unwrap();
        let fetch = viewer.fetch(&big.to_string_lossy(), &local);
        for _ in 0..20 {
            for message in viewer.take(64 * 1024) {
                for reply in agent.on_message(&message) {
                    viewer.on_message(&reply);
                }
            }
            for message in agent.take(64 * 1024) {
                for reply in viewer.on_message(&message) {
                    agent.on_message(&reply);
                }
            }
            if viewer.rows().iter().any(|row| row.done > 0) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            viewer.rows().iter().any(|row| row.done > 0),
            "the fetch began"
        );
        viewer.cancel_job(fetch);
        pump(&mut viewer, &mut agent);
        let state = viewer.jobs().into_iter().find(|j| j.id == fetch).unwrap();
        assert_eq!(state.state, JobState::Cancelled);
        let left: Vec<_> = std::fs::read_dir(&local)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert!(left.is_empty(), "{left:?}");
        assert!(!agent.busy(), "the sending side dropped it too");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_listing_survives_the_wire_in_parts() {
        let listing = Listing {
            path: "C:\\Users\\me".into(),
            parent: "C:\\Users".into(),
            entries: (0..1234u64)
                .map(|index| Entry {
                    name: format!("名字 {index}.txt"),
                    kind: if index % 3 == 0 {
                        EntryKind::Folder
                    } else {
                        EntryKind::File
                    },
                    size: index * 1000,
                    modified: 1_764_000_000 + index as i64,
                })
                .collect(),
            error: String::new(),
        };
        let mut viewer = Transfers::new(std::env::temp_dir(), true);
        let id = viewer.list_remote("C:\\Users\\me");
        // An answer to a request this side never made is dropped.
        viewer.on_message(&listing_parts(id + 1, listing.clone())[0].encode());
        let parts = listing_parts(id, listing.clone());
        assert_eq!(parts.len(), 3);
        for part in &parts {
            assert!(part.encode().len() < 64 * 1024);
            viewer.on_message(&part.encode());
        }
        assert_eq!(viewer.listings(), vec![(id, listing)]);
    }
}
