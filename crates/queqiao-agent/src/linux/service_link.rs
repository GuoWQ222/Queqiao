//! The service's channels on Linux (docs/LINUX.md): not yet. On Windows the Queqiao service starts the agent and hands
//! it two channels (requests to the service; the desktop helper for the sign-in, lock and UAC screens). The Linux
//! service (a systemd unit with a root helper for the greeter and uinput) comes in step 6; until then an agent is
//! started by the person, both links are absent, and these types exist only so the shared session reads the same.
//! They cannot be made: each holds an `Infallible`.

use std::convert::Infallible;
use std::time::Duration;

use queqiao_core::protocol::InputEvent;
use queqiao_input::ScreenRect;
use queqiao_local::FromHelper;

#[derive(Default)]
pub struct Links {
    pub service: Option<ServiceLink>,
    pub desktop: Option<DesktopLink>,
}

impl Links {
    /// No links: channels named on the command line are refused (only the Windows service hands them over).
    pub fn open(service: Option<&str>, desktop: Option<&str>) -> Result<Links, String> {
        if service.is_some() || desktop.is_some() {
            return Err(
                "the Queqiao service's channels: Windows only so far (docs/LINUX.md step 6)".into(),
            );
        }
        Ok(Links::default())
    }
}

pub struct ServiceLink {
    never: Infallible,
}

impl ServiceLink {
    pub fn send_sas(&mut self) -> String {
        match self.never {}
    }

    pub fn restart(&mut self) -> Result<(), String> {
        match self.never {}
    }

    pub fn offer_logon(
        &mut self,
        _identity: Option<queqiao_local::LogonIdentity>,
    ) -> Result<(), String> {
        match self.never {}
    }
}

pub struct DesktopLink {
    pub closed: bool,
    pub end_pressed: bool,
    pub no_banner: Option<String>,
    pub screen: Option<ScreenRect>,
    never: Infallible,
}

impl DesktopLink {
    pub fn session(&mut self, _on: bool) {
        match self.never {}
    }

    pub fn banner(&mut self, _viewer: Option<&str>) {
        match self.never {}
    }

    pub fn capture(&mut self, _screen: usize) {
        match self.never {}
    }

    pub fn stop_capture(&mut self) {
        match self.never {}
    }

    pub fn input(&mut self, _event: InputEvent, _screen: ScreenRect) {
        match self.never {}
    }

    pub fn on_own_desktop(&self) -> bool {
        match self.never {}
    }

    pub fn next(&mut self, _wait: Duration) -> Option<FromHelper> {
        match self.never {}
    }

    pub fn poll_desktop(&mut self) -> Vec<String> {
        match self.never {}
    }
}
