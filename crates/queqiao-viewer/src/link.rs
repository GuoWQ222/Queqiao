//! A session on a thread of its own (the device list's sessions) still signals through the account during the session
//! (an ICE restart after a network change: `queqiao_transport::Restarter`), and the account lives on the list's thread.
//! [`LinkedSignal`] is the session thread's [`Signaling`], through channels; [`SessionLink`] is the list thread's end,
//! served on each pass of its loop: what the session sends goes out through the account (signed there), the other
//! side's restart messages for this session come in (checked there), the server connection's state and address and
//! the relays when asked.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use queqiao_account::{Account, AccountEvent, Role as AccountRole};
use queqiao_core::account::PathReport;
use queqiao_transport::{Error, IceServers, Signaling};

/// The messages of an ICE restart a viewer receives.
const RESTART_KINDS: [&str; 2] = ["restart-request", "restart-answer"];

/// What the session thread asks of the list's thread.
enum Request {
    Send(String, String),
    IceServers(Sender<Result<IceServers, String>>),
    Report(PathReport),
}

/// The session thread's signaling during the session.
pub(crate) struct LinkedSignal {
    requests: Sender<Request>,
    inbox: Receiver<(String, String)>,
    held: Vec<(String, String)>,
    online: Arc<AtomicBool>,
    local_ip: Arc<Mutex<IpAddr>>,
}

/// The list thread's end of one session's link.
pub(crate) struct SessionLink {
    agent: String,
    session: String,
    requests: Receiver<Request>,
    inbox: Sender<(String, String)>,
    online: Arc<AtomicBool>,
    local_ip: Arc<Mutex<IpAddr>>,
}

/// A link for the session `session` with the agent `agent`: the list's end and the session's.
pub(crate) fn pair(agent: &str, session: &str, local_ip: IpAddr) -> (SessionLink, LinkedSignal) {
    let (request_sender, requests) = mpsc::channel();
    let (inbox_sender, inbox) = mpsc::channel();
    let online = Arc::new(AtomicBool::new(true));
    let local_ip = Arc::new(Mutex::new(local_ip));
    (
        SessionLink {
            agent: agent.to_string(),
            session: session.to_string(),
            requests,
            inbox: inbox_sender,
            online: online.clone(),
            local_ip: local_ip.clone(),
        },
        LinkedSignal {
            requests: request_sender,
            inbox,
            held: Vec::new(),
            online,
            local_ip,
        },
    )
}

/// A session's signaling that goes nowhere (a test that runs sessions on threads without the list's loop).
pub(crate) fn unlinked() -> LinkedSignal {
    pair("", "", IpAddr::V4(Ipv4Addr::UNSPECIFIED)).1
}

impl SessionLink {
    /// On each pass of the list thread's loop: the other side's restart messages for this session in, what the session
    /// sends out, the relays when asked (waits for the server, up to 5 s), the connection's state and address.
    pub(crate) fn serve(&self, account: &mut Account) {
        let mut signal = account.session(&self.agent, AccountRole::Agent, &self.session);
        self.online.store(signal.online(), Ordering::Relaxed);
        for kind in RESTART_KINDS {
            while let Some(body) = signal.take(kind) {
                let _ = self.inbox.send((kind.to_string(), body));
            }
        }
        while let Ok(request) = self.requests.try_recv() {
            match request {
                Request::Send(kind, body) => {
                    let _ = signal.send(&kind, &body);
                }
                Request::IceServers(reply) => {
                    let _ = reply.send(signal.ice_servers().map_err(|error| error.to_string()));
                }
                Request::Report(report) => signal.report_path(&report),
            }
        }
        if let Ok(mut ip) = self.local_ip.lock() {
            *ip = account.local_ip();
        }
    }

    /// A message the list's loop took from the account: this session's restart message, checked and handed on (true),
    /// or not this session's (false).
    pub(crate) fn forward(&self, account: &mut Account, event: &AccountEvent) -> bool {
        let AccountEvent::Signal {
            from,
            from_role,
            session,
            kind,
            body,
            signature,
            ..
        } = event
        else {
            return false;
        };
        if *from != self.agent
            || *session != self.session
            || !RESTART_KINDS.contains(&kind.as_str())
        {
            return false;
        }
        if account
            .check_signal(from, *from_role, session, kind, body, signature)
            .is_ok()
        {
            let _ = self.inbox.send((kind.clone(), body.clone()));
        }
        true
    }
}

impl Signaling for LinkedSignal {
    fn send(&mut self, kind: &str, body: &str) -> Result<(), Error> {
        self.requests
            .send(Request::Send(kind.to_string(), body.to_string()))
            .map_err(|_| Error("the device list is closed".into()))
    }

    fn expect(&mut self, kind: &str) -> Result<String, Error> {
        Err(Error(format!("no {kind} during a session")))
    }

    fn local_ip(&self) -> Result<IpAddr, Error> {
        self.local_ip
            .lock()
            .map(|ip| *ip)
            .map_err(|_| Error("the link is broken".into()))
    }

    fn ice_servers(&mut self) -> Result<IceServers, Error> {
        let (reply, answer) = mpsc::channel();
        self.requests
            .send(Request::IceServers(reply))
            .map_err(|_| Error("the device list is closed".into()))?;
        match answer.recv_timeout(Duration::from_secs(6)) {
            Ok(result) => result.map_err(Error),
            Err(_) => Err(Error("no relays from the server in 6 s".into())),
        }
    }

    fn take(&mut self, kind: &str) -> Option<String> {
        while let Ok(message) = self.inbox.try_recv() {
            self.held.push(message);
        }
        let at = self.held.iter().position(|(got, _)| got == kind)?;
        Some(self.held.remove(at).1)
    }

    fn online(&mut self) -> bool {
        self.online.load(Ordering::Relaxed)
    }

    fn report_path(&mut self, report: &PathReport) {
        let _ = self.requests.send(Request::Report(report.clone()));
    }
}
