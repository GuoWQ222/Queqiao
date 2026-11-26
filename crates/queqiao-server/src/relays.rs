//! The relays an account server hands out (the user, 2026-11-26: spread the relayed traffic over the servers): its
//! own TURN part ("main") and the relay servers that joined it (`queqiao-server relay`, with a key from `add-relay`).
//!
//! Each session gets the relay nearest to both of its ends that has room: what each device measured (`RelayRtts`)
//! summed, a relay over half full counted further away, and a nearly full one only when all are; the next ones follow
//! as backups. Both ends of one session get the same order (the second to ask gets what the first got), so their
//! traffic goes through one relay. A relay that has not reported for a while is not handed out.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::UnboundedSender;

use crate::link::ToRelay;
use crate::turn::{CREDENTIAL_SECONDS, TurnConfig};

/// The account server's own relay, by name.
pub const MAIN: &str = "main";
/// A relay a device did not measure counts as this far (one it could not reach goes after those it could).
const UNKNOWN_RTT_MS: f64 = 300.0;
/// A relay at full load counts as this much further, from half load on (linearly).
const LOAD_PENALTY_MS: f64 = 100.0;
/// From this share of its capacity a relay is full: handed out only when every relay is.
const FULL: f64 = 0.9;
/// What a session given to a relay is expected to add, until the relay's reports show it.
const RESERVE_MBPS: f64 = 10.0;
const RESERVE_FOR: Duration = Duration::from_secs(20);
/// A relay without a report for this long is not handed out (they report every 2 s).
const STALE: Duration = Duration::from_secs(10);
/// How long the order given to one end of a session is kept for the other end.
const CHOICE_FOR: Duration = Duration::from_secs(60);
/// Relays after the first, as backups.
const BACKUPS: usize = 2;
/// The account server's own relay's load is sampled this often.
const LOCAL_STEP: Duration = Duration::from_secs(2);

/// One relay as the account server knows it.
struct Relay {
    /// host:port (UDP) where clients reach its STUN and TURN; empty until its first report.
    address: String,
    key: [u8; 32],
    capacity_mbps: f64,
    rate_mbps: f64,
    allocations: u32,
    seen: Instant,
    /// Sessions given to it lately (not in its reports yet).
    reserved: Vec<Instant>,
    /// Its link (revocations go there); none for the account server's own relay.
    link: Option<(u64, UnboundedSender<String>)>,
}

impl Relay {
    fn load(&self, now: Instant) -> f64 {
        if self.capacity_mbps <= 0.0 {
            return 0.0;
        }
        let reserved = self
            .reserved
            .iter()
            .filter(|at| now.saturating_duration_since(**at) < RESERVE_FOR)
            .count() as f64;
        (self.rate_mbps + reserved * RESERVE_MBPS) / self.capacity_mbps
    }

    fn available(&self, now: Instant) -> bool {
        !self.address.is_empty() && now.saturating_duration_since(self.seen) < STALE
    }
}

/// A relay for a session: where it is and the key its credentials are made with.
pub struct Assigned {
    #[cfg(test)]
    pub name: String,
    pub address: String,
    pub key: [u8; 32],
}

/// A relay to rank: the two ends' round trips to it summed, and its load (share of its capacity).
#[derive(Clone, Debug)]
pub struct Candidate {
    pub name: String,
    pub rtt_ms: f64,
    pub load: f64,
}

fn score(candidate: &Candidate) -> f64 {
    let over = ((candidate.load - 0.5) * 2.0).clamp(0.0, 1.0);
    candidate.rtt_ms + LOAD_PENALTY_MS * over
}

/// The relays in the order to use them: not full before full, then the nearest (load counted in), then by name.
/// Equally good relays: the account server's own first (where every session went before there were others), then by
/// name.
pub fn rank(mut candidates: Vec<Candidate>) -> Vec<String> {
    candidates.sort_by(|a, b| {
        (a.load >= FULL)
            .cmp(&(b.load >= FULL))
            .then(score(a).total_cmp(&score(b)))
            .then((a.name != MAIN).cmp(&(b.name != MAIN)))
            .then(a.name.cmp(&b.name))
    });
    candidates.into_iter().map(|c| c.name).collect()
}

