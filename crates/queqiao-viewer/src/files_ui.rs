//! The session's file transfer window (UU Remote's two panes; the window itself: queqiao-display's FileWindow): this
//! PC's folders read here on a worker thread, the other PC's asked over the files channel (queqiao-files' List), each
//! pane with its history; Send turns checked entries into jobs (sent into the other pane's folder, or fetched into
//! this pane's), and the transfer list shows the session's jobs.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use queqiao_core::text::tr;
use queqiao_display::{
    FileItem, FileKind, FilePane, FileWindow, FileWindowEvent, JobCommand, JobRow, JobRowState,
};
use queqiao_files::{EntryKind, Found, Job, JobState, Lister, Listing, Transfers, browse};

/// A remote folder not answered within this long is given up (that PC's Queqiao may not know browsing).
const ANSWER_LIMIT: Duration = Duration::from_secs(10);
/// The transfer list is refreshed this often while the window is open.
const JOBS_EVERY: Duration = Duration::from_millis(250);

#[derive(Default)]
struct Pane {
    folder: String,
    parent: String,
    items: Vec<FileItem>,
    status: String,
    places: Vec<(String, String)>,
    history: Vec<String>,
    /// The listing asked for, the folder it is for, whether it goes into the history, and when it was asked.
    asked: Option<(u32, String, bool, Instant)>,
}

impl Pane {
    fn shown(&self) -> FilePane {
        FilePane {
            folder: self.folder.clone(),
            items: self.items.clone(),
            status: self.status.clone(),
            places: self.places.clone(),
            can_go_back: !self.history.is_empty(),
            can_go_up: !self.folder.is_empty(),
        }
    }

    /// A listing arrived for this pane's question.
    fn take(&mut self, listing: Listing) {
        let Some((_, asked, remember, _)) = self.asked.take() else {
            return;
        };
        if !listing.error.is_empty() && listing.entries.is_empty() && !asked.is_empty() {
            // The folder could not be read: the pane stays where it was and says why.
            self.status = format!("{asked}: {}", listing.error);
            return;
        }
        let path = if asked.is_empty() {
            String::new()
        } else {
            listing.path.clone()
        };
        if remember && path != self.folder {
            self.history.push(self.folder.clone());
            if self.history.len() > 100 {
                self.history.remove(0);
            }
        }
        if path.is_empty() {
            self.places = places_of(&listing);
        }
        self.folder = path;
        self.parent = listing.parent.clone();
        self.items = listing.entries.iter().map(item_of).collect();
        self.status = listing.error;
    }
}

fn item_of(entry: &queqiao_files::Entry) -> FileItem {
    let kind = match entry.kind {
        EntryKind::File => FileKind::File,
        EntryKind::Folder => FileKind::Folder,
        EntryKind::Drive => FileKind::Drive,
        EntryKind::Place => FileKind::Place,
    };
    let label = match entry.kind {
        EntryKind::Drive => entry.name.clone(),
        EntryKind::Place => browse::last_part(&entry.name).to_string(),
        _ => entry.name.clone(),
    };
    FileItem {
        name: entry.name.clone(),
        label,
        kind,
        size: entry.size,
        modified: entry.modified,
    }
}

/// The places of a places listing, for the folder box: (label, path).
fn places_of(listing: &Listing) -> Vec<(String, String)> {
    listing
        .entries
        .iter()
        .map(|entry| (item_of(entry).label, entry.name.clone()))
        .collect()
}

/// This PC's name as the account lists it.
fn this_pc_name() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").unwrap_or_else(|_| "This PC".into())
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|name| name.trim().to_string())
            .ok()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "This PC".into())
    }
}

/// Tests (--browse-test): the window opened, this pane in a test folder and the other in the given one, a file fetched
/// from there and one sent there, both confirmed by the receiving side.
pub(crate) struct BrowseTest {
    /// That PC's folder holding `down.txt`.
    pub(crate) remote_folder: String,
    step: u8,
    local_folder: PathBuf,
    jobs: Vec<u32>,
    started: Instant,
    remote_entries: usize,
}

