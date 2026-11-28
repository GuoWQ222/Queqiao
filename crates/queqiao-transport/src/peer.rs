//! A WebRTC connection between viewer and agent, run on its own thread ("queqiao-net").
//!
//! str0m is sans-IO: this thread owns the UDP socket and the `Rtc`, feeds it packets and timeouts and
//! sends what it produces, following str0m's rule: every change is followed by draining its output. The
//! application talks to the thread through channels; to make outgoing video leave at once, each command
//! also sends a one-byte "wake-up" datagram to the thread's own socket from a local socket, which ends
//! the thread's wait (a socket timeout would otherwise round up to Windows' 15.6 ms timer tick).
//!
//! Before the offer or answer, the candidates are gathered (`gather`): host, server-reflexive and relayed.
//! The main socket is bound to the address the server connection leaves from; for the account's own devices
//! every other uplink (`interfaces`) gets a socket of its own too, bound to its address, with a thread that
//! hands what it receives to this thread (and wakes it). What str0m sends leaves from the socket of its source.
//! What str0m sends from the relayed address goes through the TURN client, and what the TURN server
//! forwards is handed to str0m as received on the relayed address.

use std::collections::{HashSet, VecDeque};

/// Control messages waiting for the channel at most (a few seconds of input and commands).
const MAX_CONTROL_PENDING: usize = 4096;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use queqiao_core::protocol::VideoCodec;
use str0m::bwe::{Bitrate, BweKind};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::ChannelId;
use str0m::media::{Direction, Frequency, KeyframeRequestKind, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

use crate::fec;
use crate::flow::{self, Flow};
use crate::gather::{ExtraDeadline, gather, gather_extra};
use crate::punch::{self, Nat, NatInfo, Puncher};
use crate::quic::{self, Quic, QuicEvent};
use crate::signal::Signaling;
use crate::turn::{IceServers, Incoming, TurnClient};
use crate::{Error, err};
use queqiao_core::account::PathReport;

/// Which of this PC's addresses a session offers: all its uplinks (the account's own devices), or only the one
/// the server connection uses (someone helping from another account does not learn the others); when that one is a
/// tunnel (a proxy or a VPN holding the route), the uplink this PC itself would use too, first
/// ([`crate::interfaces::offered`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addresses {
    All,
    RouteOnly,
}

/// A socket on another uplink.
/// Shared, never duplicated: `UdpSocket::try_clone` is WSADuplicateSocket, which opens this process for
/// PROCESS_DUP_HANDLE, and the agent the service starts may not (its process is closed to that, so no other program
/// can copy its handles): sessions there failed with WSAEINVAL (10022), and so did its sound.
struct Extra {
    socket: Arc<UdpSocket>,
    local: SocketAddr,
}

/// A datagram an extra socket received.
struct Arriving {
    contents: Vec<u8>,
    source: SocketAddr,
    destination: SocketAddr,
}

/// The agent sends video and receives the data channel; the viewer receives video, makes the offer and
/// opens the data channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Agent,
    Viewer,
}

pub enum PeerEvent {
    /// ICE and DTLS are up.
    Connected,
    /// The data channel is open: messages can be sent.
    ChannelOpen,
    /// One whole frame: H.264 (Annex B) or an AV1 temporal unit, as `codec` says. `contiguous` is false after a
    /// loss: a keyframe was requested.
    Video {
        data: Arc<[u8]>,
        time_90khz: u64,
        contiguous: bool,
        received: Instant,
        codec: VideoCodec,
    },
    Data(Vec<u8>),
    /// A stream's channel is open (sending on it may start).
    StreamOpen(Stream),
    /// A message on a stream's channel.
    StreamData(Stream, Vec<u8>),
    /// The viewer lost a frame and needs a keyframe.
    KeyframeRequested,
    /// The way the media goes now: "direct to <address>" or "relayed through <TURN server> to <address>".
    /// Sent when it is first known and whenever it changes.
    Path(String),
    /// Something worth a line in a report: ICE state changes, relay events.
    Note(String),
    /// The estimated bandwidth towards the other side, bits per second (the sender adapts its bitrate).
    Bitrate(u64),
    /// One Opus packet of the controlled PC's sound; `time_48khz` counts samples at 48 kHz.
    Audio {
        data: Arc<[u8]>,
        time_48khz: u64,
    },
    Closed(String),
    /// This side's network changed (the main socket's address is no longer this PC's): the session gathers again
    /// ([`Peer::regather`]) and starts ICE again.
    NetworkChanged,
    /// ICE lost the other side: the session waits for an ICE restart (up to [`RESTART_LIMIT`], then `Closed`).
    Disconnected,
    /// ICE has the other side again (after `Disconnected`).
    Reconnected,
    /// The selected path has carried nothing from the other side for [`STALL_AFTER`] while ICE still counts it alive
    /// (a direct path that died: ICE keeps it for about 20 s, 15 of them for the other side's last check): the session
    /// starts ICE again at once, which moves it to a path that works (a relay) within a second or so.
    Stalled,
    /// The gathering asked for ([`Peer::regather`]) is done: this side's new candidates are in.
    Regathered,
    /// An ICE restart's offer (viewer) or answer (agent), for the other side.
    RestartSdp(String),
    /// The viewer: how the session's path went (its first direct path, or relayed after a minute), once, anonymous:
    /// for the server when it asks for such reports (`Signaling::report_path`).
    PathReport(PathReport),
}

enum Command {
    Video {
        data: Vec<u8>,
        time_90khz: u64,
        codec: VideoCodec,
    },
    Audio {
        data: Vec<u8>,
        time_48khz: u64,
    },
    Data(Vec<u8>),
    StreamData(Stream, Vec<u8>),
    RequestKeyframe,
    /// Parity for the video (the other side repairs lost packets from it: it said so in its hello).
    Fec(bool),
    BitrateRange {
        minimum: u64,
        desired: u64,
    },
    /// The bandwidth estimate starts again from this (the bitrate the encoder gets, bit/s).
    ResetBitrate(u64),
    /// After a network change: gather on these (the server connection's address now, the session's servers).
    Regather(IpAddr, IceServers),
    RestartOffer,
    RestartAnswer(String),
    AcceptRestart(String),
    Close,
}

/// Keyframe requests at most this often (UU Remote limits keyframes to one per 600 ms on loss).
const KEYFRAME_REQUEST_GAP: Duration = Duration::from_millis(600);
/// The bandwidth estimate starts here (UU Remote starts at 8.1 Mbit/s: desktop text needs bits at once); the
/// sender should start its encoder at about this rate and follow `PeerEvent::Bitrate`.
pub const START_BITRATE: u64 = 8_000_000;
/// Until connected, the network thread gives str0m time at least this often (see `Network::run`).
const UNCONNECTED_WAIT: Duration = Duration::from_millis(50);

pub struct Peer {
    commands: Sender<Command>,
    events: Receiver<PeerEvent>,
    wake: Wake,
    thread: Option<JoinHandle<()>>,
    notes: Vec<String>,
    in_flight: Arc<[InFlight; STREAMS]>,
    /// How much all streams together may have in flight (set by the network thread, [`crate::flow`]).
    flow_cap: Arc<AtomicUsize>,
    /// Tests (QUEQIAO_FORCE=flow:<kB>): each stream's cap fixed instead, to measure what the cap costs.
    fixed_cap: Option<usize>,
    /// The files go over QUIC (quic.rs), not over their data channel.
    files_on_quic: Arc<AtomicBool>,
}

/// Bulk data with a data channel of its own and flow control: a sender keeps no more in flight on each than the
/// path needs ([`crate::flow`]; at most [`STREAM_IN_FLIGHT`]). The channels share one SCTP queue, so whatever bulk
/// data waits in it delays input and control messages: the flow control keeps that to about 25 ms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stream {
    /// File transfer.
    Files,
    /// Remote command lines.
    Terminal,
    /// Clipboard images.
    Clipboard,
}

const STREAMS: usize = 3;

impl Stream {
    pub const ALL: [Stream; STREAMS] = [Stream::Files, Stream::Terminal, Stream::Clipboard];

    fn label(self) -> &'static str {
        match self {
            Stream::Files => "files",
            Stream::Terminal => "terminal",
            Stream::Clipboard => "clipboard",
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    fn from_label(label: &str) -> Option<Stream> {
        Stream::ALL
            .into_iter()
            .find(|stream| stream.label() == label)
    }
}

/// Bytes on one stream not yet written into str0m, and waiting in its buffer.
#[derive(Default)]
struct InFlight {
    queued: AtomicUsize,
    buffered: AtomicUsize,
}

/// Bytes a sender keeps in flight at most on one stream (queued for the network thread plus in the channel's
/// buffer); usually less: as much as the path needs (see [`crate::flow`]).
pub const STREAM_IN_FLIGHT: usize = 1024 * 1024;

/// Bytes the files keep waiting for QUIC at most (its own send window holds 8 MB more): QUIC paces and controls its
/// congestion itself, and has a channel of its own, so nothing waits behind the files.
const QUIC_IN_FLIGHT: usize = 4 * 1024 * 1024;

impl Peer {
    /// Negotiates over `signal` (the viewer offers, the agent answers) and starts the network thread, offering
    /// all of this PC's uplinks.
    pub fn start(role: Role, signal: &mut dyn Signaling) -> Result<Peer, Error> {
        Peer::start_with(role, signal, Addresses::All)
    }

    /// As [`Peer::start`], offering the addresses `addresses` says.
    pub fn start_with(
        role: Role,
        signal: &mut dyn Signaling,
        addresses: Addresses,
    ) -> Result<Peer, Error> {
        static PROCESS_SETUP: Once = Once::new();
        PROCESS_SETUP.call_once(|| {
            str0m::crypto::from_feature_flags().install_process_default();
            // 1 ms timer resolution for this process: str0m's timers and pacing need it (Linux's timers are that
            // fine already).
            #[cfg(windows)]
            unsafe {
                windows::Win32::Media::timeBeginPeriod(1)
            };
        });
        let local_ip = signal.local_ip()?;
        let servers = signal.ice_servers()?;
        let relay_only = queqiao_core::forced("network").as_deref() == Some("relay");
        // Tests (QUEQIAO_FORCE=network:route-only): as for someone of another account.
        let addresses = if queqiao_core::forced("network").as_deref() == Some("route-only") {
            Addresses::RouteOnly
        } else {
            addresses
        };
        let Gathering {
            socket,
            local,
            bind_ip,
            extras,
            candidates: local_candidates,
            turn,
            nats: local_nats,
            notes: gathering_notes,
        } = gather_all(local_ip, &servers, addresses, relay_only)?;
        let port = local.port();
        let mut notes = gathering_notes;
        // The files over QUIC when both sides offer it (each side's certificate fingerprint in its SDP). Tests:
        // QUEQIAO_FORCE=files:sctp keeps them on their data channel.
        let identity = if queqiao_core::forced("files").as_deref() == Some("sctp") {
            notes.push("files: QUIC off (QUEQIAO_FORCE=files:sctp)".into());
            None
        } else {
            match quic::Identity::new() {
                Ok(identity) => Some(identity),
                Err(error) => {
                    notes.push(format!("files: no QUIC ({error})"));
                    None
                }
            }
        };
        let peer_quic: Option<String>;
        // The other side's relayed candidates (its TURN allocations): a path to one of them goes through a relay
        // although this side sends straight to it.
        let remote_relays: Vec<SocketAddr>;
        // The other side's NATs and the key punch packets are signed with (from both SDPs).
        let remote_nats: Vec<NatInfo>;
        let punch_key: Option<punch::Key>;
        let remote_candidates: Vec<SocketAddr>;
        set_sctp_timeouts();
        let mut rtc = tune(
            RtcConfig::new()
                .clear_codecs()
                .enable_h264(true)
                .enable_av1(true)
                // One H.265 payload type carries every profile between two Queqiao ends (Main, Main 10, 4:4:4): the
                // decoders read the stream's own parameter sets. Tests: QUEQIAO_FORCE=h265:off.
                .enable_h265(queqiao_core::forced("h265").as_deref() != Some("off"))
                .enable_opus(true, false),
            Bitrate::bps(START_BITRATE),
        )
        .build(Instant::now());
        for candidate in &local_candidates {
            rtc.add_local_candidate(candidate.clone());
        }

        let (mut mid, mut audio_mid, mut channel) = (None, None, None);
        match role {
            Role::Viewer => {
                let mut api = rtc.sdp_api();
                mid = Some(api.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None));
                // The controlled PC's sound (Opus), and this PC's microphone the other way when it is sent.
                audio_mid =
                    Some(api.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None));
                channel = Some(api.add_channel("control".to_string()));
                // A channel for each stream, so a large transfer never delays input or control.
                for stream in Stream::ALL {
                    api.add_channel(stream.label().to_string());
                }
                let (offer, pending) = api
                    .apply()
                    .ok_or_else(|| Error("nothing to offer".into()))?;
                let offer = match &identity {
                    Some(identity) => quic::add_to_sdp(&offer.to_sdp_string(), identity),
                    None => offer.to_sdp_string(),
                };
                let offer = punch::add_to_sdp(&offer, &local_nats);
                signal.send("offer", &offer)?;
                let answer = signal.expect("answer")?;
                peer_quic = quic::from_sdp(&answer);
                remote_relays = relayed_candidates(&answer);
                remote_nats = punch::from_sdp(&answer);
                remote_candidates = candidates_of(&answer, None);
                punch_key = punch::Key::from_sdps(&offer, &answer);
                let answer = SdpAnswer::from_sdp_string(&answer).map_err(err)?;
                rtc.sdp_api().accept_answer(pending, answer).map_err(err)?;
            }
            Role::Agent => {
                let offer_text = signal.expect("offer")?;
                peer_quic = quic::from_sdp(&offer_text);
                remote_relays = relayed_candidates(&offer_text);
                remote_nats = punch::from_sdp(&offer_text);
                remote_candidates = candidates_of(&offer_text, None);
                let offer = SdpOffer::from_sdp_string(&offer_text).map_err(err)?;
                let answer = rtc.sdp_api().accept_offer(offer).map_err(err)?;
                let answer = match (&identity, &peer_quic) {
                    (Some(identity), Some(_)) => {
                        quic::add_to_sdp(&answer.to_sdp_string(), identity)
                    }
                    _ => answer.to_sdp_string(),
                };
                // Only to a Queqiao that punches too (it sent its own NAT): an older one is not told.
                let answer = if remote_nats.is_empty() {
                    answer
                } else {
                    punch::add_to_sdp(&answer, &local_nats)
                };
                punch_key = punch::Key::from_sdps(&offer_text, &answer);
                signal.send("answer", &answer)?;
            }
        }
        // What the other side's NAT does, and the plan for each pair of sockets (this side's, the other's).
        for info in &remote_nats {
            notes.push(format!(
                "NAT of the other side's {}: {} (public {})",
                info.base,
                info.nat.describe(),
                info.public
            ));
        }
        let puncher = match punch_key {
            // Tests: QUEQIAO_FORCE=punch:off (ICE alone, to compare).
            Some(key)
                if !relay_only
                    && !local_nats.is_empty()
                    && !remote_nats.is_empty()
                    && servers.punch.enabled
                    && queqiao_core::forced("punch").as_deref() != Some("off") =>
            {
                let controlling = role == Role::Viewer;
                let mut pairs = Vec::new();
                for local in &local_nats {
                    for remote in &remote_nats {
                        pairs.push((*local, *remote));
                    }
                }
                Some(
                    Puncher::rounds(key, pairs, controlling, Instant::now())
                        .limit_rounds(servers.punch.rounds),
                )
            }
            _ => None,
        };

