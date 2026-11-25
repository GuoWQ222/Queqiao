//! Folders for the file transfer window (UU Remote's two panes: this PC's files on the left, the other PC's on the
//! right): what a folder holds, the places to start from (the person's own folders and the drives), and a worker
//! thread that reads them, so a slow disk or a network drive never holds the session (its picture and input).
//!
//! The other side asks with [`crate::FileMessage::List`] and gets [`crate::FileMessage::Listing`]; only the controlled
//! side answers, and only while it allows files (see `Transfers::serve_browsing`). Paths travel as that side writes
//! them ("C:\Users\..." or "/home/..."): the asking side never takes them apart with its own path rules, it asks for
//! a folder's parent and joins a name with [`child`].

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::UNIX_EPOCH;

/// Entries of one folder at most (the rest are left out, and the listing says so).
pub const MAX_ENTRIES: usize = 20_000;
/// Files a fetched or sent folder may hold at most.
pub const MAX_FOLDER_FILES: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Folder,
    /// A drive ("C:\") or the file system's root ("/"): in the places list.
    Drive,
    /// One of the person's folders (Desktop, Documents, Downloads, the home folder): in the places list.
    Place,
}

impl EntryKind {
    pub fn code(self) -> u8 {
        match self {
            EntryKind::File => 0,
            EntryKind::Folder => 1,
            EntryKind::Drive => 2,
            EntryKind::Place => 3,
        }
    }

    pub fn from_code(code: u8) -> Option<EntryKind> {
        Some(match code {
            0 => EntryKind::File,
            1 => EntryKind::Folder,
            2 => EntryKind::Drive,
            3 => EntryKind::Place,
            _ => return None,
        })
    }

    /// Something that can be opened (a folder, a drive, a place).
    pub fn is_container(self) -> bool {
        self != EntryKind::File
    }
}

/// One entry of a folder. In the places list (path "") the name is the full path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    /// Bytes (files; 0 for the others).
    pub size: u64,
    /// Last change, seconds since 1970 (UTC); 0 when unknown.
    pub modified: i64,
}

/// What a folder holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    /// The folder as that side writes it ("" : the places).
    pub path: String,
    /// The folder above ("" : the places, from a drive or the root).
    pub parent: String,
    /// Folders first, then files, each by name (upper and lower case alike).
    pub entries: Vec<Entry>,
    /// Why the folder could not be read (empty: it was).
    pub error: String,
}

/// `name` inside the folder `path` as that side writes paths: with its separator ("\" where the path has one or a
/// drive letter, else "/"). In the places list (`path` empty) names are full paths already.
pub fn child(path: &str, name: &str) -> String {
    if path.is_empty() {
        return name.to_string();
    }
    if path.ends_with(['/', '\\']) {
        return format!("{path}{name}");
    }
    let windows = path.contains('\\') || path.as_bytes().get(1) == Some(&b':');
    format!("{path}{}{name}", if windows { '\\' } else { '/' })
}

/// The last part of a path as that side writes it ("C:\Users\me\Desktop" -> "Desktop", "C:\" -> "C:\").
pub fn last_part(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(at) if at + 1 < trimmed.len() => &trimmed[at + 1..],
        _ if trimmed.is_empty() => path,
        _ => trimmed,
    }
}

/// What the folder `path` holds ("" : the places).
pub fn list(path: &str) -> Listing {
    if path.is_empty() {
        return Listing {
            entries: places(),
            ..Listing::default()
        };
    }
    let folder = Path::new(path);
    let mut listing = Listing {
        path: path.to_string(),
        parent: parent_of(folder),
        ..Listing::default()
    };
    if !folder.is_absolute() {
        listing.error = "not a full path".into();
        return listing;
    }
    let reader = match std::fs::read_dir(folder) {
        Ok(reader) => reader,
        Err(error) => {
            listing.error = error.to_string();
            return listing;
        }
    };
    for entry in reader.flatten() {
        if listing.entries.len() >= MAX_ENTRIES {
            listing.error = format!("only the first {MAX_ENTRIES} entries are shown");
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // A link counts as what it points to (a home folder's links to other disks); a broken one is left out.
        let Ok(meta) = std::fs::metadata(entry.path()) else {
            continue;
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_secs() as i64);
        listing.entries.push(Entry {
            name,
            kind: if meta.is_dir() {
                EntryKind::Folder
            } else {
                EntryKind::File
            },
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified,
        });
    }
    sort(&mut listing.entries);
    listing
}

/// Folders first, then files, each by name (upper and lower case alike).
pub fn sort(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        b.kind
            .is_container()
            .cmp(&a.kind.is_container())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn parent_of(folder: &Path) -> String {
    match folder.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_string_lossy().into_owned(),
        _ => String::new(),
    }
}

/// The person's own folders that exist, then the drives (Windows) or the root (elsewhere).
pub fn places() -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for folder in own_folders() {
        let name = folder.to_string_lossy().into_owned();
        if folder.is_dir() && !entries.iter().any(|entry| entry.name == name) {
            entries.push(Entry {
                name,
                kind: EntryKind::Place,
                size: 0,
                modified: 0,
            });
        }
    }
    for drive in drives() {
        entries.push(Entry {
            name: drive,
            kind: EntryKind::Drive,
            size: 0,
            modified: 0,
        });
    }
    entries
}

