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

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use queqiao_core::protocol::VideoCodec;
use str0m::bwe::{Bitrate, BweKind};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::media::{Direction, Frequency, KeyframeRequestKind, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

use crate::gather::{ExtraDeadline, gather, gather_extra};
use crate::signal::Signaling;
use crate::turn::{Incoming, TurnClient};
use crate::{Error, err};

/// Which of this PC's addresses a session offers: all its uplinks (the account's own devices), or only the one
/// the server connection uses (someone helping from another account does not learn the others).
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
    BitrateRange {
        minimum: u64,
        desired: u64,
    },
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
    waker: Arc<UdpSocket>,
    wake_to: SocketAddr,
    thread: Option<JoinHandle<()>>,
    notes: Vec<String>,
    in_flight: Arc<[InFlight; STREAMS]>,
}

/// Bulk data with a data channel of its own, so it never delays input or control messages, and with flow
/// control: a sender keeps at most [`STREAM_IN_FLIGHT`] bytes in flight on each.
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
/// buffer).
pub const STREAM_IN_FLIGHT: usize = 1024 * 1024;

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
            // 1 ms timer resolution for this process: str0m's timers and pacing need it.
            unsafe { windows::Win32::Media::timeBeginPeriod(1) };
        });
        let local_ip = signal.local_ip()?;
        // Bound to the address the server connection leaves from: a socket on "any" would send out of whatever
        // interface the route picks, a VPN's or proxy's tunnel included (see `interfaces`).
        let bind_ip = match local_ip {
            IpAddr::V4(ip) if !ip.is_unspecified() => ip,
            _ => Ipv4Addr::UNSPECIFIED,
        };
        let socket = bind_socket(bind_ip)?;
        let port = socket.local_addr()?.port();
        let local = SocketAddr::new(local_ip, port);
        let servers = signal.ice_servers()?;
        let relay_only = queqiao_core::forced("network").as_deref() == Some("relay");
        let extras: Vec<Extra> =
            if addresses == Addresses::All && !relay_only && !bind_ip.is_unspecified() {
                crate::interfaces::uplinks()
                    .into_iter()
                    .filter(|ip| *ip != bind_ip)
                    .filter_map(|ip| {
                        let socket = bind_socket(ip).ok()?;
                        let local = socket.local_addr().ok()?;
                        Some(Extra {
                            socket: Arc::new(socket),
                            local,
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            };
        let stun = servers
            .stun
            .or(servers.turn.as_ref().map(|turn| turn.address));
        let deadline = ExtraDeadline::new(Instant::now());
        let (gathered, found) = std::thread::scope(|scope| {
            let jobs: Vec<_> = extras
                .iter()
                .map(|extra| {
                    let deadline = &deadline;
                    scope.spawn(move || gather_extra(&extra.socket, extra.local, stun, deadline))
                })
                .collect();
            let gathered = gather(&socket, local, &servers, relay_only);
            deadline.main_done();
            let found: Vec<_> = jobs.into_iter().filter_map(|job| job.join().ok()).collect();
            (gathered, found)
        });
        let gathered = gathered?;
        let mut notes = gathered.notes;
        let mut rtc = tune(
            RtcConfig::new()
                .clear_codecs()
                .enable_h264(true)
                .enable_av1(true)
                .enable_opus(true, false),
            Bitrate::bps(START_BITRATE),
        )
        .build(Instant::now());
        for candidate in gathered.candidates {
            rtc.add_local_candidate(candidate);
        }
        for (candidates, extra_notes) in found {
            for candidate in candidates {
                rtc.add_local_candidate(candidate);
            }
            notes.extend(extra_notes);
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
                signal.send("offer", &offer.to_sdp_string())?;
                let answer = SdpAnswer::from_sdp_string(&signal.expect("answer")?).map_err(err)?;
                rtc.sdp_api().accept_answer(pending, answer).map_err(err)?;
            }
            Role::Agent => {
                let offer = SdpOffer::from_sdp_string(&signal.expect("offer")?).map_err(err)?;
                let answer = rtc.sdp_api().accept_offer(offer).map_err(err)?;
                signal.send("answer", &answer.to_sdp_string())?;
            }
        }

        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        // The wake-up goes to the main socket, which listens on its own address only.
        let wake_ip = if bind_ip.is_unspecified() {
            Ipv4Addr::LOCALHOST
        } else {
            bind_ip
        };
        let waker = Arc::new(UdpSocket::bind((wake_ip, 0))?);
        let wake_to = SocketAddr::new(wake_ip.into(), port);
        let wake_from = waker.local_addr()?;
        let (arrivals, arrived) = mpsc::channel();
        let stop_extras = Arc::new(AtomicBool::new(false));
        for extra in &extras {
            let socket = extra.socket.clone();
            let waker = waker.clone();
            let (local, arrivals, stop) = (extra.local, arrivals.clone(), stop_extras.clone());
            std::thread::Builder::new()
                .name("queqiao-net-in".into())
                .spawn(move || receive_extra(socket, local, waker, wake_to, arrivals, stop))?;
        }
        let in_flight: Arc<[InFlight; STREAMS]> = Arc::new(Default::default());
        let mut network = Network {
            rtc,
            socket,
            local,
            wake_from,
            mid,
            audio_mid,
            channel,
            events: event_sender,
            commands: command_receiver,
            last_keyframe_request: None,
            turn: gathered.turn,
            extras,
            arrived,
            stop_extras,
            connected: false,
            dtls_notes: (0, 0),
            path: None,
            clock: Instant::now(),
            streams: [None; STREAMS],
            in_flight: in_flight.clone(),
            control_pending: VecDeque::new(),
            stream_pending: Default::default(),
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
            waker,
            wake_to,
            thread: Some(thread),
            notes,
            in_flight,
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

    /// How many more bytes may be sent on `stream` now ([`STREAM_IN_FLIGHT`] minus what is in flight).
    pub fn room(&self, stream: Stream) -> usize {
        let counters = &self.in_flight[stream.index()];
        let in_flight =
            counters.queued.load(Ordering::Relaxed) + counters.buffered.load(Ordering::Relaxed);
        STREAM_IN_FLIGHT.saturating_sub(in_flight)
    }

    /// The bitrate range the sender wants: the bandwidth estimate probes up to `desired` (str0m's BWE).
    pub fn set_bitrate_range(&self, minimum: u64, desired: u64) {
        self.command(Command::BitrateRange { minimum, desired });
    }

    /// Asks the sender for a keyframe (the viewer does this by itself after a loss).
    pub fn request_keyframe(&self) {
        self.command(Command::RequestKeyframe);
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
            waker: self.waker.clone(),
            wake_to: self.wake_to,
        })
    }

    fn command(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            let _ = self.waker.send_to(&[0], self.wake_to);
        }
    }
}

