//! The file transfer window on Linux (Windows: file_window.rs, with the same API), drawn by the GUI thread (egui):
//! this PC's files on the left and the other PC's on the right, each with back, up, refresh, the folder (typed, or
//! one of the places) and its entries with check boxes; "Send →" and "← Send" between them; the transfer list below
//! with pause all, start all, cancel all and clear finished, and each row's buttons.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use eframe::egui;
use queqiao_core::text::{tr, trf};

use super::Result;
use super::gui::{self, Opened, View};
use super::window::Hwnd;
use crate::model::{
    FileItem, FileKind, FilePane, FileWindowEvent, JobCommand, JobRow, JobRowState, size_text,
};

const NIGHT: egui::Color32 = egui::Color32::from_rgb(0x14, 0x1A, 0x3B);
const PAGE: egui::Color32 = egui::Color32::from_rgb(0xF4, 0xF5, 0xF8);
const CARD_LINE: egui::Color32 = egui::Color32::from_rgb(0xE3, 0xE6, 0xEE);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0x1B, 0x21, 0x40);
const SECONDARY: egui::Color32 = egui::Color32::from_rgb(0x64, 0x6B, 0x8C);
const GOLD: egui::Color32 = egui::Color32::from_rgb(0xE3, 0xB4, 0x5A);
const ONLINE: egui::Color32 = egui::Color32::from_rgb(0x2A, 0x9A, 0x47);
const FAILED: egui::Color32 = egui::Color32::from_rgb(0xC8, 0x4A, 0x3C);

#[derive(Default)]
struct FilesState {
    names: [String; 2],
    panes: [FilePane; 2],
    checked: [HashSet<String>; 2],
    selected: [Option<String>; 2],
    /// The folder boxes' text (the folder shown, or what is being typed).
    typed: [String; 2],
    jobs: Vec<JobRow>,
    events: Vec<FileWindowEvent>,
    focus: bool,
    /// A name being typed in a pane: for the entry `name`, or for a new folder (None).
    editing: [Option<NameEdit>; 2],
    /// The pane clicked last (the keys F2, Delete and Ctrl+Shift+N act there).
    active: usize,
    /// A delete question open now: the pane, the names, the question.
    deleting: Option<(usize, Vec<String>, String)>,
}

struct NameEdit {
    name: Option<String>,
    text: String,
    /// The field takes the keyboard on its first frame.
    focus: bool,
}

/// Whether the entry can be renamed or deleted: a file or folder inside a folder (not a drive or a place).
fn changeable(item: &FileItem, folder: &str) -> bool {
    !folder.is_empty() && matches!(item.kind, FileKind::File | FileKind::Folder)
}

/// "New folder", or "New folder (2)" ... when the folder has one already.
fn new_folder_name(pane: &FilePane) -> String {
    let base = tr("New folder").to_string();
    (1..1000)
        .map(|n| {
            if n == 1 {
                base.clone()
            } else {
                format!("{base} ({n})")
            }
        })
        .find(|name| !pane.items.iter().any(|item| &item.name == name))
        .unwrap_or(base)
}

/// The delete question for `names` of pane `side`, when they can be deleted.
fn delete_question(state: &FilesState, side: usize, names: &[String]) -> Option<String> {
    let pane = &state.panes[side];
    let items: Vec<&FileItem> = names
        .iter()
        .filter_map(|name| pane.items.iter().find(|item| &item.name == name))
        .collect();
    if items.is_empty()
        || items.len() != names.len()
        || !items.iter().all(|item| changeable(item, &pane.folder))
    {
        return None;
    }
    let pc = state.names[side].as_str();
    let question = match items.as_slice() {
        [item] if item.kind == FileKind::Folder => trf(
            "Delete the folder \"{name}\" and everything in it on {pc}?",
            &[("name", &item.label), ("pc", pc)],
        ),
        [item] => trf(
            "Delete \"{name}\" on {pc}?",
            &[("name", &item.label), ("pc", pc)],
        ),
        _ => trf(
            "Delete these {count} items on {pc}?",
            &[("count", &items.len().to_string()), ("pc", pc)],
        ),
    };
    Some(format!(
        "{question}\n\n{}",
        tr("This cannot be undone: nothing goes to the Recycle Bin or the Trash.")
    ))
}