impl BrowseTest {
    pub(crate) fn new(remote_folder: &str) -> BrowseTest {
        BrowseTest {
            remote_folder: remote_folder.to_string(),
            step: 0,
            local_folder: std::env::temp_dir().join("queqiao_browse_test"),
            jobs: Vec::new(),
            started: Instant::now(),
            remote_entries: 0,
        }
    }
}

pub(crate) struct FilesUi {
    window: Option<FileWindow>,
    that_pc: String,
    /// This PC's pane (0) and the other's (1).
    panes: [Pane; 2],
    local: Lister,
    next_local: u32,
    jobs_shown: Instant,
    pub(crate) test: Option<BrowseTest>,
}

impl FilesUi {
    pub(crate) fn new(that_pc: &str) -> FilesUi {
        FilesUi {
            window: None,
            that_pc: that_pc.to_string(),
            panes: [Pane::default(), Pane::default()],
            local: Lister::new(),
            next_local: 0,
            jobs_shown: Instant::now(),
            test: None,
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.window.is_some()
    }

    /// Opens the window (or brings it to the front): this PC's pane at the person's first own folder (Desktop), the
    /// other's at that PC's places, then its first one.
    pub(crate) fn open(&mut self, transfers: &mut Transfers, lines: &mut Vec<String>) {
        if let Some(window) = &self.window {
            window.show();
            return;
        }
        match FileWindow::new(&this_pc_name(), &self.that_pc) {
            Ok(window) => self.window = Some(window),
            Err(error) => {
                lines.push(format!("file transfer window: {error}"));
                return;
            }
        }
        lines.push("file transfer window opened".into());
        let places = browse::places();
        self.panes[0].places = places
            .iter()
            .map(|entry| (item_of(entry).label, entry.name.clone()))
            .collect();
        let start = places
            .first()
            .map(|entry| entry.name.clone())
            .unwrap_or_default();
        self.go(0, &start, false, transfers);
        self.go(1, "", false, transfers);
    }

    /// Shows `path` in pane `side` (0 here, 1 there); `remember`: the folder left goes into its history.
    fn go(&mut self, side: usize, path: &str, remember: bool, transfers: &mut Transfers) {
        let id = if side == 0 {
            self.next_local += 1;
            self.local.list(self.next_local, path);
            self.next_local
        } else {
            transfers.list_remote(path)
        };
        let pane = &mut self.panes[side];
        pane.asked = Some((id, path.to_string(), remember, Instant::now()));
        pane.status = tr("Reading...").to_string();
        self.show_pane(side);
    }

    fn show_pane(&mut self, side: usize) {
        if let Some(window) = &mut self.window {
            window.set_pane(side == 1, &self.panes[side].shown());
        }
    }

    /// Called from the session's loop: listings that arrived, what the person did, the transfer list.
    pub(crate) fn poll(&mut self, transfers: &mut Transfers, lines: &mut Vec<String>) {
        // Listings of this PC (worker thread) and of the other (the files channel).
        for found in self.local.found() {
            if let Found::Listing(id, listing) = found
                && self.panes[0]
                    .asked
                    .as_ref()
                    .is_some_and(|asked| asked.0 == id)
            {
                self.panes[0].take(listing);
                self.show_pane(0);
            }
        }
        for (id, listing) in transfers.listings() {
            if self.panes[1]
                .asked
                .as_ref()
                .is_some_and(|asked| asked.0 == id)
            {
                let first_open = self.panes[1].folder.is_empty()
                    && self.panes[1].history.is_empty()
                    && self.panes[1]
                        .asked
                        .as_ref()
                        .is_some_and(|asked| asked.1.is_empty());
                let error = listing.error.clone();
                self.panes[1].take(listing);
                if !error.is_empty() {
                    lines.push(format!("that PC's folder: {error}"));
                }
                // The first time: on into that PC's first own folder (its Desktop), as UU Remote starts.
                if first_open
                    && self.window.is_some()
                    && let Some((_, first)) = self.panes[1].places.first().cloned()
                    && self.test.is_none()
                {
                    self.go(1, &first, false, transfers);
                }
                self.show_pane(1);
            }
        }
        // A question not answered in time.
        if let Some((_, _, _, at)) = self.panes[1].asked
            && at.elapsed() > ANSWER_LIMIT
        {
            self.panes[1].asked = None;
            self.panes[1].status =
                tr("That PC did not answer: its Queqiao may be older than this one.").to_string();
            self.show_pane(1);
        }
        while let Some(event) = self.window.as_mut().and_then(FileWindow::next_event) {
            self.handle(event, transfers, lines);
        }
        if self.window.is_some() && self.jobs_shown.elapsed() >= JOBS_EVERY {
            self.jobs_shown = Instant::now();
            let rows: Vec<JobRow> = transfers.jobs().iter().map(row_of).collect();
            if let Some(window) = &mut self.window {
                window.set_jobs(&rows);
            }
        }
        self.test_step(transfers, lines);
    }

    fn handle(
        &mut self,
        event: FileWindowEvent,
        transfers: &mut Transfers,
        lines: &mut Vec<String>,
    ) {
        match event {
            FileWindowEvent::Open { remote, path } => {
                self.go(usize::from(remote), &path, true, transfers)
            }
            FileWindowEvent::OpenItem { remote, name } => {
                let side = usize::from(remote);
                let path = browse::child(&self.panes[side].folder, &name);
                self.go(side, &path, true, transfers);
            }
            FileWindowEvent::Up { remote } => {
                let side = usize::from(remote);
                let parent = self.panes[side].parent.clone();
                if !self.panes[side].folder.is_empty() {
                    self.go(side, &parent, true, transfers);
                }
            }
            FileWindowEvent::Back { remote } => {
                let side = usize::from(remote);
                if let Some(previous) = self.panes[side].history.pop() {
                    self.go(side, &previous, false, transfers);
                }
            }
            FileWindowEvent::Refresh { remote } => {
                let side = usize::from(remote);
                let folder = self.panes[side].folder.clone();
                self.go(side, &folder, false, transfers);
            }
            FileWindowEvent::Send { from_remote, names } => {
                let (here, there) = (self.panes[0].folder.clone(), self.panes[1].folder.clone());
                for name in names {
                    if from_remote {
                        let path = browse::child(&there, &name);
                        if here.is_empty() {
                            continue;
                        }
                        transfers.fetch(&path, Path::new(&here));
                        lines.push(format!("fetching {path} into {here}"));
                    } else {
                        let path = PathBuf::from(browse::child(&here, &name));
                        if there.is_empty() {
                            continue;
                        }
                        transfers.send_into(&path, &there);
                        lines.push(format!("sending {} into {there}", path.display()));
                    }
                }
            }
            FileWindowEvent::Dropped(paths) => {
                let there = self.panes[1].folder.clone();
                for path in paths {
                    if there.is_empty() {
                        let _ = transfers.send(&path);
                    } else {
                        transfers.send_into(&path, &there);
                    }
                    lines.push(format!("sending {} (dropped)", path.display()));
                }
            }
            FileWindowEvent::PauseAll => transfers.pause_all(true),
            FileWindowEvent::ResumeAll => transfers.pause_all(false),
            FileWindowEvent::CancelAll => transfers.cancel_all(),
            FileWindowEvent::ClearFinished => transfers.clear_finished(None),
            FileWindowEvent::Job { id, command } => {
                let job = transfers.jobs().into_iter().find(|job| job.id == id);
                match command {
                    JobCommand::ShowInFolder => {
                        if let Some(path) = job.as_ref().and_then(local_path) {
                            show_in_folder(&path);
                        }
                    }
                    JobCommand::Pause => transfers.pause_job(id, true),
                    JobCommand::Resume => transfers.pause_job(id, false),
                    JobCommand::Cancel => transfers.cancel_job(id),
                    JobCommand::Remove => {
                        if job.is_some_and(|job| !job.finished()) {
                            transfers.cancel_job(id);
                        }
                        transfers.clear_finished(Some(id));
                    }
                }
            }
            FileWindowEvent::Closed => {
                self.window = None;
                lines.push("file transfer window closed".into());
            }
        }
        // A changed list shows soon.
        self.jobs_shown = self
            .jobs_shown
            .checked_sub(JOBS_EVERY)
            .unwrap_or(self.jobs_shown);
    }

    /// Tests (--browse-test): see BrowseTest.
    fn test_step(&mut self, transfers: &mut Transfers, lines: &mut Vec<String>) {
        if self.window.is_none() {
            return;
        }
        let Some(step) = self.test.as_ref().map(|test| test.step) else {
            return;
        };
        match step {
            // This pane into the test folder (with up.txt), the other into the folder given.
            0 => {
                let Some(test) = self.test.as_mut() else {
                    return;
                };
                let _ = std::fs::create_dir_all(&test.local_folder);
                let _ = std::fs::remove_file(test.local_folder.join("down.txt"));
                let _ =
                    std::fs::write(test.local_folder.join("up.txt"), b"queqiao browse test: up");
                let (local, remote) = (
                    test.local_folder.to_string_lossy().into_owned(),
                    test.remote_folder.clone(),
                );
                test.step = 1;
                self.go(0, &local, true, transfers);
                self.go(1, &remote, true, transfers);
            }
            // Both shown: down.txt fetched, up.txt sent.
            1 => {
                if self.panes.iter().any(|pane| pane.asked.is_some()) {
                    return;
                }
                let Some(window) = &self.window else {
                    return;
                };
                let checked_there = window.check_for_test(true, "down.txt");
                let checked_here = window.check_for_test(false, "up.txt");
                let entries = self.panes[1].items.len();
                lines.push(format!(
                    "browse test: that PC's folder {} shows {entries} entries; down.txt there: {checked_there}, up.txt here: {checked_here}",
                    self.panes[1].folder
                ));
                window.send_for_test(true);
                window.send_for_test(false);
                if let Some(test) = self.test.as_mut() {
                    test.remote_entries = entries;
                    test.step = 2;
                }
            }
            // The jobs: two, both done.
            _ => {
                let jobs = transfers.jobs();
                let finished = jobs.len() >= 2 && jobs.iter().all(Job::finished);
                let Some(test) = self.test.as_mut() else {
                    return;
                };
                if !finished && test.started.elapsed() < Duration::from_secs(40) {
                    return;
                }
                let fetched = std::fs::read(test.local_folder.join("down.txt")).unwrap_or_default();
                let sent_done = jobs
                    .iter()
                    .any(|job| job.sending && job.name == "up.txt" && job.state == JobState::Done);
                let fetched_done = jobs.iter().any(|job| {
                    !job.sending && job.name == "down.txt" && job.state == JobState::Done
                });
                let passed =
                    sent_done && fetched_done && fetched.starts_with(b"queqiao browse test: down");
                lines.push(format!(
                    "browse test: {} (fetched down.txt: {fetched_done}, {} bytes here; sent up.txt: {sent_done}; jobs: {:?})",
                    if passed { "PASS" } else { "FAIL" },
                    fetched.len(),
                    jobs.iter()
                        .map(|job| format!("{} {:?}", job.name, job.state))
                        .collect::<Vec<_>>()
                ));
                test.jobs = jobs.iter().map(|job| job.id).collect();
                self.test = None;
            }
        }
    }

    /// Tests: a picture of the window.
    pub(crate) fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        match &self.window {
            Some(window) => window.save_picture(path),
            None => Err(std::io::Error::other(
                "the file transfer window is not open",
            )),
        }
    }

    /// The test is over (or there is none).
    pub(crate) fn test_done(&self) -> bool {
        self.test.is_none()
    }
}

/// Where a job is on this PC: what arrived, or what was sent from here.
fn local_path(job: &Job) -> Option<PathBuf> {
    if job.sending {
        Some(PathBuf::from(&job.from)).filter(|path| path.exists())
    } else {
        job.arrived.clone()
    }
}

fn row_of(job: &Job) -> JobRow {
    JobRow {
        id: job.id,
        name: job.name.clone(),
        folder: job.files > 1,
        sending: job.sending,
        from: job.from.clone(),
        to: job.to.clone(),
        bytes: job.bytes,
        bytes_done: job.bytes_done,
        state: match &job.state {
            JobState::Waiting => JobRowState::Waiting,
            JobState::Going => JobRowState::Going,
            JobState::Paused => JobRowState::Paused,
            JobState::Done => JobRowState::Done,
            JobState::Failed(reason) => JobRowState::Failed(reason.clone()),
            JobState::Cancelled => JobRowState::Cancelled,
        },
        can_pause: job.can_pause,
        local_path: local_path(job),
    }
}

fn show_in_folder(path: &Path) {
    queqiao_display::show_in_folder(path);
}
