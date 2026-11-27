//! A session kept through a network change (Wi-Fi to a phone's mobile data, a new address, a lost uplink), as UU Remote
//! keeps one: ICE starts again inside the session (the DTLS session goes on, so nothing is set up again).
//!
//! - The side whose network changed ([`PeerEvent::NetworkChanged`]) waits for its server connection (made again by the
//!   account in the background), gathers on the new network ([`Peer::regather`]: new sockets, relay allocation, NAT
//!   classification) and, the agent, asks the viewer to start again ("restart-request").
//! - The viewer always makes the restart offer ([`Peer::restart_offer`], "restart-offer"), so the two sides never offer
//!   at once; the agent answers ("restart-answer"). The viewer also starts one when ICE has been without the other side
//!   for `QUIET` with no request (the other side's network changed and its request has not come, or a relay stopped).
//! - A restart that brought nothing back in `ANSWER_WAIT` + `ICE_WAIT` is tried again, gathering again first; the
//!   network thread ends the session after [`crate::RESTART_LIMIT`] without the other side.
//! - The selected path went quiet while ICE still counts it alive ([`PeerEvent::Stalled`], after
//!   [`crate::STALL_AFTER`]: a direct path that died, which ICE gives up only after about 20 s): the viewer starts a
//!   restart at once, the agent asks for one ("restart-request" with the body "stalled", which a viewer already
//!   restarting leaves out). The restart checks every pair afresh, and the first that works (a relay) carries on.
//!
//! The session loops hand it the events it uses ([`Restarter::on_event`]) and call [`Restarter::step`] on each pass.

use std::time::{Duration, Instant};

use crate::peer::{Peer, PeerEvent, Role};
use crate::signal::Signaling;

/// How long the viewer waits, ICE without the other side and no request from it, before it starts again by itself.
const QUIET: Duration = Duration::from_secs(3);
/// How long the viewer waits for the answer to its restart offer.
const ANSWER_WAIT: Duration = Duration::from_secs(8);
/// How long it waits for ICE to have the other side again once the answer is in.
const ICE_WAIT: Duration = Duration::from_secs(10);
/// How long gathering on the new network may take before it is tried again (it failed: no route yet, say).
const GATHER_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    /// Gathering on the new network (then the agent asks, the viewer offers).
    Gathering(Instant),
    /// The viewer asked for its offer (it comes as `RestartSdp`).
    Offering,
    /// The viewer sent its offer then.
    AwaitAnswer(Instant),
    /// The answer is in: ICE checks the new pairs.
    AwaitIce(Instant),
}

/// One session's ICE restarts.
pub struct Restarter {
    role: Role,
    stage: Stage,
    /// This side's network changed and it has not gathered on the new one yet.
    changed: bool,
    /// ICE is without the other side since then.
    disconnected_since: Option<Instant>,
    /// The agent: a restart offer that came while it was gathering (answered once it is done).
    waiting_offer: Option<String>,
    /// The viewer: the agent asked for a restart (its network changed) and none has started since.
    requested: bool,
    /// The selected path went quiet (this side saw it, or the agent said so): a restart if none is under way.
    stalled: bool,
    /// The server connection was up at the last pass (it is made again in the background when lost).
    online: bool,
    /// How many restarts brought the other side back.
    pub restarts: u32,
}

impl Restarter {
    pub fn new(role: Role) -> Restarter {
        Restarter {
            role,
            stage: Stage::Idle,
            changed: false,
            disconnected_since: None,
            waiting_offer: None,
            requested: false,
            stalled: false,
            online: true,
            restarts: 0,
        }
    }

    /// Whether a restart is under way or wanted (the session window can say "reconnecting").
    pub fn busy(&self) -> bool {
        self.stage != Stage::Idle || self.changed || self.disconnected_since.is_some()
    }

    /// One of the peer's events: true when it was one of the restart's (`NetworkChanged`, `Disconnected`,
    /// `Reconnected`, `Regathered`, `RestartSdp`), which the session loop then leaves to it.
    pub fn on_event(
        &mut self,
        event: &PeerEvent,
        peer: &Peer,
        signal: &mut dyn Signaling,
        lines: &mut Vec<String>,
    ) -> bool {
        match event {
            PeerEvent::NetworkChanged => {
                self.changed = true;
                lines.push(
                    "network changed: gathering again once the server connection is back".into(),
                );
            }
            PeerEvent::Disconnected => {
                self.disconnected_since.get_or_insert_with(Instant::now);
            }
            PeerEvent::Reconnected => {
                if let Some(since) = self.disconnected_since.take() {
                    lines.push(format!(
                        "the other side is back after {} ms",
                        since.elapsed().as_millis()
                    ));
                }
                // Only once the answer is in: ICE says "connected" for a moment as the offer is made, before the other
                // side has answered (measured 2026-11-27: 22 ms after the offer, the answer then dropped).
                if let Stage::AwaitIce(_) = self.stage {
                    self.restarts += 1;
                    self.stage = Stage::Idle;
                }
            }
            PeerEvent::Regathered => {
                self.changed = false;
                match self.role {
                    Role::Viewer => {
                        peer.restart_offer();
                        self.stage = Stage::Offering;
                    }
                    Role::Agent => {
                        self.stage = Stage::Idle;
                        match self.waiting_offer.take() {
                            Some(offer) => peer.accept_restart(offer),
                            None => {
                                let _ = signal.send("restart-request", "");
                                lines.push("restart asked of the viewer".into());
                            }
                        }
                    }
                }
            }
            PeerEvent::Stalled => match self.role {
                Role::Viewer => self.stalled = true,
                Role::Agent => {
                    // The viewer offers; asked unless a restart is under way here already.
                    if self.stage == Stage::Idle && !self.changed {
                        let _ = signal.send("restart-request", "stalled");
                        lines.push(
                            "the path carries nothing from the viewer: restart asked of it".into(),
                        );
                    }
                }
            },
            PeerEvent::RestartSdp(sdp) => match self.role {
                Role::Viewer => {
                    let _ = signal.send("restart-offer", sdp);
                    self.stage = Stage::AwaitAnswer(Instant::now());
                }
                Role::Agent => {
                    let _ = signal.send("restart-answer", sdp);
                }
            },
            _ => return false,
        }
        true
    }

