//! Clipboard images between the two sides, on the session's "clipboard" stream: compressed with LZ4 (the block
//! format; the size travels with the image), sent in pieces, checked with SHA-256 at the end. A newer image
//! replaces one not yet sent.
//!
//! LZ4 since 2026-11-24, on Windows and Linux alike (Windows' XPRESS Huffman before: Linux has no decoder for it).
//! Decoding is `lz4_flex`'s safe Rust, as the data comes from the other side.

use std::collections::VecDeque;

use sha2::{Digest, Sha256};

/// Images larger than this (uncompressed) are not sent or accepted: 64 MB, an 8K screenshot.
pub const MAX_IMAGE: usize = 64 << 20;
const PIECE: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipMessage {
    /// A new image: its size uncompressed and compressed.
    Start {
        id: u32,
        size: u32,
        compressed: u32,
    },
    Piece {
        id: u32,
        offset: u32,
        data: Vec<u8>,
    },
    /// All pieces sent: the SHA-256 of the image (uncompressed).
    End {
        id: u32,
        sha256: [u8; 32],
    },
}

const START: u8 = 1;
const PIECE_TAG: u8 = 2;
const END: u8 = 3;

impl ClipMessage {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            ClipMessage::Start {
                id,
                size,
                compressed,
            } => {
                out.push(START);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&size.to_le_bytes());
                out.extend_from_slice(&compressed.to_le_bytes());
            }
            ClipMessage::Piece { id, offset, data } => {
                out.push(PIECE_TAG);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(&offset.to_le_bytes());
                out.extend_from_slice(data);
            }
            ClipMessage::End { id, sha256 } => {
                out.push(END);
                out.extend_from_slice(&id.to_le_bytes());
                out.extend_from_slice(sha256);
            }
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<ClipMessage> {
        let (&tag, rest) = bytes.split_first()?;
        let u32_at = |at: usize| Some(u32::from_le_bytes(rest.get(at..at + 4)?.try_into().ok()?));
        let id = u32_at(0)?;
        Some(match tag {
            START if rest.len() == 12 => ClipMessage::Start {
                id,
                size: u32_at(4)?,
                compressed: u32_at(8)?,
            },
            PIECE_TAG => ClipMessage::Piece {
                id,
                offset: u32_at(4)?,
                data: rest.get(8..)?.to_vec(),
            },
            END if rest.len() == 36 => ClipMessage::End {
                id,
                sha256: rest[4..36].try_into().ok()?,
            },
            _ => return None,
        })
    }
}

fn compress(data: &[u8]) -> Result<Vec<u8>, String> {
    Ok(lz4_flex::block::compress(data))
}

/// Unpacks an image the other side says is `size` bytes (at most MAX_IMAGE, checked before): never more is
/// allocated or written, and anything but exactly `size` bytes is refused.
fn decompress(data: &[u8], size: usize) -> Result<Vec<u8>, String> {
    if size > MAX_IMAGE {
        return Err(format!("an image of {size} bytes is too large"));
    }
    let mut out = vec![0u8; size];
    let written = lz4_flex::block::decompress_into(data, &mut out)
        .map_err(|e| format!("the image did not unpack: {e}"))?;
    if written != size {
        return Err(format!("the image unpacked to {written} bytes, not {size}"));
    }
    Ok(out)
}

/// Images to send (the newest only).
#[derive(Default)]
pub struct ImageSender {
    next_id: u32,
    queue: VecDeque<ClipMessage>,
}

impl ImageSender {
    /// Queues `image` (CF_DIB bytes), replacing one not yet sent.
    pub fn send(&mut self, image: &[u8]) -> Result<(), String> {
        if image.is_empty() || image.len() > MAX_IMAGE {
            return Err(format!("an image of {} bytes is not sent", image.len()));
        }
        let packed = compress(image)?;
        self.next_id = self.next_id.wrapping_add(1);
        let id = self.next_id;
        self.queue.clear();
        self.queue.push_back(ClipMessage::Start {
            id,
            size: image.len() as u32,
            compressed: packed.len() as u32,
        });
        for (index, piece) in packed.chunks(PIECE).enumerate() {
            self.queue.push_back(ClipMessage::Piece {
                id,
                offset: (index * PIECE) as u32,
                data: piece.to_vec(),
            });
        }
        self.queue.push_back(ClipMessage::End {
            id,
            sha256: Sha256::digest(image).into(),
        });
        Ok(())
    }

    /// Messages to send now (encoded), about `room` bytes at most.
    pub fn take(&mut self, room: usize) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut left = room;
        while let Some(message) = self.queue.front() {
            let length = message.encode().len();
            if length > left && !out.is_empty() {
                break;
            }
            if length > left {
                break;
            }
            left -= length;
            if let Some(message) = self.queue.pop_front() {
                out.push(message.encode());
            }
        }
        out
    }
}

/// The image coming in.
#[derive(Default)]
pub struct ImageReceiver {
    current: Option<(u32, usize, Vec<u8>, usize)>,
}

