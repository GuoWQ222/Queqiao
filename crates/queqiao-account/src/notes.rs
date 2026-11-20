//! Notes the person keeps about the account's devices on this PC: a name of their own (remark) and favourites.
//! They stay on this PC (device_notes.json in the data folder); the server never sees them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceNote {
    #[serde(default)]
    pub remark: String,
    #[serde(default)]
    pub favourite: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DeviceNotes {
    #[serde(default)]
    notes: BTreeMap<String, DeviceNote>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl DeviceNotes {
    /// The notes saved on this PC (none yet, or unreadable: empty).
    pub fn load() -> DeviceNotes {
        let path = crate::config::folder()
            .ok()
            .map(|folder| folder.join("device_notes.json"));
        let mut notes = path
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<DeviceNotes>(&text).ok())
            .unwrap_or_default();
        notes.path = path;
        notes
    }

    /// Notes kept in memory only (tests).
    pub fn in_memory() -> DeviceNotes {
        DeviceNotes::default()
    }

    pub fn get(&self, device: &str) -> DeviceNote {
        self.notes.get(device).cloned().unwrap_or_default()
    }

    /// A remark (trimmed, at most 60 characters; empty removes it).
    pub fn set_remark(&mut self, device: &str, remark: &str) -> Result<(), String> {
        let remark: String = remark
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(60)
            .collect();
        self.notes.entry(device.to_string()).or_default().remark = remark;
        self.tidy_and_save(device)
    }

    pub fn set_favourite(&mut self, device: &str, favourite: bool) -> Result<(), String> {
        self.notes.entry(device.to_string()).or_default().favourite = favourite;
        self.tidy_and_save(device)
    }

    /// How the device is shown: its remark with the device's own name, or the name alone; a star for favourites.
    pub fn label(&self, device: &str, name: &str) -> String {
        let note = self.get(device);
        let star = if note.favourite { "\u{2605} " } else { "" };
        if note.remark.is_empty() {
            format!("{star}{name}")
        } else {
            format!("{star}{} ({name})", note.remark)
        }
    }

    fn tidy_and_save(&mut self, device: &str) -> Result<(), String> {
        if self
            .notes
            .get(device)
            .is_some_and(|note| *note == DeviceNote::default())
        {
            self.notes.remove(device);
        }
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remarks_and_favourites_show_in_the_label() {
        let mut notes = DeviceNotes::in_memory();
        assert_eq!(notes.label("d1", "DESKTOP-1"), "DESKTOP-1");
        notes.set_remark("d1", "  Office PC\n ").unwrap();
        assert_eq!(notes.label("d1", "DESKTOP-1"), "Office PC (DESKTOP-1)");
        notes.set_favourite("d1", true).unwrap();
        assert_eq!(
            notes.label("d1", "DESKTOP-1"),
            "\u{2605} Office PC (DESKTOP-1)"
        );
        notes.set_remark("d1", "").unwrap();
        notes.set_favourite("d1", false).unwrap();
        // Nothing left to keep for d1.
        assert!(notes.notes.is_empty());
        let long: String = "x".repeat(200);
        notes.set_remark("d2", &long).unwrap();
        assert_eq!(notes.get("d2").remark.len(), 60);
    }
}