    /// One pass of the session loop: gathers once the server connection is back, starts and follows a restart.
    pub fn step(&mut self, peer: &Peer, signal: &mut dyn Signaling, lines: &mut Vec<String>) {
        let now = Instant::now();
        // Gathering on the new network needs the server connection (its address, the relays' credentials).
        if let Stage::Gathering(began) = self.stage
            && now >= began + GATHER_WAIT
        {
            lines.push("gathering again brought nothing: again".into());
            self.stage = Stage::Idle;
        }
        let online = signal.online();
        if online != self.online {
            self.online = online;
            lines.push(if online {
                "the server connection is back".into()
            } else {
                "the server connection is lost: it is being made again".into()
            });
        }
        let gathering = matches!(self.stage, Stage::Gathering(_));
        if self.changed && !gathering && online {
            match (signal.local_ip(), signal.ice_servers()) {
                (Ok(ip), Ok(servers)) => {
                    peer.regather(ip, servers);
                    self.stage = Stage::Gathering(now);
                }
                (Err(error), _) | (_, Err(error)) => {
                    lines.push(format!("gathering again waits: {error}"));
                }
            }
        }
        match self.role {
            Role::Agent => {
                if let Some(offer) = signal.take("restart-offer") {
                    if self.changed || matches!(self.stage, Stage::Gathering(_)) {
                        self.waiting_offer = Some(offer);
                    } else {
                        peer.accept_restart(offer);
                    }
                }
            }
            Role::Viewer => {
                // Every restart message for the viewer is taken on each pass (one left would hold up the queue it
                // waits in): a request is kept until a restart can start, an answer that is not awaited is left out.
                while let Some(body) = signal.take("restart-request") {
                    // "stalled": the agent's path went quiet (a restart under way already covers it); otherwise its
                    // network changed (it gathered anew: a restart even after one under way).
                    if body == "stalled" {
                        self.stalled = true;
                    } else {
                        self.requested = true;
                    }
                }
                // A restart under way covers a quiet path (it checks every pair afresh).
                if self.stage != Stage::Idle {
                    self.stalled = false;
                }
                let answer = signal.take("restart-answer");
                let quiet = self
                    .disconnected_since
                    .is_some_and(|since| now >= since + QUIET);
                match self.stage {
                    // A request during the ICE checks of a restart: the other side gathered again since its answer.
                    Stage::Idle | Stage::AwaitIce(_)
                        if !self.changed
                            && (self.requested
                                || (self.stage == Stage::Idle && (quiet || self.stalled))) =>
                    {
                        lines.push(if self.requested {
                            "the other side's network changed: restarting ICE".into()
                        } else if self.stalled {
                            "the path carries nothing: restarting ICE".into()
                        } else {
                            "the other side is gone: restarting ICE".into()
                        });
                        self.requested = false;
                        self.stalled = false;
                        peer.restart_offer();
                        self.stage = Stage::Offering;
                    }
                    Stage::AwaitAnswer(sent) => {
                        if let Some(answer) = answer {
                            peer.restart_answer(answer);
                            self.stage = Stage::AwaitIce(now);
                        } else if now >= sent + ANSWER_WAIT {
                            lines.push("no answer to the restart offer: again".into());
                            self.retry();
                        }
                        return;
                    }
                    Stage::AwaitIce(answered) if now >= answered + ICE_WAIT => {
                        if self.disconnected_since.is_some() {
                            lines.push(
                                "the restart did not bring the other side back: again".into(),
                            );
                            self.retry();
                        } else {
                            self.stage = Stage::Idle;
                        }
                    }
                    _ => {}
                }
                if answer.is_some() {
                    lines.push("a restart answer to no offer of this side's: left out".into());
                }
            }
        }
    }

    /// Again from the start, gathering first this time (the relay allocation, say, may be gone).
    fn retry(&mut self) {
        self.stage = Stage::Idle;
        self.changed = true;
    }
}