/// Sends Opus packets into the session from any thread; see [`Peer::audio_sender`].
pub struct AudioSender {
    commands: Sender<Command>,
    waker: Arc<UdpSocket>,
    wake_to: SocketAddr,
}

impl AudioSender {
    /// One Opus packet; `time_48khz` counts samples at 48 kHz (gaps for silence are fine).
    pub fn send(&self, data: Vec<u8>, time_48khz: u64) {
        if self
            .commands
            .send(Command::Audio { data, time_48khz })
            .is_ok()
        {
            let _ = self.waker.send_to(&[0], self.wake_to);
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

/// str0m settings for desktop video (tests/loss_recovery.rs measures them on a lossy link):
/// - hold up to 2048 video packets for reordering and repair (str0m: "at least as big as the number of packets the
///   biggest keyframe can be split over"; its default of 30 gives up on a 1440p keyframe at the first loss, which
///   costs a new keyframe, a bigger burst and more loss);
/// - keep the last 4096 sent video packets for retransmission (NACK), so a keyframe can still be repaired;
/// - bandwidth estimation (transport-wide congestion control) from `start`, which also paces the packets out
///   instead of sending a keyframe as one burst;
/// - 4 MB of data-channel buffer (str0m's default of 128 kB refuses writes beyond it, which dropped file chunks)
///   and messages up to 1 MiB (the default 256 KiB is just below a full clipboard message).
pub fn tune(config: RtcConfig, start: Bitrate) -> RtcConfig {
    config
        .set_reordering_size_video(2048)
        .set_send_buffer_video(4096)
        .enable_bwe(Some(start))
        .set_sctp_max_buffered_amount(4 << 20)
        .set_sctp_max_message_size(1 << 20)
}

/// Writes `pending` into channel `id` until str0m refuses (buffer full) or the queue is empty; `queued` counts the
/// bytes still waiting.
fn flush_channel(
    rtc: &mut Rtc,
    id: Option<ChannelId>,
    pending: &mut VecDeque<Vec<u8>>,
    queued: Option<&AtomicUsize>,
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
    waker: Arc<UdpSocket>,
    wake_to: SocketAddr,
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
                let _ = waker.send_to(&[0], wake_to);
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

fn bind_socket(ip: Ipv4Addr) -> Result<UdpSocket, Error> {
    use socket2::{Domain, Protocol as SocketProtocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(SocketProtocol::UDP))?;
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
    stream_pending: [VecDeque<Vec<u8>>; STREAMS],
    events: Sender<PeerEvent>,
    commands: Receiver<Command>,
    last_keyframe_request: Option<Instant>,
    turn: Option<TurnClient>,
    /// The other uplinks' sockets, what they received, and the flag that ends their threads.
    extras: Vec<Extra>,
    arrived: Receiver<Arriving>,
    stop_extras: Arc<AtomicBool>,
    connected: bool,
    /// DTLS packets noted before connecting (out, in): where the handshake goes and how long it takes.
    dtls_notes: (u32, u32),
    path: Option<String>,
    /// The latest time given to str0m: inputs never go back in time (deadlines are served up to 1 ms early).
    clock: Instant,
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
        let start = self.clock(Instant::now());
        self.rtc.handle_input(Input::Timeout(start)).map_err(err)?;
        let mut immediate = 0u32;
        loop {
            let Some(mut deadline) = self.drain()? else {
                return Ok(());
            };
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
            // What the other uplinks' sockets received.
            let mut took = false;
            while let Ok(arriving) = self.arrived.try_recv() {
                took = true;
                self.dtls_note(
                    false,
                    &arriving.contents,
                    arriving.destination,
                    arriving.source,
                    false,
                );
                let Ok(contents) = arriving.contents.as_slice().try_into() else {
                    continue;
                };
                let at = self.clock(Instant::now());
                let input = Input::Receive(
                    at,
                    Receive {
                        proto: Protocol::Udp,
                        source: arriving.source,
                        destination: arriving.destination,
                        contents,
                    },
                );
                // Not `rtc.accepts`: before ICE has a pair it refuses DTLS from a candidate not yet checked, and the
                // viewer's first ClientHello (sent as soon as its own ICE connected) was dropped here; its resend
                // came 2 s later (seen over the internet 2026-11-21). One Rtc per session: everything goes to it, as
                // on the main socket.
                self.rtc.handle_input(input).map_err(err)?;
            }
            if took {
                continue;
            }
            let deadline = match self.turn.as_ref().and_then(TurnClient::next_deadline) {
                Some(turn) => deadline.min(turn),
                None => deadline,
            };
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            self.socket.set_read_timeout(Some(wait))?;
            let input = match self.socket.recv_from(&mut buffer) {
                Ok((_, source)) if source == self.wake_from => continue,
                Ok((length, source))
                    if self.turn.as_ref().is_some_and(|turn| turn.server == source) =>
                {
                    self.handle_turn_packet(&buffer[..length])?;
                    continue;
                }
                Ok((length, source)) => {
                    self.dtls_note(false, &buffer[..length], self.local, source, false);
                    // Something that is not STUN, DTLS or RTP (anyone can send to this port): left out, the
                    // session goes on.
                    let Ok(contents) = buffer[..length].try_into() else {
                        continue;
                    };
                    Input::Receive(
                        self.clock(Instant::now()),
                        Receive {
                            proto: Protocol::Udp,
                            source,
                            destination: self.local,
                            contents,
                        },
                    )
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
                Err(error) => return Err(error.into()),
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
                    return Ok(Some(deadline));
                }
                Output::Transmit(packet) => {
                    let relayed = match self.turn.as_mut() {
                        Some(turn) if turn.relayed == Some(packet.source) => {
                            turn.send(packet.destination, &packet.contents, Instant::now());
                            Some(turn.server)
                        }
                        _ => {
                            let socket = self
                                .extras
                                .iter()
                                .find(|extra| extra.local == packet.source)
                                .map_or(&self.socket, |extra| &extra.socket);
                            let _ = socket.send_to(&packet.contents, packet.destination);
                            None
                        }
                    };
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
                        let path = match relayed {
                            Some(server) => {
                                format!("relayed through {server} to {}", packet.destination)
                            }
                            None => {
                                format!("direct to {} (from {})", packet.destination, packet.source)
                            }
                        };
                        if self.path.as_ref() != Some(&path) {
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
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                let _ = self
                    .events
                    .send(PeerEvent::Closed("ICE disconnected".into()));
                return false;
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
                    Some(PeerEvent::StreamOpen(stream))
                }
                None => {
                    self.channel = Some(id);
                    Some(PeerEvent::ChannelOpen)
                }
            },
            Event::ChannelData(data) => {
                match Stream::ALL
                    .into_iter()
                    .find(|stream| self.streams[stream.index()] == Some(data.id))
                {
                    Some(stream) => Some(PeerEvent::StreamData(stream, data.data)),
                    None => Some(PeerEvent::Data(data.data)),
                }
            }
            Event::KeyframeRequest(_) => Some(PeerEvent::KeyframeRequested),
            Event::EgressBitrateEstimate(
                BweKind::Twcc { estimate, .. } | BweKind::Remb { estimate, .. },
            ) => Some(PeerEvent::Bitrate(estimate.as_u64())),
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
                self.control_pending.push_back(bytes);
                self.flush_channels();
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
            Command::RequestKeyframe => {
                if let Some(mid) = self.mid {
                    self.ask_keyframe(mid);
                }
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
            // As on the direct sockets: what is not STUN, DTLS or RTP is left out, the session goes on.
            let Ok(contents) = payload.as_slice().try_into() else {
                return Ok(());
            };
            let input = Input::Receive(
                now,
                Receive {
                    proto: Protocol::Udp,
                    source: peer,
                    destination: relayed,
                    contents,
                },
            );
            self.rtc.handle_input(input).map_err(err)?;
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
        flush_channel(&mut self.rtc, self.channel, &mut self.control_pending, None);
        for stream in Stream::ALL {
            let index = stream.index();
            flush_channel(
                &mut self.rtc,
                self.streams[index],
                &mut self.stream_pending[index],
                Some(&self.in_flight[index].queued),
            );
            if let Some(id) = self.streams[index]
                && let Some(mut channel) = self.rtc.channel(id)
            {
                self.in_flight[index]
                    .buffered
                    .store(channel.buffered_amount(), Ordering::Relaxed);
            }
        }
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