fn ask_delete(state: &mut FilesState, side: usize, names: Vec<String>) {
    if state.deleting.is_none()
        && let Some(question) = delete_question(state, side, &names)
    {
        state.deleting = Some((side, names, question));
    }
}

fn start_rename(state: &mut FilesState, side: usize, name: &str) {
    let pane = &state.panes[side];
    if pane
        .items
        .iter()
        .any(|item| item.name == name && changeable(item, &pane.folder))
    {
        state.editing[side] = Some(NameEdit {
            name: Some(name.to_string()),
            text: name.to_string(),
            focus: true,
        });
    }
}

fn start_new_folder(state: &mut FilesState, side: usize) {
    if !state.panes[side].folder.is_empty() {
        state.editing[side] = Some(NameEdit {
            name: None,
            text: new_folder_name(&state.panes[side]),
            focus: true,
        });
    }
}

/// The field a name is typed in; on Enter the change goes to the session, on Escape (or a click elsewhere) it is
/// dropped.
fn name_field(ui: &mut egui::Ui, state: &mut FilesState, side: usize) {
    let Some(edit) = state.editing[side].as_mut() else {
        return;
    };
    let response = ui.add(egui::TextEdit::singleline(&mut edit.text).desired_width(220.0));
    if std::mem::take(&mut edit.focus) {
        response.request_focus();
    }
    if response.lost_focus() {
        let typed = edit.text.trim().to_string();
        let entered = ui.input(|input| input.key_pressed(egui::Key::Enter));
        let edit = state.editing[side].take();
        let remote = side == 1;
        match edit.and_then(|edit| edit.name) {
            _ if !entered || typed.is_empty() => {}
            None => state.events.push(FileWindowEvent::MakeFolder {
                remote,
                name: typed,
            }),
            Some(name) if name != typed => state.events.push(FileWindowEvent::Rename {
                remote,
                name,
                new_name: typed,
            }),
            Some(_) => {}
        }
    }
}

struct FilesView {
    state: Arc<Mutex<FilesState>>,
    title: String,
}

/// Seconds since 1970 as this PC's local date and time ("2026/06/30 17:55").
fn time_text(seconds: i64) -> String {
    if seconds <= 0 {
        return String::new();
    }
    let time = seconds as libc::time_t;
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&time, &mut parts) }.is_null() {
        return String::new();
    }
    format!(
        "{}/{:02}/{:02} {:02}:{:02}",
        parts.tm_year + 1900,
        parts.tm_mon + 1,
        parts.tm_mday,
        parts.tm_hour,
        parts.tm_min
    )
}

