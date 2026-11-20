//! Two str0m peers that have only relayed candidates, with the packets carried in memory (no sockets, no
//! TURN): both must check and connect. This separates str0m's ICE from the TURN plumbing.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer};
use str0m::media::{Direction, MediaKind};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

struct Side {
    name: &'static str,
    rtc: Rtc,
    relayed: SocketAddr,
    connected: bool,
    sent: Vec<(SocketAddr, SocketAddr)>,
    states: Vec<IceConnectionState>,
}

/// Polls one side until it only has a timeout left; returns the packets it sends and its next timeout.
fn drain(side: &mut Side) -> (Vec<(SocketAddr, SocketAddr, Vec<u8>)>, Instant) {
    let mut out = Vec::new();
    loop {
        match side.rtc.poll_output().unwrap() {
            Output::Timeout(at) => return (out, at),
            Output::Transmit(t) => {
                side.sent.push((t.source, t.destination));
                out.push((t.source, t.destination, t.contents.to_vec()));
            }
            Output::Event(Event::Connected) => side.connected = true,
            Output::Event(Event::IceConnectionStateChange(state)) => side.states.push(state),
            Output::Event(_) => {}
        }
    }
}

#[test]
fn relayed_only_candidates_connect() {
    str0m::crypto::from_feature_flags().install_process_default();
    let start = Instant::now();
    let relay_a: SocketAddr = "198.51.100.1:47200".parse().unwrap();
    let relay_b: SocketAddr = "198.51.100.1:47201".parse().unwrap();
    let mut a = Side {
        name: "viewer",
        rtc: RtcConfig::new()
            .clear_codecs()
            .enable_h264(true)
            .build(start),
        relayed: relay_a,
        connected: false,
        sent: Vec::new(),
        states: Vec::new(),
    };
    let mut b = Side {
        name: "agent",
        rtc: RtcConfig::new()
            .clear_codecs()
            .enable_h264(true)
            .build(start),
        relayed: relay_b,
        connected: false,
        sent: Vec::new(),
        states: Vec::new(),
    };
    a.rtc.add_local_candidate(
        Candidate::relayed(relay_a, "10.0.0.1:5000".parse().unwrap(), "udp").unwrap(),
    );
    b.rtc.add_local_candidate(
        Candidate::relayed(relay_b, "10.0.0.2:5000".parse().unwrap(), "udp").unwrap(),
    );

    let mut api = a.rtc.sdp_api();
    api.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
    api.add_channel("control".to_string());
    let (offer, pending) = api.apply().unwrap();
    let offer_text = offer.to_sdp_string();
    let answer = b
        .rtc
        .sdp_api()
        .accept_offer(SdpOffer::from_sdp_string(&offer_text).unwrap())
        .unwrap();
    let answer_text = answer.to_sdp_string();
    a.rtc
        .sdp_api()
        .accept_answer(pending, SdpAnswer::from_sdp_string(&answer_text).unwrap())
        .unwrap();

    let mut now = start;
    let mut in_flight: Vec<(SocketAddr, SocketAddr, Vec<u8>)> = Vec::new();
    while now < start + Duration::from_secs(10) && !(a.connected && b.connected) {
        let (from_a, next_a) = drain(&mut a);
        let (from_b, next_b) = drain(&mut b);
        in_flight.extend(from_a);
        in_flight.extend(from_b);
        for (source, destination, bytes) in std::mem::take(&mut in_flight) {
            let side = if destination == a.relayed {
                &mut a
            } else if destination == b.relayed {
                &mut b
            } else {
                continue;
            };
            let input = Input::Receive(
                now,
                Receive {
                    proto: Protocol::Udp,
                    source,
                    destination,
                    contents: bytes.as_slice().try_into().unwrap(),
                },
            );
            side.rtc.handle_input(input).unwrap();
        }
        now = next_a.min(next_b).max(now + Duration::from_millis(1));
        a.rtc.handle_input(Input::Timeout(now)).unwrap();
        b.rtc.handle_input(Input::Timeout(now)).unwrap();
    }
    for side in [&a, &b] {
        let sources: std::collections::BTreeSet<_> = side
            .sent
            .iter()
            .map(|(s, d)| format!("{s} -> {d}"))
            .collect();
        println!(
            "{}: connected {}, ICE states {:?}, {} packets sent: {:?}",
            side.name,
            side.connected,
            side.states,
            side.sent.len(),
            sources
        );
    }
    let offered = offer_text
        .lines()
        .filter(|l| l.contains("candidate"))
        .collect::<Vec<_>>();
    let answered = answer_text
        .lines()
        .filter(|l| l.contains("candidate"))
        .collect::<Vec<_>>();
    println!("offer candidates: {offered:?}\nanswer candidates: {answered:?}");
    assert!(
        a.connected && b.connected,
        "relayed-only peers did not connect"
    );
}

