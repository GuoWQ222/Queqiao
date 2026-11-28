//! The service (Linux): the supervisor that keeps the desktop helper and an agent running for the session on the PC's
//! own screen (seat0), as the Windows service does for the console session.
//!
//! The two start together, with a fresh channel between them, and end together: when either ends, the session on the
//! screen changes, or the agent asks to be restarted. The agent's exit code says whether to start it again: 0 (the
//! person quit it) and 3 (not logged in to an account) wait for the next session on the screen; anything else is a
//! failure, tried again after 1, 2, 4 ... 60 s.
//!
//! A person's desktop gets their own agent. The sign-in screen (logind's greeter, or a lock screen run as a session of
//! its own) gets the agent for the sign-in screen, when the person handed this device over for it. A text console or
//! a remote session gets none.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use queqiao_local::{FromService, ToService, Writer};

use super::launch::{self, Child, Identity};
use super::logind::{Logind, Session};
use super::{RUNTIME, log, logon};

/// The agent's exit codes that mean "do not start me again until the next session on the screen".
const AGENT_QUIT: i32 = 0;
const AGENT_NOT_LOGGED_IN: i32 = 3;
/// Replies waiting unread before the pair is ended (as on Windows).
const MAX_REPLIES: usize = 64;
/// Requests from the agents waiting for the service.
const MAX_REQUESTS: usize = 16;
/// Wrong passwords the service checks for all agents together within PASSWORD_WINDOW: the agents' own gate allows
/// fewer per session; this holds whatever an agent asks.
const PASSWORD_TRIES: usize = 10;
const PASSWORD_WINDOW: Duration = Duration::from_secs(15 * 60);

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_signal: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Read again before every start: `/etc/queqiao/service.conf` (root's). `restart_agent=0` (tests: an agent runs
/// once); `agent_arguments=` and `logon_arguments=` (more options for the agents) only in builds with the
/// `test-switches` feature: a shipping service starts its agents with nothing but their channels.
struct Settings {
    agent_arguments: Vec<String>,
    logon_arguments: Vec<String>,
    restart_agent: bool,
}

impl Settings {
    fn read() -> Settings {
        let mut settings = Settings {
            agent_arguments: Vec::new(),
            logon_arguments: Vec::new(),
            restart_agent: true,
        };
        let text = std::fs::read_to_string("/etc/queqiao/service.conf").unwrap_or_default();
        for line in text.lines() {
            let words = |value: &str| value.split_whitespace().map(str::to_string).collect();
            match line.split_once('=') {
                Some(("restart_agent", value)) => settings.restart_agent = value.trim() != "0",
                Some(("agent_arguments", value)) if cfg!(feature = "test-switches") => {
                    settings.agent_arguments = words(value)
                }
                Some(("logon_arguments", value)) if cfg!(feature = "test-switches") => {
                    settings.logon_arguments = words(value)
                }
                _ => {}
            }
        }
        settings
    }
}

/// Replies to an agent, written by a thread of their own (as on Windows: an agent that stopped reading would
/// otherwise hold the service).
#[derive(Clone)]
struct Outbox(SyncSender<Vec<u8>>);

impl Outbox {
    fn new(mut writer: Writer) -> Outbox {
        let (sender, replies) = mpsc::sync_channel::<Vec<u8>>(MAX_REPLIES);
        std::thread::spawn(move || {
            while let Ok(reply) = replies.recv() {
                if writer.write(&reply).is_err() {
                    break;
                }
            }
        });
        Outbox(sender)
    }

    fn send(&self, reply: Vec<u8>) -> bool {
        self.0.try_send(reply).is_ok()
    }
}

/// The helper and the agent of one session on the screen.
struct Pair {
    id: u64,
    session: Session,
    /// The agent for the sign-in screen, not a person's.
    logon: bool,
    /// The person's number (as text) and account name; empty for the sign-in screen.
    owner: String,
    account: String,
    helper: Child,
    agent: Child,
    to_agent: Outbox,
    started: Instant,
    started_unix: u64,
    /// The helper's private copy of the X authority file (see `private_authority`).
    authority_copy: Option<PathBuf>,
}

impl Pair {
    fn end(self, reason: &str) {
        let (agent, helper) = (self.agent.pid, self.helper.pid);
        self.agent.end();
        self.helper.end();
        if let Some(copy) = &self.authority_copy {
            let _ = std::fs::remove_file(copy);
        }
        log::line(&format!(
            "session {}: helper {helper} and agent {agent} ended: {reason}",
            self.session.id
        ));
    }
}

fn folder() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("/usr/lib/queqiao"))
}

