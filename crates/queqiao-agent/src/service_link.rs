//! The agent's side of the channels the service hands it when it starts the agent (`--service-channel`,
//! `--desktop-channel`): requests to the service (Ctrl+Alt+Del, restarting Queqiao), and the desktop helper, which
//! shows and works the sign-in, lock, Ctrl+Alt+Del and UAC screens that this agent, running as the person, may not.
//! Without the service (started by hand or at sign-in), both are absent and those screens end a session.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use queqiao_core::protocol::InputEvent;
use queqiao_input::ScreenRect;
use queqiao_local::{Ends, FromHelper, FromService, LogonIdentity, ToHelper, ToService, Writer};

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
    /// What the service hands over by itself (Linux: the sign-in screen was used where no notice could be shown).
    notices: Receiver<String>,
}

impl ServiceLink {
    fn open(argument: &str) -> Result<ServiceLink, String> {
        let (mut reader, writer) = ends(argument, "--service-channel")?.split();
        let (sender, replies) = mpsc::channel();
        let (notice_sender, notices) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(message) = reader.read() {
                let sent = match FromService::decode(&message) {
                    Some(FromService::Notice(notice)) => notice_sender.send(notice).is_ok(),
                    Some(reply) => sender.send(reply).is_ok(),
                    None => false,
                };
                if !sent {
                    break;
                }
            }
        });
        Ok(ServiceLink {
            writer,
            replies,
            notices,
        })
    }

    /// The agent at the sign-in screen: who to be, as the service hands it over right after the start.
    pub fn identity(&mut self, wait: Duration) -> Result<LogonIdentity, String> {
        match self.replies.recv_timeout(wait) {
            Ok(FromService::Identity(identity)) => Ok(identity),
            Ok(_) => Err("the service sent something else first".into()),
            Err(_) => Err("the service did not hand over this device".into()),
        }
    }

    /// Hands this device to the service for its agent at the sign-in screen (None: the person does not allow
    /// control there; the service forgets it).
    pub fn offer_logon(&mut self, identity: Option<LogonIdentity>) -> Result<(), String> {
        let message = match identity {
            Some(identity) => ToService::LogonIdentity(identity),
            None => ToService::ForgetLogon,
        };
        self.writer.write(&message.encode())
    }

    /// Ctrl+Alt+Del on this PC's screen; the service's answer.
    pub fn send_sas(&mut self) -> String {
        while self.replies.try_recv().is_ok() {}
        if let Err(error) = self.writer.write(&ToService::SendSas.encode()) {
            return format!("the Queqiao service did not take it: {error}");
        }
        match self.replies.recv_timeout(Duration::from_secs(3)) {
            Ok(FromService::Reply(reply)) => reply,
            Ok(_) => "the Queqiao service answered something else".into(),
            Err(_) => "the Queqiao service did not answer".into(),
        }
    }

    /// Hands an update to the service for its own copy of Queqiao (it checks all of it itself); its answer.
    pub fn update(
        &mut self,
        manifest: &[u8],
        signature: &str,
        installer: Vec<u8>,
    ) -> Result<String, String> {
        while self.replies.try_recv().is_ok() {}
        let message = ToService::Update {
            manifest: manifest.to_vec(),
            signature: signature.to_string(),
            installer,
        };
        self.writer.write(&message.encode())?;
        match self.replies.recv_timeout(Duration::from_secs(30)) {
            Ok(FromService::Reply(reply)) if reply.starts_with("refused") => Err(reply),
            Ok(FromService::Reply(reply)) => Ok(reply),
            Ok(_) => Err("the Queqiao service answered something else".into()),
            Err(_) => Err("the Queqiao service did not answer".into()),
        }
    }

    /// Asks the service to end this agent and its helper and start both again.
    pub fn restart(&mut self) -> Result<(), String> {
        self.writer.write(&ToService::Restart.encode())
    }

    /// Linux: whether `password` is that of the account this agent is for, as the service checks it (PAM, as root;
    /// which account is the service's choice). It can take a few seconds (PAM's delay after a wrong one).
    #[cfg_attr(windows, allow(dead_code))]
    pub fn check_password(&mut self, password: &str) -> Result<bool, String> {
        while self.replies.try_recv().is_ok() {}
        self.writer
            .write(&ToService::CheckPassword(password.to_string()).encode())?;
        match self.replies.recv_timeout(Duration::from_secs(20)) {
            Ok(FromService::Password(right)) => Ok(right),
            Ok(_) => Err("the Queqiao service answered something else".into()),
            Err(_) => Err("the Queqiao service did not answer".into()),
        }
    }

    /// Linux, the agent for the sign-in screen: tells the service what the next person to sign in is to be shown.
    #[cfg_attr(windows, allow(dead_code))]
    pub fn leave_notice(&mut self, notice: &str) -> Result<(), String> {
        self.writer
            .write(&ToService::Notice(notice.to_string()).encode())
    }

    /// What the service handed over by itself since the last call.
    #[cfg_attr(windows, allow(dead_code))]
    pub fn notices(&mut self) -> Vec<String> {
        self.notices.try_iter().collect()
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
    /// The person pressed End on the helper's banner (taken by the session).
    pub end_pressed: bool,
    /// The helper could not show its banner (taken by the session, which ends).
    pub no_banner: Option<String>,
    /// The captured screen's place, as the helper last said.
    pub screen: Option<ScreenRect>,
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
            end_pressed: false,
            no_banner: None,
            screen: None,
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

    /// The session banner the helper shows off the person's own desktop (None: none).
    pub fn banner(&mut self, viewer: Option<&str>) {
        self.send(ToHelper::Banner(viewer.map(str::to_string)));
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
            Ok(FromHelper::EndPressed) => {
                self.end_pressed = true;
                Some(FromHelper::EndPressed)
            }
            Ok(FromHelper::NoBanner(reason)) => {
                self.no_banner = Some(reason.clone());
                Some(FromHelper::NoBanner(reason))
            }
            Ok(FromHelper::Screen {
                left,
                top,
                width,
                height,
            }) => {
                self.screen = Some(ScreenRect {
                    left,
                    top,
                    width,
                    height,
                });
                Some(FromHelper::Screen {
                    left,
                    top,
                    width,
                    height,
                })
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