        let quic = match (&identity, &peer_quic) {
            (Some(identity), Some(fingerprint)) => {
                let made = match role {
                    Role::Viewer => Quic::client(identity, fingerprint),
                    Role::Agent => Quic::server(identity, fingerprint),
                };
                match made {
                    Ok(quic) => {
                        notes.push("files: over QUIC (both sides offer it)".into());
                        Some(quic)
                    }
                    Err(error) => {
                        notes.push(format!("files: over SCTP ({error})"));
                        None
                    }
                }
            }
            (Some(_), None) => {
                notes.push("files: over SCTP (the other side offers no QUIC)".into());
                None
            }
            (None, _) => None,
        };
        let files_on_quic = Arc::new(AtomicBool::new(false));
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let (waker, wake_to, wake_from) = waker_for(bind_ip, port)?;
        let wake = Wake::new(waker, wake_to);
        let (arrivals, arrived) = mpsc::channel();
        let stop_extras = Arc::new(AtomicBool::new(false));
        for extra in &extras {
            let socket = extra.socket.clone();
            let wake = wake.clone();
            let (local, arrivals, stop) = (extra.local, arrivals.clone(), stop_extras.clone());
            std::thread::Builder::new()
                .name("queqiao-net-in".into())
                .spawn(move || receive_extra(socket, local, wake, arrivals, stop))?;
        }
        let in_flight: Arc<[InFlight; STREAMS]> = Arc::new(Default::default());
        let flow = Flow::new();
        let flow_cap = Arc::new(AtomicUsize::new(flow.cap()));
        let mut network = Network {
            rtc,
            socket,
            local,
            remote_relays,
            relay_names: servers
                .turn
                .iter()
                .chain(&servers.backups)
                .filter(|server| !server.name.is_empty())
                .map(|server| (server.address.ip(), server.name.clone()))
                .collect(),
            wake_from,
            mid,
            audio_mid,
            channel,
            events: event_sender,
            commands: command_receiver,
            last_keyframe_request: None,
            turn,
            extras,
            arrived,
            stop_extras,
            punch: puncher,
            arrivals,
            wake: wake.clone(),
            remote_candidates,
            report: PathTrack {
                sent: false,
                started: Instant::now(),
                found: HashSet::new(),
                sockets: HashSet::new(),
                remote_nat: Nat::kind(remote_nats.first()),
            },
            restart: Restart {
                role,
                servers: servers.clone(),
                addresses,
                relay_only,
                local_candidates,
                local_nats,
                pending: None,
                disconnected_since: None,
                next_check: Instant::now() + NETWORK_CHECK,
                main_gone: false,
                moves: 0,
            },
            connected: false,
            dtls_notes: (0, 0),
            path: None,
            clock: Instant::now(),
            streams: [None; STREAMS],
            in_flight: in_flight.clone(),
            control_pending: VecDeque::new(),
            control_rx: None,
            control_unacked: None,
            stream_pending: Default::default(),
            flow,
            flow_cap: flow_cap.clone(),
            written: [0; STREAMS],
            rtt_notes: 0,
            video_noted: (0, 0, 0, None),
            fec_tx: None,
            video_pts: Vec::new(),
            losses: VecDeque::new(),
            fec_rx: fec::Receiver::new(),
            shaper: queqiao_core::forced("shape")
                .as_deref()
                .and_then(crate::shape::Shaper::from_spec),
            cut: queqiao_core::forced("cut")
                .as_deref()
                .and_then(cut_window)
                .map(|(from, to)| (Instant::now() + from, Instant::now() + to)),
            quic,
            quic_pair: None,
            stall: Stall::new(),
            files_on_quic: files_on_quic.clone(),
            deadlines: (None, None),
            runaway_notes: 0,
        };
        let thread = std::thread::Builder::new()
            .name("queqiao-net".into())
            .spawn(move || {
                let reason = match network.run() {
                    Ok(()) => "closed".to_string(),
                    Err(error) => error.to_string(),
                };
                let _ = network.events.send(PeerEvent::Closed(reason));
            })?;
        Ok(Peer {
            commands: command_sender,
            events: event_receiver,
            wake,
            thread: Some(thread),
            notes,
            in_flight,
            flow_cap,
            fixed_cap: queqiao_core::forced("flow")
                .and_then(|kb| kb.parse::<usize>().ok())
                .map(|kb| kb * 1024),
            files_on_quic,
        })
    }

    /// The candidates gathered for this session, and what failed, one line each.
    pub fn candidates(&self) -> &[String] {
        &self.notes
    }

    /// Sends one encoded frame (Annex-B H.264). Dropped while not connected yet.
    /// Sends one frame of `codec` (both sides offer H.264 and AV1; the viewer says which it can decode).
    pub fn send_video(&self, data: Vec<u8>, time_90khz: u64, codec: VideoCodec) {
        self.command(Command::Video {
            data,
            time_90khz,
            codec,
        });
    }

    /// Sends a message on the data channel (kept until the channel is open and has room; in order).
    pub fn send_data(&self, bytes: Vec<u8>) {
        self.command(Command::Data(bytes));
    }

    /// Sends a message on a stream's channel (kept until it is open and has room; in order).
    pub fn send_stream(&self, stream: Stream, bytes: Vec<u8>) {
        self.in_flight[stream.index()]
            .queued
            .fetch_add(bytes.len(), Ordering::Relaxed);
        self.command(Command::StreamData(stream, bytes));
    }

    /// How many more bytes may be sent on `stream` now: its share of what the path needs in flight
    /// ([`crate::flow`]; busy streams share it), less what it has in flight.
    pub fn room(&self, stream: Stream) -> usize {
        let in_flight = |stream: Stream| {
            let counters = &self.in_flight[stream.index()];
            counters.queued.load(Ordering::Relaxed) + counters.buffered.load(Ordering::Relaxed)
        };
        if stream == Stream::Files && self.files_on_quic.load(Ordering::Relaxed) {
            return QUIC_IN_FLIGHT.saturating_sub(in_flight(stream));
        }
        let others = Stream::ALL
            .into_iter()
            .filter(|other| *other != stream && in_flight(*other) > 0)
            .count();
        if let Some(fixed) = self.fixed_cap {
            return fixed.saturating_sub(in_flight(stream));
        }
        flow::room(
            self.flow_cap.load(Ordering::Relaxed),
            in_flight(stream),
            others,
            STREAM_IN_FLIGHT,
        )
    }

    /// The bitrate range the sender wants: the bandwidth estimate probes up to `desired` (str0m's BWE).
    pub fn set_bitrate_range(&self, minimum: u64, desired: u64) {
        self.command(Command::BitrateRange { minimum, desired });
    }

    /// The bandwidth estimate starts again from `bitrate` (what the encoder gets, as `PeerEvent::Bitrate` gives it):
    /// for a sender that knows the path has carried that much while the estimate sank on a still screen. A path that
    /// no longer carries it brings the estimate down again within a second or so.
    pub fn reset_bitrate(&self, bitrate: u64) {
        self.command(Command::ResetBitrate(bitrate));
    }

    /// Asks the sender for a keyframe (the viewer does this by itself after a loss).
    pub fn request_keyframe(&self) {
        self.command(Command::RequestKeyframe);
    }

    /// After this side's network changed ([`PeerEvent::NetworkChanged`]): gathers again on `local_ip` (the address the
    /// server connection leaves from now) with `servers` (this session's, asked for again): new sockets, relay
    /// allocation and NAT classification; the old candidates leave ICE. [`PeerEvent::Regathered`] when done.
    pub fn regather(&self, local_ip: IpAddr, servers: IceServers) {
        self.command(Command::Regather(local_ip, servers));
    }

    /// The viewer: starts ICE again; the offer comes as [`PeerEvent::RestartSdp`], for the other side.
    pub fn restart_offer(&self) {
        self.command(Command::RestartOffer);
    }

    /// The viewer: the other side's answer to its restart offer.
    pub fn restart_answer(&self, sdp: String) {
        self.command(Command::RestartAnswer(sdp));
    }

    /// The agent: the viewer's restart offer; the answer comes as [`PeerEvent::RestartSdp`], for the viewer.
    pub fn accept_restart(&self, sdp: String) {
        self.command(Command::AcceptRestart(sdp));
    }

    /// Sends parity for the video ([`crate::fec`]): only to a side that said in its hello it uses it. The bitrate
    /// estimate given to the encoder is then less the parity's share.
    pub fn send_fec(&self, on: bool) {
        self.command(Command::Fec(on));
    }

    pub fn try_event(&self) -> Option<PeerEvent> {
        self.events.try_recv().ok()
    }

    /// The next event, waiting at most `timeout`.
    pub fn wait_event(&self, timeout: Duration) -> Option<PeerEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    /// A handle another thread can send sound with (sound is captured on its own thread, and waiting for
    /// the session loop would add delay).
    pub fn audio_sender(&self) -> Result<AudioSender, Error> {
        Ok(AudioSender {
            commands: self.commands.clone(),
            wake: self.wake.clone(),
        })
    }

    fn command(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            self.wake.wake();
        }
    }
}

/// Sends Opus packets into the session from any thread; see [`Peer::audio_sender`].
pub struct AudioSender {
    commands: Sender<Command>,
    wake: Wake,
}