impl ImageReceiver {
    /// A message from the other side; the whole image (CF_DIB bytes) once it arrived and checked out, or why it
    /// was dropped.
    pub fn handle(&mut self, bytes: &[u8]) -> Option<Result<Vec<u8>, String>> {
        match ClipMessage::decode(bytes)? {
            ClipMessage::Start {
                id,
                size,
                compressed,
            } => {
                let (size, compressed) = (size as usize, compressed as usize);
                // Sizes are the other side's word: within limits, or the image is not taken.
                if size == 0
                    || size > MAX_IMAGE
                    || compressed == 0
                    || compressed > MAX_IMAGE + MAX_IMAGE / 8 + 4096
                {
                    self.current = None;
                    return Some(Err(format!("an image of {size} bytes is refused")));
                }
                self.current = Some((
                    id,
                    size,
                    Vec::with_capacity(compressed.min(1 << 20)),
                    compressed,
                ));
                None
            }
            ClipMessage::Piece { id, offset, data } => {
                let fits = matches!(&self.current, Some((current, _, packed, expected))
                    if *current == id && offset as usize == packed.len() && packed.len() + data.len() <= *expected);
                if !fits {
                    let was_ours = self
                        .current
                        .as_ref()
                        .is_some_and(|(current, ..)| *current == id);
                    if was_ours {
                        self.current = None;
                        return Some(Err("a piece of the image did not fit".into()));
                    }
                    return None;
                }
                if let Some((_, _, packed, _)) = &mut self.current {
                    packed.extend_from_slice(&data);
                }
                None
            }
            ClipMessage::End { id, sha256 } => {
                let (current, size, packed, expected) = self.current.take()?;
                if current != id {
                    self.current = Some((current, size, packed, expected));
                    return None;
                }
                if packed.len() != expected {
                    return Some(Err("the image arrived incomplete".into()));
                }
                Some(decompress(&packed, size).and_then(|image| {
                    let hash: [u8; 32] = Sha256::digest(&image).into();
                    if hash == sha256 {
                        Ok(image)
                    } else {
                        Err("the image changed on the way (SHA-256 differs)".into())
                    }
                }))
            }
        }
    }
}

/// A short fingerprint of an image, to tell the one last received from a new one (no echo between the sides).
pub fn image_fingerprint(image: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    image.hash(&mut hasher);
    hasher.finish()
}

/// A test picture (tests of the clipboard): `seed` makes the two sides' pictures differ.
pub fn test_picture(seed: u8) -> Vec<u8> {
    let mut dib = picture_dib(640, 400);
    for (index, byte) in dib.iter_mut().enumerate().skip(40) {
        if index % 4 == 2 {
            *byte = byte.wrapping_add(seed);
        }
    }
    dib
}

/// A BITMAPINFOHEADER and a gradient, as a DIB on the clipboard looks.
fn picture_dib(width: u32, height: u32) -> Vec<u8> {
    let mut dib = Vec::new();
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&width.to_le_bytes());
    dib.extend_from_slice(&height.to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&[0u8; 24]);
    for y in 0..height {
        for x in 0..width {
            dib.extend_from_slice(&[
                (x % 256) as u8,
                (y % 256) as u8,
                ((x + y) % 256) as u8,
                0xFF,
            ]);
        }
    }
    dib
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: u32, height: u32) -> Vec<u8> {
        picture_dib(width, height)
    }

    #[test]
    fn an_image_arrives_whole_and_smaller_on_the_way() {
        let image = picture(1280, 720);
        let mut sender = ImageSender::default();
        sender.send(&image).unwrap();
        let mut receiver = ImageReceiver::default();
        let mut wire = 0;
        let mut received = None;
        loop {
            let messages = sender.take(64 * 1024);
            if messages.is_empty() {
                break;
            }
            for message in messages {
                wire += message.len();
                if let Some(result) = receiver.handle(&message) {
                    received = Some(result);
                }
            }
        }
        assert_eq!(received, Some(Ok(image.clone())));
        assert!(
            wire * 4 < image.len(),
            "{wire} bytes on the way for {} bytes",
            image.len()
        );
    }

    #[test]
    fn a_changed_piece_or_a_huge_size_is_refused() {
        let image = picture(320, 200);
        let mut sender = ImageSender::default();
        sender.send(&image).unwrap();
        let mut messages = sender.take(usize::MAX);
        // Change a byte of the first piece.
        let piece = &mut messages[1];
        let last = piece.len() - 1;
        piece[last] ^= 0x55;
        let mut receiver = ImageReceiver::default();
        let results: Vec<_> = messages.iter().filter_map(|m| receiver.handle(m)).collect();
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
        let huge = ClipMessage::Start {
            id: 9,
            size: u32::MAX,
            compressed: 10,
        };
        assert!(matches!(receiver.handle(&huge.encode()), Some(Err(_))));
    }

    #[test]
    fn random_messages_never_panic() {
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut receiver = ImageReceiver::default();
        for _ in 0..100_000 {
            let length = (next() % 48) as usize;
            let mut bytes: Vec<u8> = (0..length).map(|_| next() as u8).collect();
            if !bytes.is_empty() {
                bytes[0] = [START, PIECE_TAG, END][(next() % 3) as usize];
            }
            let _ = receiver.handle(&bytes);
            if let Some(message) = ClipMessage::decode(&bytes) {
                assert_eq!(ClipMessage::decode(&message.encode()), Some(message));
            }
        }
    }
}