fn type_text(item: &FileItem) -> String {
    match item.kind {
        FileKind::Folder | FileKind::Place => tr("Folder").to_string(),
        FileKind::Drive => tr("Drive").to_string(),
        FileKind::File => Path::new(&item.name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default(),
    }
}

fn status_text(job: &JobRow) -> (String, egui::Color32) {
    let percent = job
        .bytes_done
        .saturating_mul(100)
        .checked_div(job.bytes)
        .unwrap_or(0);
    match &job.state {
        JobRowState::Waiting => (tr("Waiting").to_string(), SECONDARY),
        JobRowState::Going => (format!("{percent} %"), TEXT),
        JobRowState::Paused => (format!("{} {percent} %", tr("Paused")), SECONDARY),
        JobRowState::Done if job.sending => (format!("✔ {}", tr("Sent")), ONLINE),
        JobRowState::Done => (format!("✔ {}", tr("Received")), ONLINE),
        JobRowState::Failed(reason) => (format!("{}: {reason}", tr("Failed")), FAILED),
        JobRowState::Cancelled => (tr("Cancelled").to_string(), SECONDARY),
    }
}

impl FilesView {
    fn pane(ui: &mut egui::Ui, state: &mut FilesState, side: usize) {
        let remote = side == 1;
        let pane = state.panes[side].clone();
        let other_folder_empty = state.panes[1 - side].folder.is_empty();
        let checked_any = !state.checked[side].is_empty() || state.selected[side].is_some();
        // The PC's name, its chip, and the Send button towards the other pane.
        ui.horizontal(|ui| {
            let (chip, colour) = if remote {
                (tr("That PC"), egui::Color32::from_rgb(0x2F, 0x78, 0x4A))
            } else {
                (tr("This PC"), egui::Color32::from_rgb(0x3D, 0x5F, 0xAE))
            };
            let send = |ui: &mut egui::Ui, state: &mut FilesState| {
                let label = if remote {
                    tr("← Send")
                } else {
                    tr("Send →")
                };
                let button = egui::Button::new(
                    egui::RichText::new(label)
                        .color(egui::Color32::WHITE)
                        .strong(),
                )
                .fill(NIGHT)
                .corner_radius(8);
                if ui
                    .add_enabled(checked_any && !other_folder_empty, button)
                    .clicked()
                {
                    let names = checked_names(state, side);
                    state.events.push(FileWindowEvent::Send {
                        from_remote: remote,
                        names,
                    });
                }
            };
            if remote {
                send(ui, state);
            }
            ui.label(
                egui::RichText::new(&state.names[side])
                    .size(17.0)
                    .strong()
                    .color(TEXT),
            );
            ui.label(egui::RichText::new(chip).small().color(colour));
            if !remote {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    send(ui, state);
                });
            }
        });
        egui::Frame::new()
            .fill(egui::Color32::WHITE)
            .stroke(egui::Stroke::new(1.0_f32, CARD_LINE))
            .corner_radius(12)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(pane.can_go_back, egui::Button::new("←"))
                        .on_hover_text(tr("Back"))
                        .clicked()
                    {
                        state.events.push(FileWindowEvent::Back { remote });
                    }
                    if ui
                        .add_enabled(pane.can_go_up, egui::Button::new("↑"))
                        .on_hover_text(tr("Up"))
                        .clicked()
                    {
                        state.events.push(FileWindowEvent::Up { remote });
                    }
                    if ui.button("⟳").on_hover_text(tr("Refresh")).clicked() {
                        state.events.push(FileWindowEvent::Refresh { remote });
                    }
                    if ui
                        .add_enabled(!pane.folder.is_empty(), egui::Button::new("＋📁"))
                        .on_hover_text(tr("New folder"))
                        .clicked()
                    {
                        state.active = side;
                        start_new_folder(state, side);
                    }
                    let mut chosen: Option<String> = None;
                    egui::ComboBox::from_id_salt(("places", side))
                        .selected_text("☰")
                        .width(30.0)
                        .show_ui(ui, |ui| {
                            for (label, path) in &pane.places {
                                if ui
                                    .selectable_label(false, format!("{label}  —  {path}"))
                                    .clicked()
                                {
                                    chosen = Some(path.clone());
                                }
                            }
                        });
                    if let Some(path) = chosen {
                        state.events.push(FileWindowEvent::Open { remote, path });
                    }
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut state.typed[side])
                            .desired_width(f32::INFINITY),
                    );
                    if response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        let path = state.typed[side].trim().to_string();
                        if !path.is_empty() {
                            state.events.push(FileWindowEvent::Open { remote, path });
                        }
                    }
                });
                ui.separator();
                let height = (ui.available_height() - 22.0).max(80.0);
                egui::ScrollArea::vertical()
                    .id_salt(("entries", side))
                    .max_height(height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Grid::new(("grid", side))
                            .num_columns(5)
                            .striped(true)
                            .spacing([10.0, 4.0])
                            .show(ui, |ui| {
                                ui.label("");
                                for title in [tr("Name"), tr("Modified"), tr("Type"), tr("Size")] {
                                    ui.label(egui::RichText::new(title).small().color(SECONDARY));
                                }
                                ui.end_row();
                                for item in &pane.items {
                                    let mut on = state.checked[side].contains(&item.name);
                                    if ui.checkbox(&mut on, "").changed() {
                                        if on {
                                            state.checked[side].insert(item.name.clone());
                                        } else {
                                            state.checked[side].remove(&item.name);
                                        }
                                    }
                                    let icon = if item.kind == FileKind::File {
                                        "📄"
                                    } else {
                                        "📁"
                                    };
                                    let selected =
                                        state.selected[side].as_deref() == Some(item.name.as_str());
                                    let renaming = state.editing[side]
                                        .as_ref()
                                        .is_some_and(|edit| edit.name.as_ref() == Some(&item.name));
                                    if renaming {
                                        ui.horizontal(|ui| {
                                            ui.label(icon);
                                            name_field(ui, state, side);
                                        });
                                    } else {
                                        let response = ui.selectable_label(
                                            selected,
                                            format!("{icon} {}", item.label),
                                        );
                                        if response.clicked() || response.secondary_clicked() {
                                            state.selected[side] = Some(item.name.clone());
                                            state.active = side;
                                        }
                                        if response.double_clicked() && item.kind != FileKind::File
                                        {
                                            state.events.push(FileWindowEvent::OpenItem {
                                                remote,
                                                name: item.name.clone(),
                                            });
                                        }
                                        response.context_menu(|ui| {
                                            Self::item_menu(ui, state, side, item);
                                        });
                                    }
                                    ui.label(
                                        egui::RichText::new(time_text(item.modified))
                                            .color(SECONDARY),
                                    );
                                    ui.label(egui::RichText::new(type_text(item)).color(SECONDARY));
                                    ui.label(if item.kind == FileKind::File {
                                        size_text(item.size)
                                    } else {
                                        "--".to_string()
                                    });
                                    ui.end_row();
                                }
                                if state.editing[side]
                                    .as_ref()
                                    .is_some_and(|edit| edit.name.is_none())
                                {
                                    ui.label("");
                                    ui.horizontal(|ui| {
                                        ui.label("📁");
                                        name_field(ui, state, side);
                                    });
                                    ui.end_row();
                                }
                            });
                    });
                ui.label(egui::RichText::new(&pane.status).small().color(SECONDARY));
            });
    }

    /// The menu of an entry (right click).
    fn item_menu(ui: &mut egui::Ui, state: &mut FilesState, side: usize, item: &FileItem) {
        let remote = side == 1;
        let pane = state.panes[side].clone();
        // All checked entries when this one is checked, else this one.
        let names = if state.checked[side].contains(&item.name) {
            checked_names(state, side)
        } else {
            vec![item.name.clone()]
        };
        let can_change = names.iter().all(|name| {
            pane.items
                .iter()
                .any(|entry| &entry.name == name && changeable(entry, &pane.folder))
        });
        let other = state.names[1 - side].clone();
        let other_has_folder = !state.panes[1 - side].folder.is_empty();
        if ui
            .add_enabled(
                names.len() == 1 && item.kind != FileKind::File,
                egui::Button::new(tr("Open")),
            )
            .clicked()
        {
            state.events.push(FileWindowEvent::OpenItem {
                remote,
                name: item.name.clone(),
            });
            ui.close();
        }
        if ui
            .add_enabled(
                other_has_folder,
                egui::Button::new(trf("Send to {pc}", &[("pc", &other)])),
            )
            .clicked()
        {
            state.events.push(FileWindowEvent::Send {
                from_remote: remote,
                names: names.clone(),
            });
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(!pane.folder.is_empty(), egui::Button::new(tr("New folder")))
            .clicked()
        {
            start_new_folder(state, side);
            ui.close();
        }
        if ui
            .add_enabled(
                can_change && names.len() == 1,
                egui::Button::new(tr("Rename")),
            )
            .clicked()
        {
            start_rename(state, side, &item.name);
            ui.close();
        }
        if ui
            .add_enabled(can_change, egui::Button::new(tr("Delete")))
            .clicked()
        {
            ask_delete(state, side, names.clone());
            ui.close();
        }
        if ui
            .add_enabled(!pane.folder.is_empty(), egui::Button::new(tr("Size")))
            .clicked()
        {
            state
                .events
                .push(FileWindowEvent::Measure { remote, names });
            ui.close();
        }
        ui.separator();
        if ui.button(tr("Refresh")).clicked() {
            state.events.push(FileWindowEvent::Refresh { remote });
            ui.close();
        }
    }

    /// F2, Delete and Ctrl+Shift+N in the pane clicked last (not while a field takes the keys).
    fn keys(ctx: &egui::Context, state: &mut FilesState) {
        if ctx.wants_keyboard_input() || state.deleting.is_some() {
            return;
        }
        let side = state.active;
        let (rename, delete, new_folder) = ctx.input(|input| {
            (
                input.key_pressed(egui::Key::F2),
                input.key_pressed(egui::Key::Delete),
                input.key_pressed(egui::Key::N) && input.modifiers.ctrl && input.modifiers.shift,
            )
        });
        if rename && let Some(name) = state.selected[side].clone() {
            start_rename(state, side, &name);
        }
        if delete {
            let names = checked_names(state, side);
            ask_delete(state, side, names);
        }
        if new_folder {
            start_new_folder(state, side);
        }
    }

    /// The delete question, over the window until answered.
    fn delete_modal(ctx: &egui::Context, state: &mut FilesState) {
        let Some((side, names, question)) = state.deleting.clone() else {
            return;
        };
        let mut answer = None;
        let modal = egui::Modal::new(egui::Id::new("queqiao-delete")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(egui::RichText::new(tr("Delete")).strong().size(16.0));
            ui.add_space(6.0);
            ui.label(&question);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(tr("Cancel")).clicked() {
                    answer = Some(false);
                }
                let delete = egui::Button::new(
                    egui::RichText::new(tr("Delete")).color(egui::Color32::WHITE),
                )
                .fill(FAILED);
                if ui.add(delete).clicked() {
                    answer = Some(true);
                }
            });
        });
        if modal.should_close() && answer.is_none() {
            answer = Some(false);
        }
        if let Some(yes) = answer {
            state.deleting = None;
            if yes {
                state.events.push(FileWindowEvent::Delete {
                    remote: side == 1,
                    names,
                });
            }
        }
    }

    fn jobs(ui: &mut egui::Ui, state: &mut FilesState) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr("Transfers")).strong().color(TEXT));
            let done = state
                .jobs
                .iter()
                .filter(|job| job.state == JobRowState::Done)
                .count();
            let note = if state.jobs.is_empty() {
                tr("Nothing sent yet: check files and press Send.").to_string()
            } else {
                trf("{count} done", &[("count", &done.to_string())])
            };
            ui.label(egui::RichText::new(note).small().color(SECONDARY));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let running = state
                    .jobs
                    .iter()
                    .any(|job| matches!(job.state, JobRowState::Going | JobRowState::Waiting));
                let paused = state
                    .jobs
                    .iter()
                    .any(|job| job.state == JobRowState::Paused);
                for (label, event, enabled) in [
                    (
                        tr("Clear finished"),
                        FileWindowEvent::ClearFinished,
                        !state.jobs.is_empty(),
                    ),
                    (
                        tr("Cancel all"),
                        FileWindowEvent::CancelAll,
                        running || paused,
                    ),
                    (tr("Start all"), FileWindowEvent::ResumeAll, paused),
                    (tr("Pause all"), FileWindowEvent::PauseAll, running),
                ] {
                    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                        state.events.push(event);
                    }
                }
            });
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("jobs")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("jobs_grid")
                    .num_columns(6)
                    .striped(true)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for title in [
                            tr("Name"),
                            tr("Status"),
                            tr("Size"),
                            tr("From"),
                            tr("To"),
                            "",
                        ] {
                            ui.label(egui::RichText::new(title).small().color(SECONDARY));
                        }
                        ui.end_row();
                        let jobs = state.jobs.clone();
                        for job in &jobs {
                            let icon = if job.folder { "📁" } else { "📄" };
                            ui.label(format!("{icon} {}", job.name));
                            let (status, colour) = status_text(job);
                            if matches!(job.state, JobRowState::Going | JobRowState::Paused)
                                && job.bytes > 0
                            {
                                let share = job.bytes_done as f32 / job.bytes as f32;
                                ui.add(
                                    egui::ProgressBar::new(share.clamp(0.0, 1.0))
                                        .desired_width(110.0)
                                        .fill(GOLD)
                                        .text(status),
                                );
                            } else {
                                ui.label(egui::RichText::new(status).color(colour));
                            }
                            ui.label(if job.bytes > 0 {
                                size_text(job.bytes)
                            } else {
                                "--".into()
                            });
                            for (here, path) in [(job.sending, &job.from), (!job.sending, &job.to)]
                            {
                                let side = if here { tr("This PC") } else { tr("That PC") };
                                let shown = if path.is_empty() {
                                    tr("Downloads/Queqiao").to_string()
                                } else {
                                    path.clone()
                                };
                                ui.label(format!("[{side}] {shown}"));
                            }
                            ui.horizontal(|ui| {
                                if job.local_path.is_some()
                                    && ui
                                        .small_button("📂")
                                        .on_hover_text(tr("Show in folder"))
                                        .clicked()
                                {
                                    state.events.push(FileWindowEvent::Job {
                                        id: job.id,
                                        command: JobCommand::ShowInFolder,
                                    });
                                }
                                let pause = match job.state {
                                    JobRowState::Going | JobRowState::Waiting if job.can_pause => {
                                        Some((JobCommand::Pause, "⏸", tr("Pause")))
                                    }
                                    JobRowState::Paused => {
                                        Some((JobCommand::Resume, "▶", tr("Resume")))
                                    }
                                    _ => None,
                                };
                                if let Some((command, icon, tip)) = pause
                                    && ui.small_button(icon).on_hover_text(tip).clicked()
                                {
                                    state.events.push(FileWindowEvent::Job {
                                        id: job.id,
                                        command,
                                    });
                                }
                                if ui
                                    .small_button("🗑")
                                    .on_hover_text(tr("Remove from the list"))
                                    .clicked()
                                {
                                    state.events.push(FileWindowEvent::Job {
                                        id: job.id,
                                        command: JobCommand::Remove,
                                    });
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
    }
}

/// The checked entries of pane `side`, else the selected one.
fn checked_names(state: &FilesState, side: usize) -> Vec<String> {
    let checked: Vec<String> = state.panes[side]
        .items
        .iter()
        .filter(|item| state.checked[side].contains(&item.name))
        .map(|item| item.name.clone())
        .collect();
    if !checked.is_empty() {
        return checked;
    }
    state.selected[side].iter().cloned().collect()
}

impl View for FilesView {
    fn builder(&self) -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title(&self.title)
            .with_inner_size([1000.0, 680.0])
            .with_min_inner_size([780.0, 540.0])
    }

    fn draw(&mut self, ctx: &egui::Context) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if std::mem::take(&mut state.focus) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        // A light page, as on Windows: egui's light look for this window's own parts only (the GUI thread's other
        // windows keep theirs).
        let light = |ui: &mut egui::Ui| {
            let mut visuals = egui::Visuals::light();
            visuals.override_text_color = Some(TEXT);
            visuals.panel_fill = PAGE;
            ui.style_mut().visuals = visuals;
        };
        egui::TopBottomPanel::bottom("transfers")
            .exact_height(240.0)
            .frame(egui::Frame::new().fill(PAGE).inner_margin(12.0))
            .show(ctx, |ui| {
                light(ui);
                egui::Frame::new()
                    .fill(egui::Color32::WHITE)
                    .stroke(egui::Stroke::new(1.0_f32, CARD_LINE))
                    .corner_radius(12)
                    .inner_margin(10.0)
                    .show(ui, |ui| Self::jobs(ui, &mut state));
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(PAGE).inner_margin(12.0))
            .show(ctx, |ui| {
                light(ui);
                ui.columns(2, |columns| {
                    for (side, column) in columns.iter_mut().enumerate() {
                        Self::pane(column, &mut state, side);
                    }
                });
            });
        Self::keys(ctx, &mut state);
        Self::delete_modal(ctx, &mut state);
        true
    }

    fn closed(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.events.push(FileWindowEvent::Closed);
        }
    }
}