impl AudioSender {
    /// One Opus packet; `time_48khz` counts samples at 48 kHz (gaps for silence are fine).
    pub fn send(&self, data: Vec<u8>, time_48khz: u64) {
        if self
            .commands
            .send(Command::Audio { data, time_48khz })
            .is_ok()
        {
            self.wake.wake();
        }
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.command(Command::Close);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The data channels' SCTP retransmission timeouts. RFC 4960's (3 s at first, never under 1 s) made file transfers
/// stall: sctp-proto sends without pacing, a burst overflows a queue on the path and loses its tail, no later SACK
/// reports the gap, and the timer is the only way out; measured 2026-11-23 between the test laptop's Wi-Fi and the PC,
/// 14 pauses of about 1 s in 21 s, 4.9 MB/s where the link carries 11.3. The minimum stays above the receiver's 200 ms
/// delayed SACK, so a lone packet's late SACK is no timeout. Test switches: QUEQIAO_FORCE=sctp_rto_min:<ms>,
/// sctp_rto_max:<ms>.
/// The longest wait is 1 s (3 s before 2026-11-27): the control channel (mouse, keyboard, control messages) is reliable
/// and in order, so everything after the messages lost while a network went away waited for their retransmission,
/// whose backoff had reached 3 s by then; 4.8 s without input after a 2 s outage on the test laptop (the picture, on
/// RTP, was back after 2 s). Now input comes back within 1 s of the path. Files go over QUIC when they can, so a link
/// slower than 1 s round trip mostly costs a few needless retransmissions of small control messages.
pub const SCTP_RTO_MS: (u64, u64, u64) = (1000, 250, 1000);

fn set_sctp_timeouts() {
    let (initial, mut min, mut max) = SCTP_RTO_MS;
    if let Some(forced) = queqiao_core::forced("sctp_rto_min").and_then(|v| v.parse::<u64>().ok()) {
        min = forced;
    }
    // Tests: QUEQIAO_FORCE=sctp_rto_max:<ms> (3000 was the longest wait before 2026-11-27, for a comparison).
    if let Some(forced) = queqiao_core::forced("sctp_rto_max").and_then(|v| v.parse::<u64>().ok()) {
        max = forced;
    }
    sctp_proto::set_default_rto_ms(initial, min, max.max(min));
}

/// str0m settings for desktop video (tests/loss_recovery.rs measures them on a lossy link):
/// - hold up to 2048 video packets for reordering and repair (str0m: "at least as big as the number of packets the
///   biggest keyframe can be split over"; its default of 30 gives up on a 1440p keyframe at the first loss, which
///   costs a new keyframe, a bigger burst and more loss);
/// - keep the last 4096 sent video packets for retransmission (NACK), so a keyframe can still be repaired;
/// - bandwidth estimation (transport-wide congestion control) from `start`, which also paces the packets out
///   instead of sending a keyframe as one burst;
/// - 4 MB of data-channel buffer (str0m's default of 128 kB refuses writes beyond it, which dropped file chunks)
///   and messages up to 1 MiB (the default 256 KiB is just below a full clipboard message);
/// - stats once a second: the path's round trip (ICE's STUN checks) for the streams' flow control.
pub fn tune(config: RtcConfig, start: Bitrate) -> RtcConfig {
    config
        .set_reordering_size_video(2048)
        .set_send_buffer_video(4096)
        .enable_bwe(Some(start))
        .set_sctp_max_buffered_amount(4 << 20)
        .set_sctp_max_message_size(1 << 20)
        // Once a second: the path's round trip from ICE's checks, for the streams' flow control.
        .set_stats_interval(Some(Duration::from_secs(1)))
}

/// Writes `pending` into channel `id` until str0m refuses (buffer full) or the queue is empty; `queued` counts the
/// bytes still waiting, `written` those str0m took.
fn flush_channel(
    rtc: &mut Rtc,
    id: Option<ChannelId>,
    pending: &mut VecDeque<Vec<u8>>,
    queued: Option<&AtomicUsize>,
    mut written: Option<&mut u64>,
) {
    let Some(id) = id else {
        return;
    };
    while let Some(bytes) = pending.front() {
        let Some(mut channel) = rtc.channel(id) else {
            return;
        };
        match channel.write(true, bytes) {
            Ok(true) => {
                if let Some(queued) = queued {
                    queued.fetch_sub(bytes.len(), Ordering::Relaxed);
                }
                if let Some(written) = written.as_deref_mut() {
                    *written += bytes.len() as u64;
                }
                pending.pop_front();
            }
            // Full: try again on the next round (or when str0m reports the buffer low).
            Ok(false) => return,
            // A message str0m can never take (too large): dropped rather than blocking the channel.
            Err(_) => {
                if let Some(queued) = queued {
                    queued.fetch_sub(bytes.len(), Ordering::Relaxed);
                }
                pending.pop_front();
            }
        }
    }
}

/// The DTLS records in a datagram, for notes: "hs1" (handshake, its message type: 1 ClientHello, 2 ServerHello, 11
/// Certificate, 12 ServerKeyExchange, 13 CertificateRequest, 14 ServerHelloDone, 15 CertificateVerify, 16
/// ClientKeyExchange), "ccs", "enc" (a record of epoch 1 or later: Finished or application data), "alert".
fn dtls_records(bytes: &[u8]) -> String {
    let mut parts = Vec::new();
    let mut at = 0;
    while at + 13 <= bytes.len() {
        let kind = bytes[at];
        let epoch = u16::from_be_bytes([bytes[at + 3], bytes[at + 4]]);
        let length = usize::from(u16::from_be_bytes([bytes[at + 11], bytes[at + 12]]));
        parts.push(match (kind, epoch) {
            (20, _) => "ccs".to_string(),
            (21, 0) => "alert".to_string(),
            (22, 0) => format!("hs{}", bytes.get(at + 13).copied().unwrap_or(0)),
            (_, _) => format!("enc{kind}"),
        });
        at += 13 + length;
    }
    parts.join(" ")
}

/// Receives on another uplink's socket and hands each datagram to the network thread, waking it.
fn receive_extra(
    socket: Arc<UdpSocket>,
    local: SocketAddr,
    wake: Wake,
    arrivals: Sender<Arriving>,
    stop: Arc<AtomicBool>,
) {
    let _ = socket.set_read_timeout(Some(Duration::from_millis(200)));
    let mut buffer = vec![0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        match socket.recv_from(&mut buffer) {
            Ok((length, source)) => {
                let arriving = Arriving {
                    contents: buffer[..length].to_vec(),
                    source,
                    destination: local,
                };
                if arrivals.send(arriving).is_err() {
                    return;
                }
                wake.wake();
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::ConnectionReset
                ) => {}
            Err(_) => return,
        }
    }
}

/// The relayed candidates of an SDP (`a=candidate:<foundation> <component> <transport> <priority> <address> <port>
/// typ relay ...`): the other side's TURN allocations.
pub(crate) fn relayed_candidates(sdp: &str) -> Vec<SocketAddr> {
    candidates_of(sdp, Some("relay"))
}

/// The addresses of an SDP's candidates, of one type (`typ <kind>`) or all.
fn candidates_of(sdp: &str, kind: Option<&str>) -> Vec<SocketAddr> {
    sdp.lines()
        .filter_map(|line| line.trim().strip_prefix("a=candidate:"))
        .filter_map(|candidate| {
            let parts: Vec<&str> = candidate.split_whitespace().collect();
            if let Some(kind) = kind {
                let at = parts.iter().position(|part| *part == "typ")?;
                if parts.get(at + 1) != Some(&kind) {
                    return None;
                }
            }
            let ip: IpAddr = parts.get(4)?.parse().ok()?;
            let port: u16 = parts.get(5)?.parse().ok()?;
            Some(SocketAddr::new(ip, port))
        })
        .collect()
}

/// How other threads wake the network thread: a one-byte datagram from a socket of its own to the main socket (which
/// listens on its own address only). Shared and replaceable: after a network change the session has a new main socket
/// on a new address, and the wake-ups follow it.
#[derive(Clone)]
struct Wake(Arc<Mutex<(UdpSocket, SocketAddr)>>);

impl Wake {
    fn new(socket: UdpSocket, to: SocketAddr) -> Wake {
        Wake(Arc::new(Mutex::new((socket, to))))
    }

    fn wake(&self) {
        if let Ok(wake) = self.0.lock() {
            let _ = wake.0.send_to(&[0], wake.1);
        }
    }

    fn set(&self, socket: UdpSocket, to: SocketAddr) {
        if let Ok(mut wake) = self.0.lock() {
            *wake = (socket, to);
        }
    }
}

/// The socket that wakes the network thread, the main socket's address it sends to, and its own address (what comes
/// from there is a wake-up): on the main socket's address, the loopback when that is "any".
fn waker_for(bind_ip: Ipv4Addr, port: u16) -> Result<(UdpSocket, SocketAddr, SocketAddr), Error> {
    let wake_ip = if bind_ip.is_unspecified() {
        Ipv4Addr::LOCALHOST
    } else {
        bind_ip
    };
    let waker = UdpSocket::bind((wake_ip, 0))?;
    let from = waker.local_addr()?;
    Ok((waker, SocketAddr::new(wake_ip.into(), port), from))
}

/// How long the selected path may carry nothing from the other side before the session starts ICE again: str0m checks
/// every pair at least every 3 s and the other side answers, so a path that works is never this quiet, whatever the
/// screen does (2026-11-27: a direct path that died took ICE about 20 s to give up, and the picture stood still).
pub const STALL_AFTER: Duration = Duration::from_secs(5);

/// The selected pair (this side's address, the other side's) and when anything last came on it from the other side.
struct Stall {
    selected: Option<(SocketAddr, SocketAddr)>,
    /// Since when it is selected (or ICE started again), and when the other side was last heard on it.
    since: Instant,
    last_heard: Option<Instant>,
    /// Told once per selected pair (and restart).
    told: bool,
}

impl Stall {
    fn new() -> Stall {
        Stall {
            selected: None,
            since: Instant::now(),
            last_heard: None,
            told: false,
        }
    }

    /// The pair media leaves by now (a new one starts afresh).
    fn select(&mut self, local: SocketAddr, remote: SocketAddr, now: Instant) {
        if self.selected != Some((local, remote)) {
            self.selected = Some((local, remote));
            self.since = now;
            self.last_heard = None;
            self.told = false;
        }
    }

    /// A datagram for ICE (STUN, DTLS, media) from `remote` to `local`.
    fn heard(&mut self, local: SocketAddr, remote: SocketAddr, now: Instant) {
        if self.selected == Some((local, remote)) {
            self.last_heard = Some(now);
        }
    }

    /// ICE started again: the pair is given its time anew.
    fn restarted(&mut self, now: Instant) {
        self.since = now;
        self.last_heard = None;
        self.told = false;
    }

    fn quiet_since(&self) -> Instant {
        self.last_heard
            .map_or(self.since, |heard| heard.max(self.since))
    }

    /// How long the selected pair has been quiet, once it passes [`STALL_AFTER`] (told once).
    fn due(&mut self, now: Instant) -> Option<Duration> {
        if self.told || self.selected.is_none() {
            return None;
        }
        let silent = now.saturating_duration_since(self.quiet_since());
        (silent >= STALL_AFTER).then(|| {
            self.told = true;
            silent
        })
    }

    /// When to look again.
    fn next(&self) -> Option<Instant> {
        (!self.told && self.selected.is_some()).then(|| self.quiet_since() + STALL_AFTER)
    }
}

/// How often the network thread checks that the main socket's address is still this PC's.
const NETWORK_CHECK: Duration = Duration::from_secs(2);
/// How long a session waits for ICE to come back (an ICE restart) once it lost the other side, before it ends.
pub const RESTART_LIMIT: Duration = Duration::from_secs(40);

/// What a session needs to start ICE again after a network change: its role (the viewer offers), the servers and
/// addresses to gather with, this side's candidates and NATs now (replaced when it gathers again), a restart offer
/// waiting for its answer, since when ICE has lost the other side, when the network is checked next, whether the main
/// socket's address went away (told once).
/// What the session's anonymous path report (`PeerEvent::PathReport`) is made of: when the session began, the
/// addresses punching found and its own sockets that became the session's, the other side's NAT kind.
struct PathTrack {
    sent: bool,
    started: Instant,
    found: HashSet<SocketAddr>,
    sockets: HashSet<SocketAddr>,
    remote_nat: &'static str,
}

/// How long a session is relayed before its report says so.
const REPORT_RELAYED_AFTER: Duration = Duration::from_secs(60);

struct Restart {
    role: Role,
    servers: IceServers,
    addresses: Addresses,
    relay_only: bool,
    local_candidates: Vec<Candidate>,
    local_nats: Vec<NatInfo>,
    pending: Option<(SdpPendingOffer, String)>,
    disconnected_since: Option<Instant>,
    next_check: Instant,
    main_gone: bool,
    /// How often it gathered again (the test switch `move` acts once).
    moves: u32,
}

/// What a session gathered on this PC's networks (at its start, and again after a network change): the main socket and
/// its address, the other uplinks' sockets, the candidates (the preferred first), the relay allocation, what each IPv4
/// socket's NAT does, one line per finding.
struct Gathering {
    socket: UdpSocket,
    local: SocketAddr,
    bind_ip: Ipv4Addr,
    extras: Vec<Extra>,
    candidates: Vec<Candidate>,
    turn: Option<TurnClient>,
    nats: Vec<NatInfo>,
    notes: Vec<String>,
}

/// Gathers on the networks this PC has now: the main socket on `local_ip` (the address the server connection leaves
/// from), every other uplink (the account's own devices) and the public IPv6 addresses, the relay allocation and the
/// NAT of each socket (`gather`, `gather_extra`), within 1.5 s.
fn gather_all(
    local_ip: IpAddr,
    servers: &IceServers,
    addresses: Addresses,
    relay_only: bool,
) -> Result<Gathering, Error> {
    // Bound to the address the server connection leaves from: a socket on "any" would send out of whatever
    // interface the route picks, a VPN's or proxy's tunnel included (see `interfaces`).
    let bind_ip = match local_ip {
        IpAddr::V4(ip) if !ip.is_unspecified() => ip,
        _ => Ipv4Addr::UNSPECIFIED,
    };
    let socket = bind_socket(bind_ip.into())?;
    let port = socket.local_addr()?.port();
    let local = SocketAddr::new(local_ip, port);
    let uplinks = if !relay_only && !bind_ip.is_unspecified() {
        crate::interfaces::uplinks()
    } else {
        Vec::new()
    };
    let offered = crate::interfaces::offered(bind_ip, &uplinks, addresses == Addresses::All);
    let extra = |ip: IpAddr| {
        let socket = bind_socket(ip).ok()?;
        let local = socket.local_addr().ok()?;
        Some(Extra {
            socket: Arc::new(socket),
            local,
        })
    };
    let mut extras: Vec<Extra> = offered
        .iter()
        .filter_map(|ip| extra((*ip).into()))
        .collect();
    // Tests (QUEQIAO_FORCE=network:uplinks): this PC's uplinks first even when the server connection's address is
    // not a tunnel (on one PC it is the loopback): the session goes through the uplinks' sockets.
    let uplinks_forced =
        queqiao_core::forced("network").as_deref() == Some("uplinks") && !extras.is_empty();
    let main_is_tunnel =
        uplinks_forced || crate::interfaces::is_tunnel(bind_ip, &uplinks, extras.len());
    // IPv6 (2026-11-24): this PC's public IPv6 addresses, for the account's own devices only (like its other
    // uplinks: someone of another account does not learn them). No NAT there: two PCs with IPv6 connect directly,
    // each firewall letting in the other's checks as answers once both have sent. After the IPv4 uplinks.
    // Tests: QUEQIAO_FORCE=ipv6:off leaves them out; ipv6:any takes private ones too (Tailscale's, for a test on
    // one PC); network:ipv6 offers them alone, no IPv4 candidate, to prove the path.
    let ipv6_forced = queqiao_core::forced("ipv6");
    let ipv6_only = queqiao_core::forced("network").as_deref() == Some("ipv6");
    if addresses == Addresses::All && !relay_only && ipv6_forced.as_deref() != Some("off") {
        let any = ipv6_forced.as_deref() == Some("any");
        extras.extend(
            crate::interfaces::ipv6_uplinks(any)
                .into_iter()
                .filter_map(|ip| extra(ip.into())),
        );
    }
    // The servers each socket asks: the first for its server-reflexive candidate, all of them for what its NAT does.
    let stun = crate::gather::nat_servers(servers);
    let deadline = ExtraDeadline::new(Instant::now());
    let (gathered, found) = std::thread::scope(|scope| {
        let jobs: Vec<_> = extras
            .iter()
            .map(|extra| {
                let (deadline, stun) = (&deadline, &stun);
                scope.spawn(move || gather_extra(&extra.socket, extra.local, stun, deadline))
            })
            .collect();
        let gathered = gather(&socket, local, servers, relay_only);
        deadline.main_done();
        let found: Vec<_> = jobs.into_iter().filter_map(|job| job.join().ok()).collect();
        (gathered, found)
    });
    let gathered = gathered?;
    let mut notes = gathered.notes;
    // The candidates added first are preferred (str0m's local preference goes down by the order of each kind).
    // The server connection may leave through a VPN or a proxy's tunnel holding the route (Clash Verge's
    // Mihomo on the test laptop, 2026-11-23: the session went through it): then this PC's own uplinks come first
    // and the tunnel is the fallback, as UU Remote gives VPN networks a cost.
    let mut uplink_candidates = Vec::new();
    // What each IPv4 socket's NAT does, for the other side (in the SDP) and for punching.
    let mut nats: Vec<NatInfo> = gathered.nat.into_iter().collect();
    for (candidates, extra_notes, nat) in found {
        uplink_candidates.extend(candidates);
        notes.extend(extra_notes);
        nats.extend(nat);
    }
    if ipv6_only {
        nats.clear();
    }
    let (first, then) = if main_is_tunnel {
        notes.push(format!(
            "{bind_ip} (the server connection's) is a tunnel, a VPN or a proxy: this PC's uplinks come first, it is the fallback"
        ));
        (uplink_candidates, gathered.candidates)
    } else {
        (gathered.candidates, uplink_candidates)
    };
    let candidates: Vec<Candidate> = first
        .into_iter()
        .chain(then)
        .filter(|candidate| !ipv6_only || !candidate.addr().is_ipv4())
        .collect();
    if ipv6_only {
        notes.push("IPv6 candidates only (QUEQIAO_FORCE=network:ipv6)".into());
    }
    Ok(Gathering {
        socket,
        local,
        bind_ip,
        extras,
        candidates,
        turn: gathered.turn,
        nats,
        notes,
    })
}

/// `QUEQIAO_FORCE=cut:<from>-<to>` (seconds of the session, whole or decimal): when the direct paths are cut.
fn cut_window(spec: &str) -> Option<(Duration, Duration)> {
    let (from, to) = spec.split_once('-')?;
    let (from, to) = (
        from.trim().parse::<f64>().ok()?,
        to.trim().parse::<f64>().ok()?,
    );
    (from >= 0.0 && to > from && to < 86_400.0)
        .then(|| (Duration::from_secs_f64(from), Duration::from_secs_f64(to)))
}

fn bind_socket(ip: IpAddr) -> Result<UdpSocket, Error> {
    use socket2::{Domain, Protocol as SocketProtocol, Socket, Type};
    let socket = Socket::new(
        Domain::for_address(SocketAddr::new(ip, 0)),
        Type::DGRAM,
        Some(SocketProtocol::UDP),
    )?;
    if ip.is_ipv6() {
        // IPv6 only: an IPv4 packet must not reach it through a mapped address.
        socket.set_only_v6(true)?;
    }
    // A keyframe is a burst of a few hundred packets: Windows' default 64 KB buffers would drop some.
    let _ = socket.set_send_buffer_size(4 << 20);
    let _ = socket.set_recv_buffer_size(4 << 20);
    socket.bind(&SocketAddr::from((ip, 0)).into())?;
    Ok(socket.into())
}

struct Network {
    rtc: Rtc,
    socket: UdpSocket,
    local: SocketAddr,
    wake_from: SocketAddr,
    mid: Option<Mid>,
    audio_mid: Option<Mid>,
    channel: Option<ChannelId>,
    streams: [Option<ChannelId>; STREAMS],
    in_flight: Arc<[InFlight; STREAMS]>,
    /// Messages for the control channel and each stream not yet taken by str0m.
    control_pending: VecDeque<Vec<u8>>,
    /// The control channel, for the notes on a pause in it (2026-11-27: input waited seconds after a network change
    /// while the video went on): when data last came on it, and since when this side's data on it is unacknowledged
    /// (said once a pause).
    control_rx: Option<Instant>,
    control_unacked: Option<(Instant, bool)>,
    stream_pending: [VecDeque<Vec<u8>>; STREAMS],
    events: Sender<PeerEvent>,
    commands: Receiver<Command>,
    last_keyframe_request: Option<Instant>,
    turn: Option<TurnClient>,
    /// The other uplinks' sockets, what they received, and the flag that ends their threads.
    extras: Vec<Extra>,
    arrived: Receiver<Arriving>,
    stop_extras: Arc<AtomicBool>,
    /// Punching for a direct path (`punch`) while the other side punches too; a new socket that finds one becomes an
    /// extra socket (its thread gets `arrivals` and `wake` as the others').
    punch: Option<Puncher>,
    arrivals: Sender<Arriving>,
    wake: Wake,
    /// Every address the other side offered (its SDP's candidates) and every one punching handed to ICE since.
    remote_candidates: Vec<SocketAddr>,
    /// Starting ICE again after a network change ([`Peer::regather`], [`Peer::restart_offer`], ...).
    restart: Restart,
    report: PathTrack,
    connected: bool,
    /// DTLS packets noted before connecting (out, in): where the handshake goes and how long it takes.
    dtls_notes: (u32, u32),
    path: Option<String>,
    /// The other side's relayed candidates: a path to one of them is relayed by that side.
    remote_relays: Vec<SocketAddr>,
    /// The relays the account server named (address and name): the other side's relay is named by its address.
    relay_names: Vec<(std::net::IpAddr, String)>,
    /// The latest time given to str0m: inputs never go back in time (deadlines are served up to 1 ms early).
    clock: Instant,
    /// How much the streams may have in flight, from what the path delivers ([`crate::flow`]); the cap is shared
    /// with the peer's `room`.
    flow: Flow,
    flow_cap: Arc<AtomicUsize>,
    /// Bytes each stream has written into str0m so far (less its buffered amount: acknowledged).
    written: [u64; STREAMS],
    /// Notes on the path's round trip so far (a few at most).
    rtt_notes: u32,
    /// The video's RTP counters last noted (str0m's stats: packets, NACKs, PLIs), and when: a note at most every
    /// 5 s while they change (the reports show how losses were repaired).
    video_noted: (u64, u64, u64, Option<Instant>),
    /// Parity for the video sent (when the other side uses it), and the video's payload types (its packets).
    fec_tx: Option<fec::Sender>,
    video_pts: Vec<u8>,
    /// The loss the other side reported (str0m's stats, once a second) over the last seconds: how much parity.
    losses: VecDeque<(Instant, f32)>,
    /// Lost video packets rebuilt from the other side's parity.
    fec_rx: fec::Receiver,
    /// Tests: a bottleneck for what this side sends (QUEQIAO_FORCE=shape:<kbit/s>/<queue ms>/<loss per mille>).
    shaper: Option<crate::shape::Shaper>,
    /// Tests: the direct paths cut from one time to another of the session (QUEQIAO_FORCE=cut:<from s>-<to s>):
    /// every datagram to or from an address that is not a relay's left out both ways, as when a carrier's NAT drops a
    /// mapping mid-session; the relays (this side's TURN server, the other side's relay addresses) still carry.
    cut: Option<(Instant, Instant)>,
    /// The file channel over QUIC while it is negotiated and has not failed; its datagrams go over the selected
    /// pair (source, destination), the one DTLS and SRTP use.
    quic: Option<Quic>,
    quic_pair: Option<(SocketAddr, SocketAddr)>,
    /// The selected path's liveness ([`PeerEvent::Stalled`]).
    stall: Stall,
    files_on_quic: Arc<AtomicBool>,
    /// The last deadlines asked for (str0m's, QUIC's), and notes on runaway loops so far.
    deadlines: (Option<Instant>, Option<Instant>),
    runaway_notes: u32,
}

impl Drop for Network {
    fn drop(&mut self) {
        // The other uplinks' threads end within their 200 ms receive wait.
        self.stop_extras.store(true, Ordering::Relaxed);
    }
}

impl Network {
    fn run(&mut self) -> Result<(), Error> {
        let mut buffer = vec![0u8; 2048];
        // str0m's ICE in the controlled role (the agent) schedules its first check only once it is given
        // time: until then it asks for a timeout years away (measured: 315,360,000 s). Directly, the
        // viewer's checks wake it; through TURN they are dropped until we have sent something ourselves
        // (permissions), so nothing would move. Hence a timeout input at once, and short waits until
        // connected (tests/relay_ice.rs).
        // Punching first: its packet to the other side's public address leaves before ICE's checks, so on a NAT that
        // counts its ports up it gets the next one, the one the other side predicts.
        self.punch_tick()?;
        let start = self.clock(Instant::now());
        let started = Instant::now();
        self.rtc.handle_input(Input::Timeout(start)).map_err(err)?;
        let mut immediate = 0u32;
        loop {
            let Some(mut deadline) = self.drain()? else {
                return Ok(());
            };
            if let Some(at) = self.punch_tick()? {
                deadline = deadline.min(at);
            }
            self.check_network(started)?;
            deadline = deadline.min(self.restart.next_check);
            if self.connected && self.restart.disconnected_since.is_none() {
                if let Some(silent) = self.stall.due(Instant::now()) {
                    self.note(format!(
                        "the path {} has carried nothing from the other side for {} ms (ICE still counts it alive): starting ICE again",
                        self.path.as_deref().unwrap_or("selected"),
                        silent.as_millis()
                    ));
                    let _ = self.events.send(PeerEvent::Stalled);
                }
                if let Some(at) = self.stall.next() {
                    deadline = deadline.min(at);
                }
            }
            if let Some(since) = self.restart.disconnected_since {
                deadline = deadline.min(since + RESTART_LIMIT);
            }
            // Tests: what the bottleneck has carried goes out, and the loop wakes for the next.
            self.release_shaped();
            if let Some(next) = self
                .shaper
                .as_ref()
                .and_then(crate::shape::Shaper::next_release)
            {
                deadline = deadline.min(next);
            }
            // str0m's pacer releases a packet and asks for the next one a fraction of a millisecond later
            // (one packet's time at the pacing rate); its budget cannot go below zero. Waking up no more
            // often than every millisecond (the socket wait) would cap paced video at about 1000 packets a
            // second, roughly 10 Mbit/s, whatever the estimate (tests/loss_recovery.rs). A deadline less than
            // 1 ms away is served at once, as that time (packets leave up to 1 ms early); the clock never goes
            // back, and the guard only stops a runaway loop.
            // Compared with real time, not with the clock: the clock may already be ahead, and comparing with it
            // would let str0m's time run away from real time (its ICE consent timers then expire at once).
            let real = Instant::now();
            if deadline < real + Duration::from_millis(1) && immediate < 10_000 {
                immediate += 1;
                let at = self.clock(deadline.max(real));
                self.rtc.handle_input(Input::Timeout(at)).map_err(err)?;
                if immediate.is_multiple_of(64) {
                    // Let commands (new frames, input) in during a long paced burst.
                    if let Ok(command) = self.commands.try_recv()
                        && !self.apply(command)?
                    {
                        return Ok(());
                    }
                }
                continue;
            }
            if immediate >= 10_000 && self.runaway_notes < 3 {
                // The guard stopped a runaway: which deadline kept the loop from waiting (and from reading the
                // network: everything that arrived meanwhile waited).
                self.runaway_notes += 1;
                let now = Instant::now();
                let when = |at: Option<Instant>| match at {
                    Some(at) if at >= now => format!("in {:.3} ms", (at - now).as_secs_f64() * 1e3),
                    Some(at) => format!("{:.3} ms ago", (now - at).as_secs_f64() * 1e3),
                    None => "none".into(),
                };
                let _ = self.events.send(PeerEvent::Note(format!(
                    "the network loop ran 10000 times without waiting: str0m's next deadline {}, QUIC's {}",
                    when(self.deadlines.0),
                    when(self.deadlines.1)
                )));
            }
            immediate = 0;
            if !self.connected {
                deadline = deadline.min(Instant::now() + UNCONNECTED_WAIT);
            }
            match self.commands.try_recv() {
                Ok(command) => {
                    if !self.apply(command)? {
                        return Ok(());
                    }
                    continue;
                }
                Err(TryRecvError::Disconnected) => return Ok(()),
                Err(TryRecvError::Empty) => {}
            }
            // What the other uplinks' sockets received: one datagram at a time, str0m's output taken after each (the
            // loop goes round), as on the main socket. Handed over in a row, a burst of file data filled the DTLS
            // layer's queue (30 packets) and ended the session: "DTLS error: receive queue full" (M4, 2026-11-23,
            // once uplinks came before a tunnel).
            if let Ok(arriving) = self.arrived.try_recv() {
                if self.cut_off(arriving.source) {
                    continue;
                }
                if punch::is_punch(&arriving.contents) {
                    self.on_punch(arriving.destination, arriving.source, &arriving.contents);
                    continue;
                }
                self.dtls_note(
                    false,
                    &arriving.contents,
                    arriving.destination,
                    arriving.source,
                    false,
                );
                let at = self.clock(Instant::now());
                // Not `rtc.accepts`: before ICE has a pair it refuses DTLS from a candidate not yet checked, and the
                // viewer's first ClientHello (sent as soon as its own ICE connected) was dropped here; its resend
                // came 2 s later (seen over the internet 2026-11-21). One Rtc per session: everything goes to it, as
                // on the main socket.
                self.deliver(
                    at,
                    arriving.source,
                    arriving.destination,
                    &arriving.contents,
                )?;
                continue;
            }
            let deadline = match self.turn.as_ref().and_then(TurnClient::next_deadline) {
                Some(turn) => deadline.min(turn),
                None => deadline,
            };
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            if let Err(error) = self.socket.set_read_timeout(Some(wait)) {
                self.main_socket_failed(&error.to_string());
                std::thread::sleep(wait.min(Duration::from_millis(50)));
                let at = self.clock(Instant::now());
                self.rtc.handle_input(Input::Timeout(at)).map_err(err)?;
                continue;
            }
            let input = match self.socket.recv_from(&mut buffer) {
                Ok((_, source)) if source == self.wake_from => continue,
                Ok((length, source))
                    if self.turn.as_ref().is_some_and(|turn| turn.server == source) =>
                {
                    self.handle_turn_packet(&buffer[..length])?;
                    continue;
                }
                Ok((_, source)) if self.cut_off(source) => continue,
                Ok((length, source)) if punch::is_punch(&buffer[..length]) => {
                    let (local, datagram) = (self.local, buffer[..length].to_vec());
                    self.on_punch(local, source, &datagram);
                    continue;
                }
                Ok((length, source)) => {
                    self.dtls_note(false, &buffer[..length], self.local, source, false);
                    let at = self.clock(Instant::now());
                    let local = self.local;
                    self.deliver(at, source, local, &buffer[..length])?;
                    continue;
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    if let Some(turn) = self.turn.as_mut() {
                        turn.poll(Instant::now());
                    }
                    self.flush_turn();
                    Input::Timeout(self.clock(Instant::now()))
                }
                // Windows reports an earlier ICMP "port unreachable" on the next receive: not fatal.
                Err(error) if error.kind() == ErrorKind::ConnectionReset => continue,
                // The address went away (a network change): the session gathers again and starts ICE again; until
                // then the loop goes on (str0m's time and the other sockets).
                Err(error) => {
                    self.main_socket_failed(&error.to_string());
                    std::thread::sleep(wait.min(Duration::from_millis(50)));
                    Input::Timeout(self.clock(Instant::now()))
                }
            };
            self.rtc.handle_input(input).map_err(err)?;
        }
    }

    /// Polls str0m until it only has a timeout left. `None` once the connection is over.
    fn drain(&mut self) -> Result<Option<Instant>, Error> {
        loop {
            match self.rtc.poll_output().map_err(err)? {
                Output::Timeout(deadline) => {
                    self.flush_turn();
                    self.flush_channels();
                    let quic = self.pump_quic();
                    self.deadlines = (Some(deadline), quic);
                    let deadline = match quic {
                        Some(quic) => deadline.min(quic),
                        None => deadline,
                    };
                    return Ok(Some(deadline));
                }
                Output::Transmit(packet) => {
                    let relayed =
                        self.send_datagram(packet.source, packet.destination, &packet.contents);
                    // Parity for the video once its group is complete, the same way.
                    for parity in self.video_parity(&packet.contents) {
                        self.send_datagram(packet.source, packet.destination, &parity);
                    }
                    self.dtls_note(
                        true,
                        &packet.contents,
                        packet.source,
                        packet.destination,
                        relayed.is_some(),
                    );
                    // DTLS and SRTP packets (first byte 20 and up; STUN starts with 0 or 1) go only on the
                    // selected pair, so they show which way the session goes.
                    if self.connected && packet.contents.first().is_some_and(|byte| *byte >= 20) {
                        self.quic_pair = Some((packet.source, packet.destination));
                        self.stall
                            .select(packet.source, packet.destination, Instant::now());
                        let path = match relayed {
                            // With the relay's name when the account server gave one ("relayed through
                            // 198.51.100.1:3478 (shanghai) to ..."; relay_name reads it back).
                            Some(server) => match self
                                .turn
                                .as_ref()
                                .map(|turn| turn.name.as_str())
                                .filter(|name| !name.is_empty())
                            {
                                Some(name) => format!(
                                    "relayed through {server} ({name}) to {}",
                                    packet.destination
                                ),
                                None => {
                                    format!("relayed through {server} to {}", packet.destination)
                                }
                            },
                            // Straight to the other side's relay: relayed all the same (it was shown as direct,
                            // 2026-11-25, the test laptop behind a phone's hotspot).
                            // Named by its address among the relays the account server named for this session (both
                            // ends get the same ones): "... relay 198.51.100.1:47203 (shanghai) (from ...)".
                            None if self.remote_relays.contains(&packet.destination) => {
                                match self
                                    .relay_names
                                    .iter()
                                    .find(|(ip, _)| *ip == packet.destination.ip())
                                {
                                    Some((_, name)) => format!(
                                        "relayed through that PC's relay {} ({name}) (from {})",
                                        packet.destination, packet.source
                                    ),
                                    None => format!(
                                        "relayed through that PC's relay {} (from {})",
                                        packet.destination, packet.source
                                    ),
                                }
                            }
                            None => {
                                format!("direct to {} (from {})", packet.destination, packet.source)
                            }
                        };
                        if self.path.as_ref() != Some(&path) {
                            if relayed.is_none()
                                && !self.remote_relays.contains(&packet.destination)
                            {
                                self.report_path(Some((packet.source, packet.destination)));
                            }
                            self.path = Some(path.clone());
                            let _ = self.events.send(PeerEvent::Path(path));
                        }
                    }
                }
                Output::Event(event) => {
                    if !self.handle(event) {
                        return Ok(None);
                    }
                }
            }
        }
    }

    fn handle(&mut self, event: Event) -> bool {
        let forward = match event {
            Event::Connected => {
                self.connected = true;
                Some(PeerEvent::Connected)
            }
            // Not the end: the session waits for an ICE restart (a network change on either side), up to RESTART_LIMIT
            // (`check_network`). A side that leaves says so first (its Leave message).
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                if self.restart.disconnected_since.is_none() {
                    self.restart.disconnected_since = Some(Instant::now());
                    self.note(format!(
                        "ICE disconnected: waiting up to {} s for a restart",
                        RESTART_LIMIT.as_secs()
                    ));
                }
                Some(PeerEvent::Disconnected)
            }
            Event::IceConnectionStateChange(
                state @ (IceConnectionState::Connected | IceConnectionState::Completed),
            ) if self.restart.disconnected_since.is_some() => {
                let since = self
                    .restart
                    .disconnected_since
                    .take()
                    .unwrap_or_else(Instant::now);
                self.note(format!(
                    "ICE {state:?} again after {} ms",
                    since.elapsed().as_millis()
                ));
                Some(PeerEvent::Reconnected)
            }
            Event::IceConnectionStateChange(state) => {
                Some(PeerEvent::Note(format!("ICE {state:?}")))
            }
            Event::Closed => return false,
            Event::MediaAdded(added) => {
                match added.kind {
                    MediaKind::Audio => self.audio_mid = Some(added.mid),
                    MediaKind::Video => self.mid = Some(added.mid),
                }
                None
            }
            Event::MediaData(data) if Some(data.mid) == self.audio_mid => Some(PeerEvent::Audio {
                data: data.data,
                time_48khz: data.time.numer(),
            }),
            Event::MediaData(data) => {
                if !data.contiguous {
                    self.ask_keyframe(data.mid);
                }
                let codec = match data.params.spec().codec {
                    str0m::format::Codec::Av1 => VideoCodec::Av1,
                    str0m::format::Codec::H265 => VideoCodec::H265,
                    _ => VideoCodec::H264,
                };
                Some(PeerEvent::Video {
                    data: data.data,
                    time_90khz: data.time.numer(),
                    contiguous: data.contiguous,
                    received: Instant::now(),
                    codec,
                })
            }
            Event::ChannelOpen(id, label) => match Stream::from_label(&label) {
                Some(stream) => {
                    self.streams[stream.index()] = Some(id);
                    // While QUIC carries (or may yet carry) the files, their data channel stays unused.
                    if stream == Stream::Files && self.quic.is_some() {
                        None
                    } else {
                        Some(PeerEvent::StreamOpen(stream))
                    }
                }
                None => {
                    self.channel = Some(id);
                    if let Some(quic) = self.quic.as_mut() {
                        quic.start(Instant::now());
                    }
                    Some(PeerEvent::ChannelOpen)
                }
            },
            Event::ChannelData(data) => {
                match Stream::ALL
                    .into_iter()
                    .find(|stream| self.streams[stream.index()] == Some(data.id))
                {
                    Some(stream) => Some(PeerEvent::StreamData(stream, data.data)),
                    None => {
                        // A pause on the control channel (the other side sends on it at least every 500 ms): noted.
                        let now = Instant::now();
                        if let Some(last) = self.control_rx.replace(now)
                            && now - last > Duration::from_secs(1)
                        {
                            self.note(format!(
                                "control channel: data again after {} ms",
                                (now - last).as_millis()
                            ));
                        }
                        Some(PeerEvent::Data(data.data))
                    }
                }
            }
            Event::PeerStats(stats) => {
                let rtt = stats
                    .selected_candidate_pair
                    .and_then(|pair| pair.current_round_trip_time);
                if let Some(rtt) = rtt
                    && self.flow.path_rtt(rtt)
                    && self.rtt_notes < 5
                {
                    self.rtt_notes += 1;
                    Some(PeerEvent::Note(format!(
                        "path round trip (STUN), shortest so far: {:.1} ms",
                        rtt.as_secs_f64() * 1e3
                    )))
                } else {
                    None
                }
            }
            Event::MediaEgressStats(stats) if Some(stats.mid) == self.mid => {
                // How much parity: the worst loss the other side reported in the last 10 s.
                let now = Instant::now();
                if let Some(loss) = stats.loss {
                    self.losses.push_back((now, loss));
                }
                while self.losses.front().is_some_and(|(at, _)| {
                    now.saturating_duration_since(*at) > Duration::from_secs(10)
                }) {
                    self.losses.pop_front();
                }
                let worst = self
                    .losses
                    .iter()
                    .map(|(_, loss)| f64::from(*loss))
                    .fold(0.0, f64::max);
                let fec = match self.fec_tx.as_mut() {
                    Some(sender) => {
                        sender.set_rate(fec::rate_for_loss(worst));
                        format!(
                            "; parity {:.0} %: {} for {} packets",
                            sender.rate() * 100.0,
                            sender.parity_sent,
                            sender.protected
                        )
                    }
                    None => String::new(),
                };
                let lost = stats.remote.as_ref().map_or(String::new(), |remote| {
                    format!(", {} lost by the other side's count", remote.packets_lost)
                });
                let parity_sent = self.fec_tx.as_ref().map_or(0, |sender| sender.parity_sent);
                let shaped = self.shaper.as_ref().map_or(String::new(), |shaper| {
                    format!(
                        "; bottleneck: {} passed, {} dropped (queue full), {} lost",
                        shaper.passed, shaper.dropped_full, shaper.dropped_lost
                    )
                });
                self.video_note((stats.nacks, stats.plis, parity_sent), || {
                    format!(
                        "video out: {} packets, {} NACKs and {} PLIs received{lost}{fec}{shaped}",
                        stats.packets, stats.nacks, stats.plis
                    )
                })
            }
            Event::MediaIngressStats(stats) if Some(stats.mid) == self.mid => {
                let (received, rebuilt, unrecoverable) = (
                    self.fec_rx.parity_received,
                    self.fec_rx.rebuilt,
                    self.fec_rx.unrecoverable,
                );
                let fec = if received > 0 {
                    format!(
                        "; parity: {received} came, {rebuilt} packets rebuilt, {unrecoverable} groups not completed from it"
                    )
                } else {
                    String::new()
                };
                self.video_note((stats.nacks, stats.plis, received), || {
                    format!(
                        "video in: {} packets, {} NACKs and {} PLIs sent, loss {:.1} %{fec}",
                        stats.packets,
                        stats.nacks,
                        stats.plis,
                        stats.loss.unwrap_or(0.0) * 100.0
                    )
                })
            }
            Event::KeyframeRequest(_) => Some(PeerEvent::KeyframeRequested),
            Event::EgressBitrateEstimate(
                BweKind::Twcc { estimate, .. } | BweKind::Remb { estimate, .. },
            ) => {
                // The parity goes beside the video, outside str0m's pacing: the encoder gets the rest.
                let share = self.fec_tx.as_ref().map_or(0.0, fec::Sender::rate);
                Some(PeerEvent::Bitrate(
                    (estimate.as_u64() as f64 / (1.0 + share)) as u64,
                ))
            }
            _ => None,
        };
        if let Some(event) = forward {
            let _ = self.events.send(event);
        }
        true
    }

    fn apply(&mut self, command: Command) -> Result<bool, Error> {
        match command {
            Command::Video {
                data,
                time_90khz,
                codec,
            } => {
                let Some(mid) = self.mid else {
                    return Ok(true);
                };
                let now = self.clock(Instant::now());
                let Some(writer) = self.rtc.writer(mid) else {
                    return Ok(true);
                };
                let wanted = match codec {
                    VideoCodec::H264 => str0m::format::Codec::H264,
                    VideoCodec::Av1 => str0m::format::Codec::Av1,
                    VideoCodec::H265 => str0m::format::Codec::H265,
                };
                let Some(pt) = writer
                    .payload_params()
                    .find(|params| params.spec().codec == wanted)
                    .map(|params| params.pt())
                else {
                    return Ok(true);
                };
                let time = MediaTime::from_90khz(time_90khz);
                writer.write(pt, now, time, data).map_err(err)?;
            }
            Command::Audio { data, time_48khz } => {
                let Some(mid) = self.audio_mid else {
                    return Ok(true);
                };
                let now = self.clock(Instant::now());
                let Some(writer) = self.rtc.writer(mid) else {
                    return Ok(true);
                };
                let Some(pt) = writer.payload_params().next().map(|params| params.pt()) else {
                    return Ok(true);
                };
                let time = MediaTime::new(time_48khz, Frequency::FORTY_EIGHT_KHZ);
                writer.write(pt, now, time, data).map_err(err)?;
            }
            Command::Data(bytes) => {
                // A side that does not read is not given more memory: past MAX_CONTROL_PENDING, messages are dropped.
                if self.control_pending.len() < MAX_CONTROL_PENDING {
                    self.control_pending.push_back(bytes);
                }
                self.flush_channels();
            }
            Command::StreamData(stream, bytes)
                if stream == Stream::Files && self.files_on_quic.load(Ordering::Relaxed) =>
            {
                self.in_flight[stream.index()]
                    .queued
                    .fetch_sub(bytes.len(), Ordering::Relaxed);
                if let Some(quic) = self.quic.as_mut() {
                    quic.send(&bytes);
                }
                self.pump_quic();
            }
            Command::StreamData(stream, bytes) => {
                self.stream_pending[stream.index()].push_back(bytes);
                self.flush_channels();
            }
            Command::BitrateRange { minimum, desired } => {
                self.rtc
                    .bwe()
                    .set_bitrate(Bitrate::bps(minimum), Bitrate::bps(desired));
            }
            Command::ResetBitrate(bitrate) => {
                // The estimate covers the parity too (the encoder got the rest, PeerEvent::Bitrate).
                let share = self.fec_tx.as_ref().map_or(0.0, fec::Sender::rate);
                self.rtc
                    .bwe()
                    .reset(Bitrate::bps((bitrate as f64 * (1.0 + share)) as u64));
            }
            Command::RequestKeyframe => {
                if let Some(mid) = self.mid {
                    self.ask_keyframe(mid);
                }
            }
            Command::Fec(on) => {
                // Tests (QUEQIAO_FORCE=fec:off): no parity, to measure without it.
                let off = queqiao_core::forced("fec").as_deref() == Some("off");
                self.fec_tx = (on && !off).then(fec::Sender::new);
            }
            Command::Regather(local_ip, servers) => {
                match self.regather(local_ip, servers) {
                    Ok(()) => {
                        let _ = self.events.send(PeerEvent::Regathered);
                    }
                    // Not this time (the new network has no route yet): the session tries again.
                    Err(error) => self.note(format!("gathering again failed: {error}")),
                }
            }
            Command::RestartOffer => {
                let mut api = self.rtc.sdp_api();
                // The candidates kept are this side's now (the old ones went out when it gathered again).
                api.ice_restart(true);
                match api.apply() {
                    Some((offer, pending)) => {
                        let offer =
                            punch::add_to_sdp(&offer.to_sdp_string(), &self.restart.local_nats);
                        self.restart.pending = Some((pending, offer.clone()));
                        self.note("ICE restart: offer made".into());
                        let _ = self.events.send(PeerEvent::RestartSdp(offer));
                    }
                    None => self.note("ICE restart: nothing to offer".into()),
                }
            }
            Command::RestartAnswer(answer) => match self.restart.pending.take() {
                Some((pending, offer)) => match SdpAnswer::from_sdp_string(&answer) {
                    Ok(sdp) => match self.rtc.sdp_api().accept_answer(pending, sdp) {
                        Ok(()) => {
                            // Until ICE has the other side again (then `Reconnected`), up to RESTART_LIMIT: from the
                            // answer on (ICE says "connected" for a moment as the offer is made).
                            self.restart
                                .disconnected_since
                                .get_or_insert_with(Instant::now);
                            self.restarted(&offer, &answer, &answer);
                        }
                        Err(error) => self.note(format!("ICE restart: answer refused: {error}")),
                    },
                    Err(error) => self.note(format!("ICE restart: answer not read: {error}")),
                },
                None => self.note("ICE restart: an answer without an offer, left out".into()),
            },
            Command::AcceptRestart(offer) => {
                self.restart
                    .disconnected_since
                    .get_or_insert_with(Instant::now);
                self.accept_restart(offer);
            }
            Command::Close => {
                self.rtc.disconnect();
                if let Some(turn) = self.turn.as_mut() {
                    turn.close(Instant::now());
                }
                self.flush_turn();
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// A datagram from the TURN server: a peer's data for str0m, or the TURN client's own business.
    fn handle_turn_packet(&mut self, packet: &[u8]) -> Result<(), Error> {
        let now = self.clock(Instant::now());
        let Some(turn) = self.turn.as_mut() else {
            return Ok(());
        };
        let incoming = turn.handle(packet, now);
        let relayed = turn.relayed;
        self.flush_turn();
        if let (Incoming::Data { peer, payload }, Some(relayed)) = (incoming, relayed) {
            self.dtls_note(false, &payload, relayed, peer, true);
            // As on the direct sockets (parity included).
            self.deliver(now, peer, relayed, &payload)?;
        }
        Ok(())
    }

    /// Before connecting: a note for each DTLS packet either way (at most 12 each), to see where the handshake goes.
    fn dtls_note(
        &mut self,
        outgoing: bool,
        bytes: &[u8],
        local: SocketAddr,
        remote: SocketAddr,
        relayed: bool,
    ) {
        // DTLS records only (content types 20 to 23; SRTP starts at 128), until connected.
        if self.connected || !bytes.first().is_some_and(|byte| (20..=23).contains(byte)) {
            return;
        }
        let count = if outgoing {
            &mut self.dtls_notes.0
        } else {
            &mut self.dtls_notes.1
        };
        if *count >= 12 {
            return;
        }
        *count += 1;
        let how = if relayed { " (relayed)" } else { "" };
        let records = dtls_records(bytes);
        let note = if outgoing {
            format!(
                "DTLS out {} bytes [{records}], {local} -> {remote}{how}",
                bytes.len()
            )
        } else {
            format!(
                "DTLS in {} bytes [{records}], {remote} -> {local}{how}",
                bytes.len()
            )
        };
        let _ = self.events.send(PeerEvent::Note(note));
    }

    /// Writes queued data-channel messages, in order, as far as str0m's buffer takes them (a refused write is
    /// kept and tried again later, so nothing is lost or reordered; messages wait for their channel to open).
    fn flush_channels(&mut self) {
        flush_channel(
            &mut self.rtc,
            self.channel,
            &mut self.control_pending,
            None,
            None,
        );
        // This side's control data unacknowledged for over a second (SCTP's buffer for the channel not empty): noted
        // once a pause (the agent sends on it all the time, so its buffer is seldom empty: no note when it is again).
        if let Some(id) = self.channel
            && let Some(mut channel) = self.rtc.channel(id)
        {
            let buffered = channel.buffered_amount();
            let now = Instant::now();
            match (buffered, self.control_unacked) {
                (0, _) => self.control_unacked = None,
                (_, None) => self.control_unacked = Some((now, false)),
                (bytes, Some((since, false))) if now - since > Duration::from_secs(1) => {
                    self.note(format!(
                        "control channel: {bytes} bytes this side sent are unacknowledged for {} ms",
                        (now - since).as_millis()
                    ));
                    self.control_unacked = Some((since, true));
                }
                _ => {}
            }
        }
        let mut acked = 0u64;
        for stream in Stream::ALL {
            let index = stream.index();
            flush_channel(
                &mut self.rtc,
                self.streams[index],
                &mut self.stream_pending[index],
                Some(&self.in_flight[index].queued),
                Some(&mut self.written[index]),
            );
            let mut buffered = 0;
            if let Some(id) = self.streams[index]
                && let Some(mut channel) = self.rtc.channel(id)
            {
                buffered = channel.buffered_amount();
                self.in_flight[index]
                    .buffered
                    .store(buffered, Ordering::Relaxed);
            }
            acked += self.written[index].saturating_sub(buffered as u64);
        }
        // What the other side acknowledged sets how much the streams may have in flight.
        self.flow.update(Instant::now(), acked);
        self.flow_cap.store(self.flow.cap(), Ordering::Relaxed);
    }

    /// Runs QUIC: its datagrams out over the selected pair, its events to the peer. When it next needs running.
    fn pump_quic(&mut self) -> Option<Instant> {
        let quic = self.quic.as_mut()?;
        let (datagrams, events) = quic.poll(Instant::now());
        let queued = quic.queued();
        let next = quic.next_timeout();
        if let Some((source, destination)) = self.quic_pair {
            for datagram in &datagrams {
                self.send_datagram(source, destination, datagram);
            }
        }
        if self.files_on_quic.load(Ordering::Relaxed) {
            self.in_flight[Stream::Files.index()]
                .buffered
                .store(queued, Ordering::Relaxed);
        }
        for event in events {
            match event {
                QuicEvent::Open => {
                    self.files_on_quic.store(true, Ordering::Relaxed);
                    let _ = self
                        .events
                        .send(PeerEvent::Note("files: QUIC connected".into()));
                    let _ = self.events.send(PeerEvent::StreamOpen(Stream::Files));
                }
                QuicEvent::Message(message) => {
                    let _ = self
                        .events
                        .send(PeerEvent::StreamData(Stream::Files, message));
                }
                QuicEvent::Failed(reason) => {
                    // The files go over their data channel from now on (open already, or when it opens).
                    let was_on_quic = self.files_on_quic.swap(false, Ordering::Relaxed);
                    self.quic = None;
                    self.in_flight[Stream::Files.index()]
                        .buffered
                        .store(0, Ordering::Relaxed);
                    let _ = self.events.send(PeerEvent::Note(format!(
                        "files: QUIC {} ({reason}); over SCTP from now on",
                        if was_on_quic {
                            "ended"
                        } else {
                            "did not connect"
                        }
                    )));
                    if self.streams[Stream::Files.index()].is_some() {
                        let _ = self.events.send(PeerEvent::StreamOpen(Stream::Files));
                    }
                    return None;
                }
            }
        }
        next
    }

    fn flush_turn(&mut self) {
        if let Some(turn) = self.turn.as_mut() {
            for datagram in turn.take_outbox() {
                let _ = self.socket.send_to(&datagram, turn.server);
            }
            for note in turn.take_notes() {
                let _ = self.events.send(PeerEvent::Note(note));
            }
        }
    }

    /// `at`, or the latest time already given to str0m if that is later.
    fn clock(&mut self, at: Instant) -> Instant {
        self.clock = self.clock.max(at);
        self.clock
    }

    fn note(&self, text: String) {
        let _ = self.events.send(PeerEvent::Note(text));
    }

    /// The main socket failed (its address went away): told once as a network change.
    fn main_socket_failed(&mut self, error: &str) {
        if !self.restart.main_gone {
            self.restart.main_gone = true;
            self.note(format!(
                "this PC's network changed: the session's socket on {} failed ({error})",
                self.local
            ));
            let _ = self.events.send(PeerEvent::NetworkChanged);
        }
    }

    /// Every `NETWORK_CHECK`: whether the main socket's address is still this PC's (a new network takes it away; a
    /// socket bound to it then cannot be made); told once ([`PeerEvent::NetworkChanged`]). Tests:
    /// QUEQIAO_FORCE=move:<seconds> acts as if the network changed that long after the session started. Ends the
    /// session (an error) once ICE has been without the other side for [`RESTART_LIMIT`].
    fn check_network(&mut self, started: Instant) -> Result<(), Error> {
        let now = Instant::now();
        if !self.report.sent && now >= self.report.started + REPORT_RELAYED_AFTER {
            self.report_path(None);
        }
        if let Some(since) = self.restart.disconnected_since
            && now >= since + RESTART_LIMIT
        {
            return Err(Error(format!(
                "ICE disconnected: no restart brought the other side back in {} s",
                RESTART_LIMIT.as_secs()
            )));
        }
        if now < self.restart.next_check || self.restart.main_gone {
            return Ok(());
        }
        self.restart.next_check = now + NETWORK_CHECK;
        let ip = self.local.ip();
        let gone = !ip.is_unspecified() && UdpSocket::bind((ip, 0)).is_err();
        let moved = self.restart.moves == 0
            && queqiao_core::forced("move")
                .and_then(|seconds| seconds.parse::<u64>().ok())
                .is_some_and(|seconds| now >= started + Duration::from_secs(seconds));
        if gone || moved {
            self.restart.main_gone = true;
            self.note(if gone {
                format!("this PC's network changed: {ip} is no longer one of its addresses")
            } else {
                "this PC's network changed (QUEQIAO_FORCE=move)".into()
            });
            let _ = self.events.send(PeerEvent::NetworkChanged);
        }
        Ok(())
    }

    /// The viewer's report of the session's path, once: its first direct path (`direct`: this side's address, the
    /// other's), or none after `REPORT_RELAYED_AFTER`. Only kinds and a duration go in it.
    fn report_path(&mut self, direct: Option<(SocketAddr, SocketAddr)>) {
        if self.report.sent || self.restart.role != Role::Viewer {
            return;
        }
        self.report.sent = true;
        let method = match direct {
            None => "relay",
            Some((local, _)) if self.report.sockets.contains(&local) => "punch-socket",
            Some((_, remote)) if self.report.found.contains(&remote) => "punch",
            Some(_) => "ice",
        };
        let ms = match direct {
            Some(_) => self.report.started.elapsed().as_millis().min(600_000) as u32,
            None => 0,
        };
        let _ = self.events.send(PeerEvent::PathReport(PathReport {
            local_nat: Nat::kind(self.restart.local_nats.first()).into(),
            remote_nat: self.report.remote_nat.into(),
            direct: direct.is_some(),
            method: method.into(),
            ms,
        }));
    }

    /// Gathers again on the networks this PC has now (after a network change): the old candidates leave ICE, the old
    /// uplinks' threads end, the old relay allocation is given back; the new sockets, candidates and NATs take their
    /// place (the wake-ups go to the new main socket).
    fn regather(&mut self, local_ip: IpAddr, servers: IceServers) -> Result<(), Error> {
        let gathering = gather_all(
            local_ip,
            &servers,
            self.restart.addresses,
            self.restart.relay_only,
        )?;
        for candidate in self.restart.local_candidates.drain(..) {
            self.rtc.direct_api().invalidate_candidate(&candidate);
        }
        self.stop_extras.store(true, Ordering::Relaxed);
        self.stop_extras = Arc::new(AtomicBool::new(false));
        if let Some(turn) = self.turn.as_mut() {
            turn.close(Instant::now());
        }
        self.flush_turn();
        let (waker, wake_to, wake_from) = waker_for(gathering.bind_ip, gathering.local.port())?;
        self.wake.set(waker, wake_to);
        self.wake_from = wake_from;
        self.socket = gathering.socket;
        self.local = gathering.local;
        self.turn = gathering.turn;
        self.extras = gathering.extras;
        for extra in &self.extras {
            let (socket, local, wake, arrivals, stop) = (
                extra.socket.clone(),
                extra.local,
                self.wake.clone(),
                self.arrivals.clone(),
                self.stop_extras.clone(),
            );
            let _ = std::thread::Builder::new()
                .name("queqiao-net-in".into())
                .spawn(move || receive_extra(socket, local, wake, arrivals, stop));
        }
        for candidate in &gathering.candidates {
            self.rtc.add_local_candidate(candidate.clone());
        }
        self.relay_names = servers
            .turn
            .iter()
            .chain(&servers.backups)
            .filter(|server| !server.name.is_empty())
            .map(|server| (server.address.ip(), server.name.clone()))
            .collect();
        self.restart.local_candidates = gathering.candidates;
        self.restart.local_nats = gathering.nats;
        self.restart.servers = servers;
        self.restart.main_gone = false;
        self.restart.moves += 1;
        // Punching keeps nothing of the old network: it starts again with the restart's answer.
        if let Some(punch) = self.punch.as_mut() {
            punch.stop();
        }
        self.note(format!("gathered again on {}", self.local));
        for note in gathering.notes {
            self.note(note);
        }
        Ok(())
    }

    /// The agent: the viewer's restart offer, answered (the answer goes to the session as `RestartSdp`, with this
    /// side's NATs when the viewer sent its own).
    fn accept_restart(&mut self, offer: String) {
        let sdp = match SdpOffer::from_sdp_string(&offer) {
            Ok(sdp) => sdp,
            Err(error) => return self.note(format!("ICE restart: offer not read: {error}")),
        };
        match self.rtc.sdp_api().accept_offer(sdp) {
            Ok(answer) => {
                let answer = answer.to_sdp_string();
                let answer = if punch::from_sdp(&offer).is_empty() {
                    answer
                } else {
                    punch::add_to_sdp(&answer, &self.restart.local_nats)
                };
                let _ = self.events.send(PeerEvent::RestartSdp(answer.clone()));
                self.restarted(&offer, &answer, &offer);
            }
            Err(error) => self.note(format!("ICE restart: offer refused: {error}")),
        }
    }

    /// ICE started again (`offer` and `answer` of the restart, `remote` the other side's): what the other side offers
    /// now (its candidates, relays and NATs), and punching anew with the new key.
    fn restarted(&mut self, offer: &str, answer: &str, remote: &str) {
        self.stall.restarted(Instant::now());
        self.remote_relays = relayed_candidates(remote);
        self.remote_candidates = candidates_of(remote, None);
        let remote_nats = punch::from_sdp(remote);
        self.report.remote_nat = Nat::kind(remote_nats.first());
        self.path = None;
        self.note(format!(
            "ICE restarted: the other side offers {} candidates",
            self.remote_candidates.len()
        ));
        self.punch = match punch::Key::from_sdps(offer, answer) {
            Some(key)
                if !self.restart.relay_only
                    && !self.restart.local_nats.is_empty()
                    && !remote_nats.is_empty()
                    && self.restart.servers.punch.enabled
                    && queqiao_core::forced("punch").as_deref() != Some("off") =>
            {
                let mut pairs = Vec::new();
                for local in &self.restart.local_nats {
                    for remote in &remote_nats {
                        pairs.push((*local, *remote));
                    }
                }
                Some(
                    Puncher::rounds(
                        key,
                        pairs,
                        self.restart.role == Role::Viewer,
                        Instant::now(),
                    )
                    .limit_rounds(self.restart.servers.punch.rounds),
                )
            }
            _ => None,
        };
    }

    /// The socket a datagram from `local` leaves by: an extra socket's, else the main one.
    fn socket_for(&self, local: SocketAddr) -> &UdpSocket {
        self.extras
            .iter()
            .find(|extra| extra.local == local)
            .map_or(&self.socket, |extra| &extra.socket)
    }

    /// Punching's turn: what is due leaves, what its new sockets received is handled; when to come back (none while the
    /// session is direct: punching waits, and goes on with a new round if the path is relayed again).
    fn punch_tick(&mut self) -> Result<Option<Instant>, Error> {
        let direct = self
            .path
            .as_deref()
            .is_some_and(|path| path.starts_with("direct"));
        let Some(punch) = self.punch.as_mut() else {
            return Ok(None);
        };
        if punch.stopped() {
            return Ok(None);
        }
        if direct {
            punch.pause();
        } else if punch.paused() {
            punch.resume(Instant::now());
        }
        let mut out = Vec::new();
        let next = punch.poll(Instant::now(), &mut out);
        let events = punch.receive();
        let notes = punch.take_notes();
        for (from, to, datagram) in out {
            if !self.cut_off(to) {
                let _ = self.socket_for(from).send_to(&datagram, to);
            }
        }
        for note in notes {
            let _ = self.events.send(PeerEvent::Note(note));
        }
        for event in events {
            match event {
                punch::Event::Found {
                    local,
                    remote,
                    socket,
                } => self.punch_found(local, remote, socket),
                punch::Event::Deliver {
                    local,
                    source,
                    contents,
                    socket,
                } => {
                    self.adopt(local, socket);
                    let at = self.clock(Instant::now());
                    self.deliver(at, source, local, &contents)?;
                }
            }
        }
        Ok(next)
    }

    /// A punch packet on one of the session's sockets: answered when it is a request (also after punching ended: the
    /// other side may still be at it); its path handed to ICE once.
    fn on_punch(&mut self, local: SocketAddr, source: SocketAddr, datagram: &[u8]) {
        let Some(punch) = self.punch.as_mut() else {
            return;
        };
        let (answer, new) = punch.on_packet(local, source, datagram);
        if let Some(answer) = answer
            && !self.cut_off(source)
        {
            let _ = self.socket_for(local).send_to(&answer, source);
        }
        if new {
            self.punch_found(local, source, None);
        }
    }

    /// A path punching found (`remote` reached `local`, or answered there): the other side's address for ICE as a
    /// candidate (unless its SDP had it already), and a new socket as one of this side's.
    fn punch_found(
        &mut self,
        local: SocketAddr,
        remote: SocketAddr,
        socket: Option<Arc<UdpSocket>>,
    ) {
        if let Some(socket) = socket {
            self.report.sockets.insert(local);
            self.adopt(local, socket);
        }
        self.report.found.insert(remote);
        let known = self.remote_candidates.contains(&remote);
        let _ = self.events.send(PeerEvent::Note(format!(
            "punching: {remote} reached {local}, a direct path{}",
            if known {
                " (ICE has that address already)"
            } else {
                ": handed to ICE"
            }
        )));
        if !known {
            self.remote_candidates.push(remote);
            match Candidate::server_reflexive(remote, remote, "udp") {
                Ok(candidate) => self.rtc.add_remote_candidate(candidate),
                Err(error) => {
                    let _ = self.events.send(PeerEvent::Note(format!(
                        "punching: {remote} not usable: {error}"
                    )));
                }
            }
        }
    }

    /// A new socket punching found a path on becomes one of the session's: a receiving thread as the other uplinks'
    /// sockets have, and a host candidate for ICE.
    fn adopt(&mut self, local: SocketAddr, socket: Arc<UdpSocket>) {
        if self.extras.iter().any(|extra| extra.local == local) {
            return;
        }
        let _ = socket.set_nonblocking(false);
        let (thread_socket, wake, arrivals, stop) = (
            socket.clone(),
            self.wake.clone(),
            self.arrivals.clone(),
            self.stop_extras.clone(),
        );
        let _ = std::thread::Builder::new()
            .name("queqiao-net-in".into())
            .spawn(move || receive_extra(thread_socket, local, wake, arrivals, stop));
        self.extras.push(Extra { socket, local });
        if let Ok(candidate) = Candidate::host(local, "udp") {
            // Kept with the others: a network change takes it out of ICE too.
            self.restart.local_candidates.push(candidate.clone());
            self.rtc.add_local_candidate(candidate);
        }
    }

    /// Tests (QUEQIAO_FORCE=cut): whether a datagram to or from `remote` is left out now: a direct path (not one of
    /// the other side's relay addresses; this side's TURN server never comes here) within the cut's window.
    fn cut_off(&self, remote: SocketAddr) -> bool {
        self.cut.is_some_and(|(from, to)| {
            let now = Instant::now();
            now >= from && now < to && !self.remote_relays.contains(&remote)
        })
    }

    /// Sends a datagram from `source` (our address: a socket of ours, or the TURN relay's) to `destination`;
    /// the TURN server's address when it went through it.
    fn send_datagram(
        &mut self,
        source: SocketAddr,
        destination: SocketAddr,
        contents: &[u8],
    ) -> Option<SocketAddr> {
        // Tests: through the bottleneck (QUEQIAO_FORCE=shape:...), sent when it has carried the datagram.
        if let Some(shaper) = self.shaper.as_mut() {
            shaper.offer(Instant::now(), source, destination, contents);
            return match self.turn.as_ref() {
                Some(turn) if turn.relayed == Some(source) => Some(turn.server),
                _ => None,
            };
        }
        self.send_now(source, destination, contents)
    }

    /// The datagrams the test bottleneck has carried, sent now.
    fn release_shaped(&mut self) {
        let due = match self.shaper.as_mut() {
            Some(shaper) => shaper.due(Instant::now()),
            None => return,
        };
        for (source, destination, datagram) in due {
            self.send_now(source, destination, &datagram);
        }
    }

    fn send_now(
        &mut self,
        source: SocketAddr,
        destination: SocketAddr,
        contents: &[u8],
    ) -> Option<SocketAddr> {
        match self.turn.as_mut() {
            Some(turn) if turn.relayed == Some(source) => {
                turn.send(destination, contents, Instant::now());
                Some(turn.server)
            }
            _ => {
                if self.cut_off(destination) {
                    return None;
                }
                let socket = self
                    .extras
                    .iter()
                    .find(|extra| extra.local == source)
                    .map_or(&self.socket, |extra| &extra.socket);
                let _ = socket.send_to(contents, destination);
                None
            }
        }
    }

    /// The parity datagrams to send after `datagram` (a video packet leaving, when parity is on).
    fn video_parity(&mut self, datagram: &[u8]) -> Vec<Vec<u8>> {
        if self.fec_tx.is_none() {
            return Vec::new();
        }
        let Some(header) = fec::rtp(datagram) else {
            return Vec::new();
        };
        if self.video_pts.is_empty()
            && let Some(mid) = self.mid
            && let Some(writer) = self.rtc.writer(mid)
        {
            self.video_pts = writer.payload_params().map(|params| *params.pt()).collect();
        }
        if !self.video_pts.contains(&header.pt) {
            return Vec::new();
        }
        match self.fec_tx.as_mut() {
            Some(sender) => sender.sent(datagram, &header, Instant::now()),
            None => Vec::new(),
        }
    }

    /// A datagram that came (from any of our sockets or through TURN): parity goes to the FEC receiver, which may
    /// rebuild lost video packets; the rest to str0m, as do the rebuilt packets. What is not STUN, DTLS or RTP is
    /// left out (anyone can send to these ports): the session goes on.
    fn deliver(
        &mut self,
        at: Instant,
        source: SocketAddr,
        destination: SocketAddr,
        datagram: &[u8],
    ) -> Result<(), Error> {
        let now = Instant::now();
        if datagram.first() == Some(&quic::MAGIC) {
            if let Some(quic) = self.quic.as_mut() {
                quic.receive(now, &datagram[1..]);
                self.pump_quic();
            }
            return Ok(());
        }
        let rebuilt = if datagram.first() == Some(&fec::MAGIC) {
            self.fec_rx.parity(datagram, now)
        } else {
            let rebuilt = match fec::rtp(datagram) {
                Some(header) => self.fec_rx.media(datagram, &header, now),
                None => Vec::new(),
            };
            self.hand_to_rtc(at, source, destination, datagram)?;
            rebuilt
        };
        for datagram in rebuilt {
            self.hand_to_rtc(at, source, destination, &datagram)?;
        }
        Ok(())
    }

    fn hand_to_rtc(
        &mut self,
        at: Instant,
        source: SocketAddr,
        destination: SocketAddr,
        datagram: &[u8],
    ) -> Result<(), Error> {
        let Ok(contents) = str0m::net::DatagramRecv::try_from(datagram) else {
            return Ok(());
        };
        self.stall.heard(destination, source, Instant::now());
        let input = Input::Receive(
            at,
            Receive {
                proto: Protocol::Udp,
                source,
                destination,
                contents,
            },
        );
        self.rtc.handle_input(input).map_err(err)
    }

    /// A note on the video's counters when they changed (NACKs, PLIs, parity), at most every 5 s.
    fn video_note(
        &mut self,
        counters: (u64, u64, u64),
        text: impl FnOnce() -> String,
    ) -> Option<PeerEvent> {
        let (nacks, plis, parity, at) = self.video_noted;
        if counters == (nacks, plis, parity)
            || at.is_some_and(|at| at.elapsed() < Duration::from_secs(5))
        {
            return None;
        }
        self.video_noted = (counters.0, counters.1, counters.2, Some(Instant::now()));
        Some(PeerEvent::Note(text()))
    }

    fn ask_keyframe(&mut self, mid: Mid) {
        let now = Instant::now();
        if self
            .last_keyframe_request
            .is_some_and(|last| now - last < KEYFRAME_REQUEST_GAP)
        {
            return;
        }
        if let Some(mut writer) = self.rtc.writer(mid) {
            let _ = writer.request_keyframe(None, KeyframeRequestKind::Pli);
            self.last_keyframe_request = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_selected_path_that_carries_nothing_is_told_once() {
        use std::time::{Duration, Instant};
        let (local, remote, other): (
            std::net::SocketAddr,
            std::net::SocketAddr,
            std::net::SocketAddr,
        ) = (
            "192.0.2.1:4000".parse().unwrap(),
            "198.51.100.2:5000".parse().unwrap(),
            "203.0.113.3:3478".parse().unwrap(),
        );
        let start = Instant::now();
        let mut stall = super::Stall::new();
        assert_eq!(
            stall.due(start + Duration::from_secs(60)),
            None,
            "nothing selected"
        );
        stall.select(local, remote, start);
        // Heard every second: never due.
        for second in 1..10 {
            stall.heard(local, remote, start + Duration::from_secs(second));
            assert_eq!(stall.due(start + Duration::from_secs(second)), None);
        }
        // Then only another pair carries (a relay's checks): 5 s after the last datagram on this one, once.
        stall.heard(local, other, start + Duration::from_secs(12));
        assert_eq!(
            stall.next(),
            Some(start + Duration::from_secs(9) + super::STALL_AFTER)
        );
        assert_eq!(stall.due(start + Duration::from_millis(13_900)), None);
        assert_eq!(
            stall.due(start + Duration::from_secs(14)),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            stall.due(start + Duration::from_secs(20)),
            None,
            "told once"
        );
        // ICE started again, or another pair selected: watched afresh.
        stall.restarted(start + Duration::from_secs(20));
        assert_eq!(
            stall.due(start + Duration::from_secs(25)),
            Some(Duration::from_secs(5))
        );
        stall.select(local, other, start + Duration::from_secs(26));
        assert_eq!(stall.due(start + Duration::from_secs(30)), None);
        assert!(stall.due(start + Duration::from_secs(31)).is_some());
    }

    #[test]
    fn the_cut_of_the_direct_paths_is_read_from_its_switch() {
        use std::time::Duration;
        assert_eq!(
            super::cut_window("25-55"),
            Some((Duration::from_secs(25), Duration::from_secs(55)))
        );
        assert_eq!(
            super::cut_window("0.5-1.5"),
            Some((Duration::from_millis(500), Duration::from_millis(1500)))
        );
        for nonsense in ["55-25", "10-10", "-5-10", "x-10", "10", ""] {
            assert_eq!(super::cut_window(nonsense), None, "{nonsense}");
        }
    }

    #[test]
    fn the_other_sides_relays_are_read_from_its_sdp() {
        let sdp = "v=0\r\n\
            a=candidate:1 1 udp 2130706431 172.20.10.2 58077 typ host\r\n\
            a=candidate:2 1 udp 1694498815 223.104.42.214 12934 typ srflx raddr 172.20.10.2 rport 58077\r\n\
            a=candidate:3 1 udp 16777215 101.201.221.56 47223 typ relay raddr 223.104.42.214 rport 12934\r\n\
            a=candidate:4 1 udp 16777214 2001:db8::1 47224 typ relay\r\n\
            a=candidate:5 1 udp 16777213 not-an-address 1 typ relay\r\n";
        let relays = super::relayed_candidates(sdp);
        assert_eq!(
            relays,
            vec![
                "101.201.221.56:47223".parse().unwrap(),
                "[2001:db8::1]:47224".parse().unwrap()
            ]
        );
        assert!(super::relayed_candidates("v=0\r\n").is_empty());
    }
}