/// Desktop, Documents, Downloads (where Windows has them, also when moved), then the home folder.
#[cfg(windows)]
fn own_folders() -> Vec<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Profile,
        KF_FLAG_DEFAULT, SHGetKnownFolderPath,
    };
    [
        &FOLDERID_Desktop,
        &FOLDERID_Documents,
        &FOLDERID_Downloads,
        &FOLDERID_Profile,
    ]
    .into_iter()
    .filter_map(|id| {
        let path = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
        let text = unsafe { path.to_string() }.unwrap_or_default();
        unsafe { CoTaskMemFree(Some(path.0.cast())) };
        (!text.is_empty()).then(|| PathBuf::from(text))
    })
    .collect()
}

/// The desktop's own folders (user-dirs.dirs, translated names included), then the home folder.
#[cfg(unix)]
fn own_folders() -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/".into()));
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).unwrap_or_default();
    let mut folders: Vec<PathBuf> = [
        ("XDG_DESKTOP_DIR", "Desktop"),
        ("XDG_DOCUMENTS_DIR", "Documents"),
        ("XDG_DOWNLOAD_DIR", "Downloads"),
    ]
    .into_iter()
    .map(|(key, fallback)| crate::xdg_dir(&text, key, &home).unwrap_or_else(|| home.join(fallback)))
    .collect();
    folders.push(home);
    folders
}

/// The drives Windows has ("C:\", "D:\", ...).
#[cfg(windows)]
fn drives() -> Vec<String> {
    use windows::Win32::Storage::FileSystem::GetLogicalDriveStringsW;
    let mut buffer = vec![0u16; 512];
    let length = unsafe { GetLogicalDriveStringsW(Some(&mut buffer)) } as usize;
    if length == 0 || length > buffer.len() {
        return vec!["C:\\".into()];
    }
    buffer[..length]
        .split(|unit| *unit == 0)
        .filter(|drive| !drive.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[cfg(unix)]
fn drives() -> Vec<String> {
    vec!["/".into()]
}

/// The files to send for `path`: the file itself, or every file of the folder and below as "<folder>/<inside>"
/// (links inside are not followed); at most MAX_FOLDER_FILES, else nothing.
pub fn files_of(path: &Path) -> Result<Vec<(PathBuf, String, u64)>, String> {
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_dir() {
        if !meta.is_file() {
            return Err(format!("{}: not a file", path.display()));
        }
        return Ok(vec![(path.to_path_buf(), name, meta.len())]);
    }
    let mut files = Vec::new();
    let mut pending = vec![(path.to_path_buf(), name)];
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
                if files.len() >= MAX_FOLDER_FILES {
                    return Err(format!(
                        "{}: more than {MAX_FOLDER_FILES} files",
                        path.display()
                    ));
                }
                let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                files.push((entry.path(), inside, size));
            }
        }
    }
    Ok(files)
}

/// A job for the worker thread.
enum Work {
    List(u32, String),
    Files(u32, PathBuf),
}

/// What the worker thread found.
pub enum Found {
    /// The listing asked for with this number.
    Listing(u32, Listing),
    /// The files of what was asked to be sent with this number.
    Files(u32, Result<Vec<(PathBuf, String, u64)>, String>),
}

/// Reads folders on a thread of its own; answers come back in the order asked.
pub struct Lister {
    work: Sender<Work>,
    found: Receiver<Found>,
}

impl Default for Lister {
    fn default() -> Self {
        Lister::new()
    }
}