pub struct FileWindow {
    opened: Opened,
    state: Arc<Mutex<FilesState>>,
}

impl FileWindow {
    pub fn new(this_pc: &str, that_pc: &str) -> Result<FileWindow> {
        let state = Arc::new(Mutex::new(FilesState {
            names: [this_pc.to_string(), that_pc.to_string()],
            ..FilesState::default()
        }));
        let view = FilesView {
            state: state.clone(),
            title: trf("Queqiao - file transfer with {name}", &[("name", that_pc)]),
        };
        Ok(FileWindow {
            opened: gui::open(Box::new(view))?,
            state,
        })
    }

    pub fn hwnd(&self) -> Hwnd {
        self.opened.hwnd()
    }

    pub fn show(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.focus = true;
        }
        gui::repaint();
    }

    pub fn set_pane(&mut self, remote: bool, pane: &FilePane) {
        let side = usize::from(remote);
        if let Ok(mut state) = self.state.lock() {
            if state.panes[side] == *pane {
                return;
            }
            if state.panes[side].folder != pane.folder {
                state.editing[side] = None;
            }
            if state.panes[side].folder != pane.folder || state.panes[side].items != pane.items {
                state.checked[side].clear();
                state.selected[side] = None;
                state.typed[side] = if pane.folder.is_empty() {
                    if remote {
                        tr("That PC's places")
                    } else {
                        tr("This PC's places")
                    }
                    .to_string()
                } else {
                    pane.folder.clone()
                };
            }
            state.panes[side] = pane.clone();
        }
        gui::repaint();
    }

    pub fn set_jobs(&mut self, jobs: &[JobRow]) {
        if let Ok(mut state) = self.state.lock()
            && state.jobs != jobs
        {
            state.jobs = jobs.to_vec();
            gui::repaint();
        }
    }

    pub fn next_event(&mut self) -> Option<FileWindowEvent> {
        let mut state = self.state.lock().ok()?;
        (!state.events.is_empty()).then(|| state.events.remove(0))
    }

    pub fn save_picture(&self, path: &Path) -> std::io::Result<()> {
        self.opened.save_picture(path)
    }

    /// Tests: checks the entry `name` of a pane.
    pub fn check_for_test(&self, remote: bool, name: &str) -> bool {
        let side = usize::from(remote);
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if !state.panes[side].items.iter().any(|item| item.name == name) {
            return false;
        }
        state.checked[side].insert(name.to_string());
        true
    }

    /// Tests: presses a pane's Send button.
    pub fn send_for_test(&self, remote: bool) {
        let side = usize::from(remote);
        if let Ok(mut state) = self.state.lock() {
            let names = checked_names(&state, side);
            state.events.push(FileWindowEvent::Send {
                from_remote: remote,
                names,
            });
        }
    }
}

/// Opens the file manager at the folder of `path` (a file or folder that arrived, or one sent).
pub fn show_in_folder(path: &Path) {
    let folder = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    let _ = std::process::Command::new("xdg-open").arg(folder).spawn();
}
