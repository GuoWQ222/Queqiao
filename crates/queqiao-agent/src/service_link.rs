//! The agent's side of the channels the service hands it when it starts the agent (`--service-channel`,
//! `--desktop-channel`): requests to the service (Ctrl+Alt+Del, restarting Queqiao), and the desktop helper, which
//! shows and works the sign-in, lock, Ctrl+Alt+Del and UAC screens that this agent, running as the person, may not.
//! Without the service (started by hand or at sign-in), both are absent and those screens end a session.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use queqiao_core::protocol::InputEvent;
use queqiao_input::ScreenRect;
use queqiao_local::{Ends, FromHelper, FromService, ToHelper, ToService, Writer};

/// The person's own desktop, as the helper names it.
pub const OWN_DESKTOP: &str = "Default";

#[derive(Default)]
pub struct Links {
    pub service: Option<ServiceLink>,
    pub desktop: Option<DesktopLink>,
}

impl Links {
    pub fn open(service: Option<&str>, desktop: Option<&str>) -> Result<Links, String> {
        Ok(Links {
            service: service.map(ServiceLink::open).transpose()?,
            desktop: desktop.map(DesktopLink::open).transpose()?,
        })
    }
}

fn ends(argument: &str, what: &str) -> Result<Ends, String> {
    Ends::parse(argument)
        .ok_or_else(|| format!("{what}: not a pair of pipes handed over by the service"))
}

pub struct ServiceLink {
    writer: Writer,
    replies: Receiver<FromService>,
}

impl ServiceLink {
    fn open(argument: &str) -> Result<ServiceLink, String> {
        let (mut reader, writer) = ends(argument, "--service-channel")?.split();
        let (sender, replies) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(message) = reader.read() {
                let Some(reply) = FromService::decode(&message) else {
                    break;
                };
                if sender.send(reply).is_err() {
                    break;
                }
            }
        });
        Ok(ServiceLink { writer, replies })
    }

    /// Ctrl+Alt+Del on this PC's screen; the service's answer.
    pub fn send_sas(&mut self) -> String {
        while self.replies.try_recv().is_ok() {}
        if let Err(error) = self.writer.write(&ToService::SendSas.encode()) {
            return format!("the Queqiao service did not take it: {error}");
        }
        match self.replies.recv_timeout(Duration::from_secs(3)) {
            Ok(FromService::Reply(reply)) => reply,
            Err(_) => "the Queqiao service did not answer".into(),
        }
    }

    /// Asks the service to end this agent and its helper and start both again.
    pub fn restart(&mut self) -> Result<(), String> {
        self.writer.write(&ToService::Restart.encode())
    }
}

pub struct DesktopLink {
    writer: Writer,
    events: Receiver<FromHelper>,
    /// The desktop that takes input, as the helper last said ("" until it has).
    pub desktop: String,
    /// The helper is capturing for this agent.
    pub capturing: bool,
    /// The helper's channel is gone.
    pub closed: bool,
}

impl DesktopLink {
    fn open(argument: &str) -> Result<DesktopLink, String> {
        let (mut reader, writer) = ends(argument, "--desktop-channel")?.split();
        let (sender, events) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(message) = reader.read() {
                let Some(event) = FromHelper::decode(&message) else {
                    break;
                };
                if sender.send(event).is_err() {
                    break;
                }
            }
        });
        Ok(DesktopLink {
            writer,
            events,
            desktop: String::new(),
            capturing: false,
            closed: false,
        })
    }

    fn send(&mut self, message: ToHelper) {
        if self.writer.write(&message.encode()).is_err() {
            self.closed = true;
        }
    }

    pub fn session(&mut self, on: bool) {
        self.send(ToHelper::Session(on));
        if !on && self.capturing {
            self.stop_capture();
        }
    }

    pub fn capture(&mut self, screen: usize) {
        self.send(ToHelper::Capture {
            screen: screen.min(255) as u8,
        });
        self.capturing = true;
    }

    pub fn stop_capture(&mut self) {
        self.send(ToHelper::StopCapture);
        self.capturing = false;
    }

    pub fn input(&mut self, event: InputEvent, screen: ScreenRect) {
        self.send(ToHelper::Input {
            event,
            left: screen.left,
            top: screen.top,
            width: screen.width,
            height: screen.height,
        });
    }

    /// Whether the desktop taking input is the person's own (or not known yet).
    pub fn on_own_desktop(&self) -> bool {
        self.desktop.is_empty() || self.desktop == OWN_DESKTOP
    }

    /// The next picture or failure from the helper within `wait`; desktop changes are taken in on the way.
    pub fn next(&mut self, wait: Duration) -> Option<FromHelper> {
        let event = if wait.is_zero() {
            self.events
                .try_recv()
                .map_err(|_| RecvTimeoutError::Timeout)
        } else {
            self.events.recv_timeout(wait)
        };
        match event {
            Ok(FromHelper::Desktop(name)) => {
                self.desktop = name.clone();
                Some(FromHelper::Desktop(name))
            }
            Ok(other) => Some(other),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                self.closed = true;
                None
            }
        }
    }

    /// Takes in desktop changes without waiting, dropping pictures (when this agent captures itself); the names
    /// of the desktops that took input meanwhile.
    pub fn poll_desktop(&mut self) -> Vec<String> {
        let mut names = Vec::new();
        while let Some(event) = self.next(Duration::ZERO) {
            if let FromHelper::Desktop(name) = event {
                names.push(name);
            }
        }
        names
    }
}