/// The X server's authority file for `display`, from the server's own command line (`-auth <file>`): how the
/// helper reaches a sign-in screen on X11 for its notice.
fn x_server_authority(display: &str) -> Option<String> {
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(command) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let words: Vec<&[u8]> = command.split(|&b| b == 0).collect();
        let program = words
            .first()
            .map(|w| String::from_utf8_lossy(w))
            .unwrap_or_default();
        if !(program.ends_with("Xorg") || program.ends_with("X") || program.contains("Xorg")) {
            continue;
        }
        if !words.contains(&display.as_bytes()) {
            continue;
        }
        if let Some(at) = words.iter().position(|w| *w == b"-auth") {
            return words
                .get(at + 1)
                .map(|w| String::from_utf8_lossy(w).into_owned());
        }
    }
    None
}

/// The largest X authority file taken (they hold a few cookies: well under 1 KiB).
const MAX_AUTHORITY: u64 = 64 * 1024;

/// A private copy of an X authority file for the desktop helper: `/run/queqiao/xauth-<pair>`, root's only (0600). The
/// helper reads that copy, never a path a person's programs name: it is root and may read any file, and the security
/// review of 2026-11-26 found it read whatever file a person's XAUTHORITY named and sent the "cookie" in it to the X
/// server. The original is opened without following a link at its end, must be a plain file (not a pipe or device,
/// which could also hold the service up), at most MAX_AUTHORITY, and, when `owner` is given, that person's own (a
/// path through a linked folder to someone else's file fails this too).
fn private_authority(path: &str, owner: Option<u32>, copy: &Path) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("{path}: {e}"))?;
    let meta = file.metadata().map_err(|e| format!("{path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path} is not a plain file"));
    }
    if let Some(owner) = owner
        && meta.uid() != owner
    {
        return Err(format!("{path} is not the person's own file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_AUTHORITY + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() as u64 > MAX_AUTHORITY {
        return Err(format!("{path} is too large for an X authority file"));
    }
    let _ = std::fs::remove_file(copy);
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(copy)
        .map_err(|e| format!("{}: {e}", copy.display()))?;
    out.write_all(&bytes)
        .map_err(|e| format!("{}: {e}", copy.display()))
}

fn start_pair(
    id: u64,
    session: &Session,
    settings: &Settings,
    requests: &SyncSender<(u64, ToService)>,
) -> Result<Pair, String> {
    let folder = folder();
    let logon = !session.is_desktop();
    let (identity, environment, handed, owner, account) = if logon {
        if !logon::stored() {
            return Err("control at the sign-in screen is not set up".into());
        }
        let handed = logon::load().ok_or(
            "the device for the sign-in screen cannot be read (made anew at the next sign-in)",
        )?;
        let account = launch::account_by_name(launch::SIGN_IN_USER)?;
        let environment = launch::plain_environment(Some(&account));
        (
            Identity::SignIn(account),
            environment,
            Some(handed),
            String::new(),
            String::new(),
        )
    } else {
        let account = launch::account_by_uid(session.uid)?;
        let environment = launch::session_environment(session, &account)?;
        let name = account.name.clone();
        (
            Identity::Person(account),
            environment,
            None,
            session.uid.to_string(),
            name,
        )
    };
    // The helper: its channel, the session it watches, and an X11 screen's display for the notice.
    let (helper_ends, agent_desktop_ends) = queqiao_local::channel()?;
    let mut helper_arguments = vec![
        "desktop".to_string(),
        "--channel".to_string(),
        helper_ends.argument(),
        "--session".to_string(),
        session.id.clone(),
    ];
    if logon {
        helper_arguments.push("--sign-in".into());
    }
    let mut authority_copy = None;
    if session.kind == "x11" && !session.display.is_empty() {
        // The person's own (named by their programs: theirs only), else the X server's (root's command line).
        let authority = environment
            .iter()
            .find(|(name, _)| name == "XAUTHORITY")
            .map(|(_, value)| (value.clone(), (!logon).then_some(session.uid)))
            .or_else(|| x_server_authority(&session.display).map(|path| (path, None)));
        helper_arguments.extend(["--x11-display".to_string(), session.display.clone()]);
        if let Some((path, owner)) = authority {
            let copy = Path::new(RUNTIME).join(format!("xauth-{id}"));
            match private_authority(&path, owner, &copy) {
                Ok(()) => {
                    helper_arguments
                        .extend(["--x11-authority".to_string(), copy.display().to_string()]);
                    authority_copy = Some(copy);
                }
                Err(error) => log::line(&format!(
                    "session {}: the X authority file is not used ({error}): no banner on that screen",
                    session.id
                )),
            }
        }
    }
    let helper = launch::start(
        &folder.join("queqiao-service"),
        &helper_arguments,
        &Identity::Helper,
        &launch::plain_environment(None),
        &helper_ends.for_child()?,
    )
    .inspect_err(|_| {
        if let Some(copy) = &authority_copy {
            let _ = std::fs::remove_file(copy);
        }
    })?;
    drop(helper_ends);
    let (ours, theirs) = queqiao_local::channel()?;
    let mut inherit = theirs.for_child()?.to_vec();
    inherit.extend(agent_desktop_ends.for_child()?);
    let mut arguments = if logon {
        let mut arguments = vec!["--logon".to_string()];
        arguments.extend(settings.logon_arguments.iter().cloned());
        arguments
    } else {
        settings.agent_arguments.clone()
    };
    arguments.extend([
        "--service-channel".to_string(),
        theirs.argument(),
        "--desktop-channel".to_string(),
        agent_desktop_ends.argument(),
    ]);
    let agent = match launch::start(
        &folder.join("queqiao-agent"),
        &arguments,
        &identity,
        &environment,
        &inherit,
    ) {
        Ok(agent) => agent,
        Err(error) => {
            helper.end();
            if let Some(copy) = &authority_copy {
                let _ = std::fs::remove_file(copy);
            }
            return Err(error);
        }
    };
    drop((theirs, agent_desktop_ends));
    let (mut from_agent, mut to_agent) = ours.split();
    // The agent for the sign-in screen learns who it is first.
    if let Some(handed) = handed
        && let Err(error) = to_agent.write(&FromService::Identity(handed).encode())
    {
        agent.end();
        helper.end();
        if let Some(copy) = &authority_copy {
            let _ = std::fs::remove_file(copy);
        }
        return Err(format!("handing over the device: {error}"));
    }
    let to_agent = Outbox::new(to_agent);
    // The person's agent: what happened at the sign-in screen while no notice could be shown there.
    if !logon {
        for notice in logon::take_notices() {
            log::line(&format!(
                "session {}: notice handed to {account}: {notice}",
                session.id
            ));
            to_agent.send(FromService::Notice(notice).encode());
        }
    }
    let requests = requests.clone();
    std::thread::spawn(move || {
        while let Ok(message) = from_agent.read() {
            match ToService::decode(&message) {
                Some(request) => {
                    if requests.send((id, request)).is_err() {
                        break;
                    }
                }
                None => log::line("the agent sent a request the service does not know: ignored"),
            }
        }
    });
    log::line(&format!(
        "session {} ({} {}, {}): started the desktop helper ({}) and {} ({})",
        session.id,
        session.class,
        session.kind,
        session.user,
        helper.pid,
        if logon {
            "the agent for the sign-in screen"
        } else {
            "the agent"
        },
        agent.pid
    ));
    Ok(Pair {
        id,
        session: session.clone(),
        logon,
        owner,
        account,
        helper,
        agent,
        to_agent,
        started: Instant::now(),
        started_unix: queqiao_update::unix_now(),
        authority_copy,
    })
}

/// What `status` shows, and what an update checks after it installed (the new service's version, its agent).
fn write_status(pair: Option<&Pair>) {
    let mut text = format!(
        "version={}\npid={}\n",
        queqiao_core::VERSION,
        std::process::id()
    );
    if let Some(pair) = pair {
        text.push_str(&format!(
            "session={}\nkind={} {}\nuser={}\nsign_in={}\nagent_pid={}\nagent_since={}\n",
            pair.session.id,
            pair.session.class,
            pair.session.kind,
            pair.session.user,
            pair.logon,
            pair.agent.pid,
            pair.started_unix
        ));
    }
    let _ = std::fs::write(Path::new(RUNTIME).join("status"), text);
}

pub fn status() -> String {
    match std::fs::read_to_string(Path::new(RUNTIME).join("status")) {
        Ok(text) => text.trim_end().to_string(),
        Err(_) => "the Queqiao service does not run".into(),
    }
}

/// Checks a password with PAM on a thread of its own and answers the agent; at most PASSWORD_TRIES wrong ones in
/// PASSWORD_WINDOW for all agents together.
fn check_password(
    account: String,
    password: String,
    to_agent: Outbox,
    failures: Arc<Mutex<Vec<Instant>>>,
) {
    std::thread::spawn(move || {
        let over = failures.lock().map_or(true, |mut failures| {
            failures.retain(|at| at.elapsed() < PASSWORD_WINDOW);
            failures.len() >= PASSWORD_TRIES
        });
        let right = !over
            && !account.is_empty()
            && match queqiao_core::pam::password_ok(&account, &password) {
                Ok(right) => right,
                Err(error) => {
                    log::line(&format!("password check: {error}"));
                    false
                }
            };
        if !right && let Ok(mut failures) = failures.lock() {
            failures.push(Instant::now());
        }
        log::line(&format!(
            "password for {account}: {}",
            if over {
                "not checked (too many wrong ones lately)"
            } else if right {
                "right"
            } else {
                "wrong"
            }
        ));
        to_agent.send(FromService::Password(right).encode());
    });
}

pub fn run() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("the Queqiao service runs as root (systemd starts it: queqiao.service)".into());
    }
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
    let _ = std::fs::create_dir_all(RUNTIME);
    let _ = std::fs::write(
        Path::new(RUNTIME).join("service.pid"),
        std::process::id().to_string(),
    );
    log::line(&format!(
        "service started ({})",
        queqiao_core::banner(env!("CARGO_PKG_NAME"))
    ));
    write_status(None);
    let mut logind = None;
    while logind.is_none() && !STOP.load(Ordering::SeqCst) {
        match Logind::connect() {
            Ok(connected) => logind = Some(connected),
            Err(error) => {
                log::line(&format!("logind: {error} (tried again in 2 s)"));
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
    if let Some(logind) = logind {
        supervise(&logind);
    }
    let _ = std::fs::remove_file(Path::new(RUNTIME).join("service.pid"));
    let _ = std::fs::remove_file(Path::new(RUNTIME).join("status"));
    log::line("service stopped");
    Ok(())
}

fn supervise(logind: &Logind) {
    let (requests, incoming) = mpsc::sync_channel::<(u64, ToService)>(MAX_REQUESTS);
    let failures = Arc::new(Mutex::new(Vec::new()));
    let mut pair: Option<Pair> = None;
    let mut next_id = 1u64;
    let mut failed_starts = 0u32;
    let mut next_start = Instant::now();
    // The session whose agent left by itself (quit, not logged in, or a test run over): not started again until
    // another session is on the screen.
    let mut done_in: Option<String> = None;
    let mut last_error = String::new();
    while !STOP.load(Ordering::SeqCst) {
        let mut stalled = false;
        let handle = |id: u64, request: ToService, pair: &mut Option<Pair>, stalled: &mut bool| {
            let Some(current) = pair.as_mut().filter(|p| p.id == id) else {
                return;
            };
            match request {
                ToService::SendSas => {
                    let reply = FromService::Reply(
                        "Ctrl+Alt+Del has no meaning on Linux: lock that PC or switch user instead"
                            .into(),
                    );
                    *stalled |= !current.to_agent.send(reply.encode());
                }
                ToService::Restart => {
                    if let Some(old) = pair.take() {
                        old.end("the agent asked to restart Queqiao");
                    }
                }
                ToService::LogonIdentity(_) | ToService::ForgetLogon if current.logon => {
                    log::line(
                        "the agent for the sign-in screen tried to change its device: ignored",
                    );
                }
                ToService::LogonIdentity(mut identity) => {
                    let kept_owner = logon::load().map(|kept| kept.owner_sid);
                    if logon::may_hand_over(kept_owner.as_deref(), &current.owner) {
                        // Whose it is: from the session it came from, never from what the agent says.
                        identity.owner_sid = current.owner.clone();
                        identity.owner_account = current.account.clone();
                        match logon::save(&identity) {
                            Ok(()) => log::line(&format!(
                                "session {}: the sign-in screen: device {} kept for it, for {}",
                                current.session.id, identity.device_id, identity.owner_account
                            )),
                            Err(error) => {
                                log::line(&format!("the sign-in screen: not kept: {error}"))
                            }
                        }
                    } else {
                        log::line(&format!(
                            "session {}: the sign-in screen stays with the account that set it up: another's device refused",
                            current.session.id
                        ));
                    }
                }
                ToService::ForgetLogon => {
                    let kept_owner = logon::load().map(|kept| kept.owner_sid);
                    if logon::stored()
                        && !logon::may_hand_over(kept_owner.as_deref(), &current.owner)
                    {
                        log::line(&format!(
                            "session {}: the sign-in screen stays with the account that set it up: another cannot take it back",
                            current.session.id
                        ));
                    } else if logon::stored() {
                        logon::forget();
                        log::line(&format!(
                            "session {}: the sign-in screen: the device was forgotten (the person does not allow it)",
                            current.session.id
                        ));
                    }
                }
                ToService::Update { .. } if current.logon => {
                    log::line("the agent for the sign-in screen asked for an update: ignored");
                }
                ToService::Update {
                    manifest,
                    signature,
                    installer,
                } => {
                    let reply = match super::update::start(&manifest, &signature, &installer) {
                        Ok(version) => {
                            log::line(&format!(
                                "session {}: update to {version} checked (signed by the release key); installing it restarts this service",
                                current.session.id
                            ));
                            format!("installing {version}")
                        }
                        Err(error) => {
                            log::line(&format!(
                                "session {}: update refused: {error}",
                                current.session.id
                            ));
                            format!("refused: {error}")
                        }
                    };
                    *stalled |= !current.to_agent.send(FromService::Reply(reply).encode());
                }
                ToService::CheckPassword(password) => {
                    // Whose password: the person signed in, or the one who set up the sign-in screen; never what
                    // the agent names.
                    let account = if current.logon {
                        logon::load()
                            .map(|kept| kept.owner_account)
                            .unwrap_or_default()
                    } else {
                        current.account.clone()
                    };
                    check_password(
                        account,
                        password,
                        current.to_agent.clone(),
                        failures.clone(),
                    );
                }
                ToService::Notice(notice) if current.logon => {
                    log::line(&format!("the sign-in screen: {notice}"));
                    if let Err(error) = logon::keep_notice(&notice) {
                        log::line(&format!("the notice was not kept: {error}"));
                    }
                }
                ToService::Notice(_) => {}
                // Windows' Super Screen (its display driver): not on Linux.
                ToService::AddMonitor { .. } => {
                    let reply = FromService::Monitor {
                        luid_low: 0,
                        luid_high: 0,
                        target: 0,
                        error: "Super Screen is not on Linux yet".into(),
                    };
                    *stalled |= !current.to_agent.send(reply.encode());
                }
                ToService::RemoveMonitors => {}
            }
        };
        if let Ok((id, request)) = incoming.recv_timeout(Duration::from_millis(500)) {
            handle(id, request, &mut pair, &mut stalled);
            while let Ok((id, request)) = incoming.try_recv() {
                handle(id, request, &mut pair, &mut stalled);
            }
        }
        if stalled && let Some(old) = pair.take() {
            old.end("the agent did not read what the service sent it");
        }
        let active = match logind.active() {
            Ok(active) => active,
            Err(error) => {
                if error != last_error {
                    log::line(&format!("logind: {error}"));
                    last_error = error;
                }
                continue;
            }
        };
        let had_pair = pair.is_some();
        if let Some(current) = pair.as_mut() {
            let mut verdict: Option<(String, bool)> = None;
            if active.as_ref().is_none_or(|s| s.id != current.session.id) {
                verdict = Some(("the session on the screen changed".into(), false));
            } else if let Some(code) = current.agent.exit_code() {
                let finished = code == AGENT_QUIT || code == AGENT_NOT_LOGGED_IN;
                verdict = Some((format!("the agent left with code {code}"), !finished));
                if finished || !Settings::read().restart_agent {
                    done_in = Some(current.session.id.clone());
                }
            } else if current.helper.exit_code().is_some() {
                verdict = Some(("the desktop helper ended".into(), true));
            }
            if let Some((reason, failed)) = verdict {
                let ran = current.started.elapsed();
                if let Some(old) = pair.take() {
                    old.end(&reason);
                }
                if failed {
                    failed_starts = if ran > Duration::from_secs(60) {
                        1
                    } else {
                        failed_starts + 1
                    };
                    let delay = Duration::from_secs(1 << failed_starts.min(6).saturating_sub(1));
                    next_start = Instant::now() + delay.min(Duration::from_secs(60));
                }
            }
        }
        if done_in
            .as_ref()
            .is_some_and(|id| active.as_ref().is_none_or(|s| s.id != *id))
        {
            done_in = None;
        }
        if pair.is_none()
            && let Some(session) = active.as_ref().filter(|s| s.is_desktop() || s.is_sign_in())
            && done_in.as_ref() != Some(&session.id)
            && Instant::now() >= next_start
        {
            let id = next_id;
            next_id += 1;
            match start_pair(id, session, &Settings::read(), &requests) {
                Ok(started) => {
                    pair = Some(started);
                    last_error.clear();
                }
                Err(error) => {
                    if error != last_error {
                        log::line(&format!("session {}: not started: {error}", session.id));
                        last_error = error;
                    }
                    next_start = Instant::now() + Duration::from_secs(2);
                }
            }
        }
        if had_pair != pair.is_some()
            || pair
                .as_ref()
                .is_some_and(|p| p.started.elapsed() < Duration::from_secs(1))
        {
            write_status(pair.as_ref());
        }
    }
    if let Some(old) = pair.take() {
        old.end("the service stops");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn only_a_persons_own_plain_authority_file_is_copied() {
        let dir = std::env::temp_dir().join(format!("queqiao-xauth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let me = std::fs::metadata(&dir).unwrap().uid();
        let own = dir.join("own");
        std::fs::write(&own, b"cookie").unwrap();
        let copy = dir.join("copy");
        let text = |path: &Path| path.to_str().unwrap().to_string();
        // The person's own plain file: copied, root's only.
        private_authority(&text(&own), Some(me), &copy).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"cookie");
        assert_eq!(std::fs::metadata(&copy).unwrap().mode() & 0o777, 0o600);
        // Someone else's file, a link (even to their own), a pipe (without waiting on it), too large: refused.
        assert!(private_authority(&text(&own), Some(me.wrapping_add(1)), &copy).is_err());
        let link = dir.join("link");
        std::os::unix::fs::symlink(&own, &link).unwrap();
        assert!(private_authority(&text(&link), Some(me), &copy).is_err());
        let pipe = dir.join("pipe");
        let name = std::ffi::CString::new(text(&pipe)).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(private_authority(&text(&pipe), Some(me), &copy).is_err());
        let large = dir.join("large");
        std::fs::write(&large, vec![0u8; MAX_AUTHORITY as usize + 1]).unwrap();
        assert!(private_authority(&text(&large), Some(me), &copy).is_err());
        // The X server's own file (from its command line, no owner asked): copied whoever owns it.
        private_authority(&text(&own), None, &copy).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
