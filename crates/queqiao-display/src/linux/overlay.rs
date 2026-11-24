//! The session bar and its card of figures on Linux (Windows: overlay.rs): they come with Queqiao's own GUI
//! (docs/LINUX.md step 5). Until then the bar is refused when a session asks for it, and the session goes on without
//! (it says so); this type exists so the shared session reads the same.

use super::Result;
use super::window::Hwnd;
use crate::model::{BarButton, Figures};

pub struct SessionOverlay {
    _never: std::convert::Infallible,
}

impl SessionOverlay {
    pub fn new(_owner: Hwnd, _name: &str, _buttons: Vec<BarButton>) -> Result<SessionOverlay> {
        Err("the session bar: not on Linux yet (docs/LINUX.md step 5)".into())
    }

    pub fn set_card(&mut self, _on: bool) {
        match self._never {}
    }

    pub fn card_shown(&self) -> bool {
        match self._never {}
    }

    pub fn keep_bar_out(&mut self, _on: bool) {
        match self._never {}
    }

    pub fn click_for_test(&self, _id: u16) -> bool {
        match self._never {}
    }

    pub fn hover_for_test(&mut self, _index: Option<usize>) {
        match self._never {}
    }

    pub fn set_figures(&mut self, _figures: Figures) {
        match self._never {}
    }

    pub fn tick(&mut self) {
        match self._never {}
    }
}