/// The order given to a session (its two device ids, in order), and when.
type Choices = HashMap<(String, String), (Vec<String>, Instant)>;

pub struct Relays {
    local: Option<Arc<TurnConfig>>,
    relays: Mutex<HashMap<String, Relay>>,
    /// The order given to a session's devices (the pair of device ids, in order), for the other one.
    choices: Mutex<Choices>,
    /// Revocations of the last CREDENTIAL_SECONDS, for relays that join later: (device, program) -> Unix seconds.
    revoked: Mutex<HashMap<(String, String), u64>>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Relays {
    /// The account server's own relay (if its TURN part is on), with `capacity_mbps` (0: unknown).
    pub fn new(local: Option<Arc<TurnConfig>>, capacity_mbps: f64) -> Arc<Relays> {
        let mut relays = HashMap::new();
        if let Some(config) = &local {
            relays.insert(
                MAIN.to_string(),
                Relay {
                    address: config.address(),
                    key: config.secret,
                    capacity_mbps,
                    rate_mbps: 0.0,
                    allocations: 0,
                    seen: Instant::now(),
                    reserved: Vec::new(),
                    link: None,
                },
            );
        }
        Arc::new(Relays {
            local,
            relays: Mutex::new(relays),
            choices: Mutex::new(HashMap::new()),
            revoked: Mutex::new(HashMap::new()),
        })
    }

    /// Samples the account server's own relay's load every LOCAL_STEP (it has no link to report on).
    pub async fn watch_local(self: Arc<Relays>) {
        let Some(config) = self.local.clone() else {
            return;
        };
        let mut last = (config.relayed.load(Ordering::Relaxed), Instant::now());
        loop {
            tokio::time::sleep(LOCAL_STEP).await;
            let now = (config.relayed.load(Ordering::Relaxed), Instant::now());
            let seconds = now
                .1
                .saturating_duration_since(last.1)
                .as_secs_f64()
                .max(0.001);
            let rate_mbps = now.0.saturating_sub(last.0) as f64 * 8.0 / seconds / 1e6;
            last = now;
            if let Ok(mut relays) = self.relays.lock()
                && let Some(main) = relays.get_mut(MAIN)
            {
                main.rate_mbps = rate_mbps;
                main.allocations = config.allocations.load(Ordering::Relaxed) as u32;
                main.seen = now.1;
            }
        }
    }

    /// The STUN addresses of the relays handed out now (`Welcome`), for the devices to measure.
    pub fn addresses(&self) -> Vec<String> {
        let now = Instant::now();
        let Ok(relays) = self.relays.lock() else {
            return Vec::new();
        };
        let mut addresses: Vec<String> = relays
            .values()
            .filter(|relay| relay.available(now))
            .map(|relay| relay.address.clone())
            .collect();
        addresses.sort();
        addresses.dedup();
        addresses
    }

    /// Whether `address` is a relay's (what a device may report a round trip for).
    pub fn is_relay(&self, address: &str) -> bool {
        self.relays
            .lock()
            .is_ok_and(|relays| relays.values().any(|relay| relay.address == address))
    }

    /// The relays for a session of `me` with `peer` (device ids), best first: `mine` and `theirs` are the round trips
    /// the two measured (by relay address). The other end of the same session, asking within CHOICE_FOR, gets the
    /// same order. `prefer` (tests: an IP address) puts the relay there first.
    pub fn assign(
        &self,
        me: &str,
        peer: Option<&str>,
        mine: &HashMap<String, u32>,
        theirs: Option<&HashMap<String, u32>>,
        prefer: Option<&str>,
    ) -> Vec<Assigned> {
        let mut assigned = self.ranked(me, peer, mine, theirs);
        if let Some(ip) = prefer
            && let Some(at) = assigned.iter().position(|relay| {
                relay
                    .address
                    .parse::<std::net::SocketAddr>()
                    .is_ok_and(|address| address.ip().to_string() == ip)
            })
        {
            let wanted = assigned.remove(at);
            assigned.insert(0, wanted);
        }
        assigned.truncate(1 + BACKUPS);
        assigned
    }

    fn ranked(
        &self,
        me: &str,
        peer: Option<&str>,
        mine: &HashMap<String, u32>,
        theirs: Option<&HashMap<String, u32>>,
    ) -> Vec<Assigned> {
        let now = Instant::now();
        let pair = peer.map(|peer| {
            if me <= peer {
                (me.to_string(), peer.to_string())
            } else {
                (peer.to_string(), me.to_string())
            }
        });
        let Ok(mut relays) = self.relays.lock() else {
            return Vec::new();
        };
        let kept = pair.as_ref().and_then(|pair| {
            let mut choices = self.choices.lock().ok()?;
            choices.retain(|_, (_, at)| now.saturating_duration_since(*at) < CHOICE_FOR);
            choices.get(pair).map(|(order, _)| order.clone())
        });
        let order = match kept {
            Some(order) => order,
            None => {
                let rtt = |rtts: Option<&HashMap<String, u32>>, address: &str| {
                    rtts.and_then(|rtts| rtts.get(address))
                        .map_or(UNKNOWN_RTT_MS, |ms| f64::from(*ms))
                };
                let candidates = relays
                    .iter()
                    .filter(|(_, relay)| relay.available(now))
                    .map(|(name, relay)| Candidate {
                        name: name.clone(),
                        rtt_ms: rtt(Some(mine), &relay.address) + rtt(theirs, &relay.address),
                        load: relay.load(now),
                    })
                    .collect();
                let mut order = rank(candidates);
                // Neither end measured any relay (programs from before several relays: they also use only the first
                // relay given, never a backup): the account server's own, as before, whatever its load. A relay
                // they cannot reach would end their relayed sessions.
                let measured = !mine.is_empty() || theirs.is_some_and(|theirs| !theirs.is_empty());
                if !measured && let Some(at) = order.iter().position(|name| name == MAIN) {
                    let main = order.remove(at);
                    order.insert(0, main);
                }
                // The session is expected on the first, until its reports show it.
                if let Some(first) = order.first().and_then(|name| relays.get_mut(name)) {
                    first
                        .reserved
                        .retain(|at| now.saturating_duration_since(*at) < RESERVE_FOR);
                    first.reserved.push(now);
                }
                if let (Some(pair), Ok(mut choices)) = (pair, self.choices.lock()) {
                    choices.insert(pair, (order.clone(), now));
                }
                order
            }
        };
        order
            .iter()
            .filter_map(|name| {
                let relay = relays.get(name).filter(|relay| relay.available(now))?;
                Some(Assigned {
                    #[cfg(test)]
                    name: name.clone(),
                    address: relay.address.clone(),
                    key: relay.key,
                })
            })
            .collect()
    }

    /// A relay server proved it has `name`'s key: its link (`id`, revocations go to `sender`) replaces any earlier one.
    /// It is handed out once it reports where it is.
    pub fn joined(&self, name: &str, key: [u8; 32], id: u64, sender: UnboundedSender<String>) {
        // What was revoked lately goes to it first.
        let now = unix_now();
        if let Ok(mut revoked) = self.revoked.lock() {
            revoked.retain(|_, at| *at + CREDENTIAL_SECONDS >= now);
            for ((device, program), at) in revoked.iter() {
                let message = ToRelay::Revoke {
                    device: device.clone(),
                    programs: vec![program.clone()],
                    at: *at,
                };
                if let Ok(text) = serde_json::to_string(&message) {
                    let _ = sender.send(text);
                }
            }
        }
        if let Ok(mut relays) = self.relays.lock() {
            relays.insert(
                name.to_string(),
                Relay {
                    address: String::new(),
                    key,
                    capacity_mbps: 0.0,
                    rate_mbps: 0.0,
                    allocations: 0,
                    seen: Instant::now(),
                    reserved: Vec::new(),
                    link: Some((id, sender)),
                },
            );
        }
    }

    /// A relay's report on link `id` (one replaced by a newer link is ignored).
    pub fn report(
        &self,
        name: &str,
        id: u64,
        address: String,
        capacity_mbps: f64,
        rate_mbps: f64,
        allocations: u32,
    ) {
        if let Ok(mut relays) = self.relays.lock()
            && let Some(relay) = relays.get_mut(name)
            && relay.link.as_ref().is_some_and(|(link, _)| *link == id)
        {
            relay.address = address;
            relay.capacity_mbps = capacity_mbps;
            relay.rate_mbps = rate_mbps;
            relay.allocations = allocations;
            relay.seen = Instant::now();
        }
    }

    /// Link `id` of relay `name` ended: it is no longer handed out (unless a newer link took its place).
    pub fn left(&self, name: &str, id: u64) {
        if let Ok(mut relays) = self.relays.lock()
            && relays
                .get(name)
                .is_some_and(|relay| relay.link.as_ref().is_some_and(|(link, _)| *link == id))
        {
            relays.remove(name);
        }
    }

    /// The relay credentials `programs` of `device` were given until now stop working on every relay.
    pub fn revoke(&self, device: &str, programs: &[&str]) {
        let at = unix_now();
        if let Some(local) = &self.local {
            local.revoke_at(device, programs, at);
        }
        if let Ok(mut revoked) = self.revoked.lock() {
            for program in programs {
                revoked.insert((device.to_string(), (*program).to_string()), at);
            }
        }
        let message = ToRelay::Revoke {
            device: device.to_string(),
            programs: programs.iter().map(|p| (*p).to_string()).collect(),
            at,
        };
        let Ok(text) = serde_json::to_string(&message) else {
            return;
        };
        if let Ok(relays) = self.relays.lock() {
            for relay in relays.values() {
                if let Some((_, sender)) = &relay.link {
                    let _ = sender.send(text.clone());
                }
            }
        }
    }

    /// One line per relay, for the server's log.
    pub fn summary(&self) -> String {
        let now = Instant::now();
        let Ok(relays) = self.relays.lock() else {
            return String::new();
        };
        let mut lines: Vec<String> = relays
            .iter()
            .map(|(name, relay)| {
                format!(
                    "{name} {} {:.1}/{:.0} Mbit/s, {} allocations{}",
                    relay.address,
                    relay.rate_mbps,
                    relay.capacity_mbps,
                    relay.allocations,
                    if relay.available(now) {
                        ""
                    } else {
                        " (not handed out)"
                    }
                )
            })
            .collect();
        lines.sort();
        lines.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: &str, rtt_ms: f64, load: f64) -> Candidate {
        Candidate {
            name: name.into(),
            rtt_ms,
            load,
        }
    }

    #[test]
    fn the_nearest_relay_with_room_comes_first() {
        // Nearest first while there is room.
        let order = rank(vec![
            candidate("beijing", 80.0, 0.1),
            candidate("shanghai", 40.0, 0.2),
            candidate("guangzhou", 60.0, 0.0),
        ]);
        assert_eq!(order, ["shanghai", "guangzhou", "beijing"]);
        // Shanghai at 80 % counts 60 ms further: Guangzhou (20 ms further) goes first.
        let order = rank(vec![
            candidate("shanghai", 40.0, 0.8),
            candidate("guangzhou", 60.0, 0.0),
        ]);
        assert_eq!(order, ["guangzhou", "shanghai"]);
        // A full relay comes after every other, however near.
        let order = rank(vec![
            candidate("shanghai", 10.0, 0.95),
            candidate("chengdu", 290.0, 0.1),
        ]);
        assert_eq!(order, ["chengdu", "shanghai"]);
        // All full: still the nearest first; equal ones by name.
        let order = rank(vec![
            candidate("b", 50.0, 0.95),
            candidate("a", 50.0, 0.95),
            candidate("c", 20.0, 0.99),
        ]);
        assert_eq!(order, ["c", "a", "b"]);
    }

    fn relays_with(names: &[(&str, &str)]) -> Arc<Relays> {
        let relays = Relays::new(None, 0.0);
        for (index, (name, address)) in names.iter().enumerate() {
            let (sender, _) = tokio::sync::mpsc::unbounded_channel();
            relays.joined(name, [index as u8; 32], index as u64, sender);
            relays.report(name, index as u64, (*address).to_string(), 100.0, 0.0, 0);
        }
        relays
    }

    #[test]
    fn both_ends_of_a_session_get_the_same_relays_nearest_to_both() {
        let relays = relays_with(&[
            ("north", "198.51.100.1:3478"),
            ("south", "198.51.100.2:3478"),
        ]);
        let viewer: HashMap<String, u32> = [
            ("198.51.100.1:3478".into(), 10),
            ("198.51.100.2:3478".into(), 50),
        ]
        .into();
        let agent: HashMap<String, u32> = [
            ("198.51.100.1:3478".into(), 90),
            ("198.51.100.2:3478".into(), 20),
        ]
        .into();
        // North: 10 + 90; south: 50 + 20 - south for both.
        let first = relays.assign("viewer-pc", Some("agent-pc"), &viewer, Some(&agent), None);
        let names: Vec<&str> = first.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["south", "north"]);
        assert_eq!(first[0].key, [1; 32]);
        // The agent asks with only its own measurements (the viewer's are not given): the same order all the same.
        let second = relays.assign("agent-pc", Some("viewer-pc"), &agent, None, None);
        let names: Vec<&str> = second.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["south", "north"]);
        // A relay whose link ended is not handed out; nor one that has not said where it is.
        relays.left("south", 1);
        let (sender, _) = tokio::sync::mpsc::unbounded_channel();
        relays.joined("east", [7; 32], 9, sender);
        let third = relays.assign("viewer-pc", Some("agent-pc"), &viewer, Some(&agent), None);
        let names: Vec<&str> = third.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["north"]);
        assert_eq!(relays.addresses(), ["198.51.100.1:3478"]);
        // A report on an old link changes nothing.
        relays.report("east", 8, "198.51.100.3:3478".into(), 100.0, 0.0, 0);
        assert_eq!(relays.addresses(), ["198.51.100.1:3478"]);
    }

    #[test]
    fn programs_that_measure_nothing_stay_on_the_account_servers_relay() {
        let local = Arc::new(TurnConfig::new(
            "0.0.0.0:3478".parse().unwrap(),
            "198.51.100.9".parse().unwrap(),
            (47200, 47399),
            false,
            [9; 32],
            0.0,
        ));
        let relays = Relays::new(Some(local), 100.0);
        for (index, name) in ["chengdu", "guangzhou"].iter().enumerate() {
            let (sender, _) = tokio::sync::mpsc::unbounded_channel();
            relays.joined(name, [index as u8; 32], index as u64, sender);
            relays.report(
                name,
                index as u64,
                format!("198.51.100.{}:3478", index + 1),
                100.0,
                0.0,
                0,
            );
        }
        let names =
            |assigned: Vec<Assigned>| assigned.into_iter().map(|a| a.name).collect::<Vec<_>>();
        let none = HashMap::new();
        // Nothing measured (programs from before): the account server's own first ("chengdu" sorts before "main").
        assert_eq!(
            names(relays.assign("old-v", Some("old-a"), &none, None, None))[0],
            MAIN
        );
        // Even when it is busy: they use only the first relay given, and may not reach the others.
        relays
            .relays
            .lock()
            .unwrap()
            .get_mut(MAIN)
            .unwrap()
            .rate_mbps = 80.0;
        assert_eq!(
            names(relays.assign("old-v2", Some("old-a2"), &none, None, None))[0],
            MAIN
        );
        relays
            .relays
            .lock()
            .unwrap()
            .get_mut(MAIN)
            .unwrap()
            .rate_mbps = 0.0;
        // Measured: the nearest, here Chengdu.
        let near: HashMap<String, u32> = [
            ("198.51.100.9:3478".to_string(), 60),
            ("198.51.100.1:3478".to_string(), 20),
            ("198.51.100.2:3478".to_string(), 40),
        ]
        .into();
        assert_eq!(
            names(relays.assign("new-v", Some("new-a"), &near, Some(&near), None))[0],
            "chengdu"
        );
        // Equally near: the account server's own wins the tie.
        let even: HashMap<String, u32> = [
            ("198.51.100.9:3478".to_string(), 30),
            ("198.51.100.1:3478".to_string(), 30),
            ("198.51.100.2:3478".to_string(), 30),
        ]
        .into();
        assert_eq!(
            names(relays.assign("even-v", Some("even-a"), &even, Some(&even), None))[0],
            MAIN
        );
    }

    #[test]
    fn a_test_may_ask_for_one_relay_first() {
        let relays = relays_with(&[
            ("north", "198.51.100.1:3478"),
            ("south", "198.51.100.2:3478"),
        ]);
        let none = HashMap::new();
        let names =
            |assigned: Vec<Assigned>| assigned.into_iter().map(|a| a.name).collect::<Vec<_>>();
        assert_eq!(
            names(relays.assign("v", Some("a"), &none, None, None)),
            ["north", "south"]
        );
        assert_eq!(
            names(relays.assign("v", Some("a"), &none, None, Some("198.51.100.2"))),
            ["south", "north"]
        );
        // An address no relay has changes nothing.
        assert_eq!(
            names(relays.assign("v", Some("a"), &none, None, Some("203.0.113.1"))),
            ["north", "south"]
        );
        // One past the backups is asked for: it comes first all the same (it was cut off before, 2026-11-26).
        let four = relays_with(&[
            ("a", "198.51.100.1:3478"),
            ("b", "198.51.100.2:3478"),
            ("c", "198.51.100.3:3478"),
            ("d", "198.51.100.4:3478"),
        ]);
        assert_eq!(
            names(four.assign("v", Some("a"), &none, None, None)),
            ["a", "b", "c"]
        );
        assert_eq!(
            names(four.assign("v", Some("a"), &none, None, Some("198.51.100.4"))),
            ["d", "a", "b"]
        );
    }

    #[test]
    fn sessions_given_to_a_relay_count_until_it_reports_them() {
        let relays = relays_with(&[("a", "198.51.100.1:3478"), ("b", "198.51.100.2:3478")]);
        let none = HashMap::new();
        // Equal distance: the sessions go to "a" until its expected load (10 Mbit/s each of 100) passes half, then to
        // "b" until its does.
        let mut firsts = Vec::new();
        for session in 0..10 {
            let assigned = relays.assign(
                &format!("v{session}"),
                Some(&format!("a{session}")),
                &none,
                None,
                None,
            );
            firsts.push(assigned[0].name.clone());
        }
        assert_eq!(firsts[..6], ["a"; 6]);
        assert_eq!(firsts[6..], ["b"; 4]);
    }

    #[test]
    fn revocations_reach_relays_that_join_later() {
        let relays = Relays::new(None, 0.0);
        relays.revoke("dev1", &["agent", "viewer"]);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        relays.joined("late", [3; 32], 1, sender);
        let mut got = Vec::new();
        while let Ok(text) = receiver.try_recv() {
            got.push(text);
        }
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.iter().all(|text| text.contains("\"device\":\"dev1\"")));
    }
}
