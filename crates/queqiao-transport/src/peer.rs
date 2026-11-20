//! A WebRTC connection between viewer and agent, run on its own thread ("queqiao-net").
//!
//! str0m is sans-IO: this thread owns the UDP socket and the `Rtc`, feeds it packets and timeouts and
//! sends what it produces, following str0m's rule: every change is followed by draining its output. The
//! application talks to the thread through channels; to make outgoing video leave at once, each command
//! also sends a one-byte "wake-up" datagram to the thread's own socket from a local socket, which ends
//! the thread's wait (a socket timeout would otherwise round up to Windows' 15.6 ms timer tick).

use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::media::{Direction, KeyframeRequestKind, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};

use crate::signal::Signal;
use crate::{Error, err};

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
    /// One whole H.264 frame (Annex B). `contiguous` is false after a loss: a keyframe was requested.
    Video {
        data: Arc<[u8]>,
        time_90khz: u64,
        contiguous: bool,
        received: Instant,
    },
    Data(Vec<u8>),
    /// The viewer lost a frame and needs a keyframe.
    KeyframeRequested,
    Closed(String),
}

enum Command {
    Video { data: Vec<u8>, time_90khz: u64 },
    Data(Vec<u8>),
    RequestKeyframe,
    Close,
}

/// Keyframe requests at most this often (UU Remote limits keyframes to one per 600 ms on loss).
const KEYFRAME_REQUEST_GAP: Duration = Duration::from_millis(600);

pub struct Peer {
    commands: Sender<Command>,
    events: Receiver<PeerEvent>,
    waker: UdpSocket,
    wake_to: SocketAddr,
    thread: Option<JoinHandle<()>>,
}

impl Peer {
    /// Negotiates over `signal` (the viewer offers, the agent answers) and starts the network thread.
    pub fn start(role: Role, signal: &mut Signal) -> Result<Peer, Error> {
        static PROCESS_SETUP: Once = Once::new();
        PROCESS_SETUP.call_once(|| {
            str0m::crypto::from_feature_flags().install_process_default();
            // 1 ms timer resolution for this process: str0m's timers and pacing need it.
            unsafe { windows::Win32::Media::timeBeginPeriod(1) };
        });
        let socket = bind_socket()?;
        let port = socket.local_addr()?.port();
        let local = SocketAddr::new(signal.local_ip()?, port);
        let mut rtc = RtcConfig::new()
            .clear_codecs()
            .enable_h264(true)
            .build(Instant::now());
        let candidate = Candidate::host(local, "udp").map_err(err)?;
        rtc.add_local_candidate(candidate);

        let (mut mid, mut channel) = (None, None);
        match role {
            Role::Viewer => {
                let mut api = rtc.sdp_api();
                mid = Some(api.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None));
                channel = Some(api.add_channel("control".to_string()));
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
        let waker = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
        let wake_to = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port);
        let wake_from = waker.local_addr()?;
        let mut network = Network {
            rtc,
            socket,
            local,
            wake_from,
            mid,
            channel,
            events: event_sender,
            commands: command_receiver,
            last_keyframe_request: None,
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
        })
    }

    /// Sends one encoded frame (Annex-B H.264). Dropped while not connected yet.
    pub fn send_video(&self, data: Vec<u8>, time_90khz: u64) {
        self.command(Command::Video { data, time_90khz });
    }

    /// Sends a message on the data channel (dropped while the channel is not open).
    pub fn send_data(&self, bytes: Vec<u8>) {
        self.command(Command::Data(bytes));
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

    fn command(&self, command: Command) {
        if self.commands.send(command).is_ok() {
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

fn bind_socket() -> Result<UdpSocket, Error> {
    use socket2::{Domain, Protocol as SocketProtocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(SocketProtocol::UDP))?;
    // A keyframe is a burst of a few hundred packets: Windows' default 64 KB buffers would drop some.
    let _ = socket.set_send_buffer_size(4 << 20);
    let _ = socket.set_recv_buffer_size(4 << 20);
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)).into())?;
    Ok(socket.into())
}

struct Network {
    rtc: Rtc,
    socket: UdpSocket,
    local: SocketAddr,
    wake_from: SocketAddr,
    mid: Option<Mid>,
    channel: Option<ChannelId>,
    events: Sender<PeerEvent>,
    commands: Receiver<Command>,
    last_keyframe_request: Option<Instant>,
}

impl Network {
    fn run(&mut self) -> Result<(), Error> {
        let mut buffer = vec![0u8; 2048];
        loop {
            let Some(deadline) = self.drain()? else {
                return Ok(());
            };
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
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            self.socket.set_read_timeout(Some(wait))?;
            let input = match self.socket.recv_from(&mut buffer) {
                Ok((_, source)) if source == self.wake_from => continue,
                Ok((length, source)) => Input::Receive(
                    Instant::now(),
                    Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: self.local,
                        contents: buffer[..length].try_into().map_err(err)?,
                    },
                ),
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    Input::Timeout(Instant::now())
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
                Output::Timeout(deadline) => return Ok(Some(deadline)),
                Output::Transmit(packet) => {
                    let _ = self.socket.send_to(&packet.contents, packet.destination);
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
            Event::Connected => Some(PeerEvent::Connected),
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                let _ = self
                    .events
                    .send(PeerEvent::Closed("ICE disconnected".into()));
                return false;
            }
            Event::Closed => return false,
            Event::MediaAdded(added) => {
                self.mid = Some(added.mid);
                None
            }
            Event::MediaData(data) => {
                if !data.contiguous {
                    self.ask_keyframe(data.mid);
                }
                Some(PeerEvent::Video {
                    data: data.data,
                    time_90khz: data.time.numer(),
                    contiguous: data.contiguous,
                    received: Instant::now(),
                })
            }
            Event::ChannelOpen(id, _) => {
                self.channel = Some(id);
                Some(PeerEvent::ChannelOpen)
            }
            Event::ChannelData(data) => Some(PeerEvent::Data(data.data)),
            Event::KeyframeRequest(_) => Some(PeerEvent::KeyframeRequested),
            _ => None,
        };
        if let Some(event) = forward {
            let _ = self.events.send(event);
        }
        true
    }

    fn apply(&mut self, command: Command) -> Result<bool, Error> {
        match command {
            Command::Video { data, time_90khz } => {
                let Some(mid) = self.mid else {
                    return Ok(true);
                };
                let Some(writer) = self.rtc.writer(mid) else {
                    return Ok(true);
                };
                let Some(pt) = writer.payload_params().next().map(|params| params.pt()) else {
                    return Ok(true);
                };
                let time = MediaTime::from_90khz(time_90khz);
                writer.write(pt, Instant::now(), time, data).map_err(err)?;
            }
            Command::Data(bytes) => {
                if let Some(id) = self.channel
                    && let Some(mut channel) = self.rtc.channel(id)
                {
                    let _ = channel.write(true, &bytes);
                }
            }
            Command::RequestKeyframe => {
                if let Some(mid) = self.mid {
                    self.ask_keyframe(mid);
                }
            }
            Command::Close => {
                self.rtc.disconnect();
                return Ok(false);
            }
        }
        Ok(true)
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