/// As the real network does it: each side runs only on its own timers, and a packet reaches a side only
/// after that side has sent to its sender (a TURN allocation drops packets from addresses it has not
/// given permission to).
#[test]
fn relayed_only_candidates_connect_on_own_timers_with_permissions() {
    str0m::crypto::from_feature_flags().install_process_default();
    let start = Instant::now();
    let relay_a: SocketAddr = "198.51.100.1:47210".parse().unwrap();
    let relay_b: SocketAddr = "198.51.100.1:47211".parse().unwrap();
    let new_side = |name, relayed: SocketAddr, host: &str| Side {
        name,
        rtc: {
            let mut rtc = RtcConfig::new()
                .clear_codecs()
                .enable_h264(true)
                .build(start);
            rtc.add_local_candidate(
                Candidate::relayed(relayed, host.parse().unwrap(), "udp").unwrap(),
            );
            rtc
        },
        relayed,
        connected: false,
        sent: Vec::new(),
        states: Vec::new(),
    };
    let mut a = new_side("viewer", relay_a, "10.0.0.1:5000");
    let mut b = new_side("agent", relay_b, "10.0.0.2:5000");
    let mut api = a.rtc.sdp_api();
    api.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None);
    api.add_channel("control".to_string());
    let (offer, pending) = api.apply().unwrap();
    let answer = b
        .rtc
        .sdp_api()
        .accept_offer(SdpOffer::from_sdp_string(&offer.to_sdp_string()).unwrap())
        .unwrap();
    a.rtc
        .sdp_api()
        .accept_answer(
            pending,
            SdpAnswer::from_sdp_string(&answer.to_sdp_string()).unwrap(),
        )
        .unwrap();

    // Each side as the network thread runs it: str0m's controlled side asks for its first timeout years
    // away until it is given time, so the thread starts with a timeout input and, until connected, never
    // waits longer than 50 ms.
    let mut now = start;
    let mut in_flight: Vec<(SocketAddr, SocketAddr, Vec<u8>)> = Vec::new();
    let (_, untouched) = drain(&mut b);
    let agent_first_wait = untouched.saturating_duration_since(now);
    a.rtc.handle_input(Input::Timeout(now)).unwrap();
    b.rtc.handle_input(Input::Timeout(now)).unwrap();
    let (out, mut next_a) = drain(&mut a);
    in_flight.extend(out);
    let (out, mut next_b) = drain(&mut b);
    in_flight.extend(out);
    let cap = |at: Instant, now: Instant, connected: bool| {
        if connected {
            at
        } else {
            at.min(now + Duration::from_millis(50))
        }
    };
    while now < start + Duration::from_secs(10) && !(a.connected && b.connected) {
        for (source, destination, bytes) in std::mem::take(&mut in_flight) {
            let to_a = destination == a.relayed;
            let side = if to_a { &mut a } else { &mut b };
            // The permission: this side must have sent to `source` before.
            if !side.sent.iter().any(|(_, to)| *to == source) {
                continue;
            }
            let input = Input::Receive(
                now,
                Receive {
                    proto: Protocol::Udp,
                    source,
                    destination,
                    contents: bytes.as_slice().try_into().unwrap(),
                },
            );
            side.rtc.handle_input(input).unwrap();
            let (out, at) = drain(side);
            in_flight.extend(out);
            if to_a {
                next_a = at;
            } else {
                next_b = at;
            }
        }
        if !in_flight.is_empty() {
            continue;
        }
        next_a = cap(next_a, now, a.connected);
        next_b = cap(next_b, now, b.connected);
        now = next_a.min(next_b).max(now);
        if now >= next_a {
            a.rtc.handle_input(Input::Timeout(now)).unwrap();
            let (out, at) = drain(&mut a);
            in_flight.extend(out);
            next_a = at;
        }
        if now >= next_b {
            b.rtc.handle_input(Input::Timeout(now)).unwrap();
            let (out, at) = drain(&mut b);
            in_flight.extend(out);
            next_b = at;
        }
    }
    println!("agent's first wait after the answer, untouched: {agent_first_wait:?}");
    for side in [&a, &b] {
        println!(
            "{}: connected {}, ICE states {:?}, {} packets sent",
            side.name,
            side.connected,
            side.states,
            side.sent.len()
        );
    }
    assert!(
        a.connected && b.connected,
        "relayed-only peers did not connect on their own timers"
    );
}