impl Lister {
    pub fn new() -> Lister {
        let (work, incoming) = mpsc::channel::<Work>();
        let (answer, found) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("queqiao-folders".into())
            .spawn(move || {
                for job in incoming {
                    let reply = match job {
                        Work::List(id, path) => Found::Listing(id, list(&path)),
                        Work::Files(id, path) => Found::Files(id, files_of(&path)),
                    };
                    if answer.send(reply).is_err() {
                        break;
                    }
                }
            });
        Lister { work, found }
    }

    /// Reads folder `path` ("" : the places); the listing comes back as `Found::Listing(id, ...)`.
    pub fn list(&self, id: u32, path: &str) {
        let _ = self.work.send(Work::List(id, path.to_string()));
    }

    /// Finds the files to send for `path`; they come back as `Found::Files(id, ...)`.
    pub fn files(&self, id: u32, path: &Path) {
        let _ = self.work.send(Work::Files(id, path.to_path_buf()));
    }

    /// What was found since the last call.
    pub fn found(&self) -> Vec<Found> {
        self.found.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_join_as_that_side_writes_paths() {
        assert_eq!(child("C:\\Users\\me", "a.txt"), "C:\\Users\\me\\a.txt");
        assert_eq!(child("C:\\", "Users"), "C:\\Users");
        assert_eq!(child("D:", "x"), "D:\\x");
        assert_eq!(child("/home/me", "a.txt"), "/home/me/a.txt");
        assert_eq!(child("/", "home"), "/home");
        assert_eq!(
            child("", "C:\\Users\\me\\Desktop"),
            "C:\\Users\\me\\Desktop"
        );
        assert_eq!(last_part("C:\\Users\\me\\Desktop"), "Desktop");
        assert_eq!(last_part("/home/me/"), "me");
        assert_eq!(last_part("C:\\"), "C:");
        assert_eq!(last_part("/"), "/");
    }

    #[test]
    fn a_folder_lists_folders_first_and_its_parent() {
        let root = std::env::temp_dir().join(format!("queqiao-browse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("b folder")).unwrap();
        std::fs::create_dir_all(root.join("A folder")).unwrap();
        std::fs::write(root.join("c.txt"), b"12345").unwrap();
        std::fs::write(root.join("B.txt"), b"1").unwrap();
        let listing = list(&root.to_string_lossy());
        assert!(listing.error.is_empty(), "{}", listing.error);
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["A folder", "b folder", "B.txt", "c.txt"]);
        assert_eq!(listing.entries[3].size, 5);
        assert!(listing.entries[3].modified > 1_700_000_000);
        assert_eq!(
            Path::new(&listing.parent),
            root.parent().unwrap(),
            "{}",
            listing.parent
        );
        // Not there, not a full path.
        assert!(!list(&root.join("none").to_string_lossy()).error.is_empty());
        assert!(!list("relative/path").error.is_empty());
        // The files of a folder, named inside it.
        std::fs::write(root.join("A folder").join("in.bin"), vec![1u8; 10]).unwrap();
        let mut files = files_of(&root).unwrap();
        files.sort_by(|a, b| a.1.cmp(&b.1));
        let top = root.file_name().unwrap().to_string_lossy().into_owned();
        let named: Vec<(String, u64)> = files.iter().map(|f| (f.1.clone(), f.2)).collect();
        assert_eq!(
            named,
            [
                (format!("{top}/A folder/in.bin"), 10),
                (format!("{top}/B.txt"), 1),
                (format!("{top}/c.txt"), 5)
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_places_are_folders_and_drives_that_exist() {
        let places = places();
        assert!(
            places.iter().any(|e| e.kind == EntryKind::Drive),
            "{places:?}"
        );
        for entry in &places {
            assert!(Path::new(&entry.name).is_dir(), "{entry:?}");
        }
        // Each place lists without an error, and its parent leads up.
        let listing = list(&places[0].name);
        assert!(listing.error.is_empty(), "{listing:?}");
    }

    #[test]
    fn the_worker_answers() {
        let lister = Lister::new();
        lister.list(7, "");
        let started = std::time::Instant::now();
        loop {
            if let Some(Found::Listing(7, listing)) = lister.found().into_iter().next() {
                assert!(!listing.entries.is_empty());
                break;
            }
            assert!(started.elapsed().as_secs() < 10, "no answer");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
