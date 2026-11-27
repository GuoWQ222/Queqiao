//! Messages between a device (agent or viewer) and queqiao-server, as JSON over one TLS WebSocket.
//!
//! A connection starts with `Login` (user name and password) or `Resume` (a refresh token from an earlier
//! login); the server answers `Welcome`. After that the server pushes the account's device list as it
//! changes, and forwards `Signal`s between the account's own devices (the WebRTC offer and answer of a
//! session), and between two devices of different accounts only after a help code was given (`RequestHelp`).
//! Every device on an account can be both agent (controlled) and viewer (controlling).
//!
//! Each device has its own Ed25519 key; the server publishes the public keys in the device list, and every
//! signal carries the sender's signature over [`signed_bytes`], which the receiving device checks against the
//! key it pinned for the sender. So the server can route sessions but cannot change what they agree on (the
//! DTLS fingerprints in the offer and answer).

use serde::{Deserialize, Serialize};

/// Which program a connection comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Agent,
    Viewer,
}

/// One device of the account, as the device list shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// Its agent is connected: it can be controlled now.
    pub controllable: bool,
    /// Any of its programs is connected.
    pub online: bool,
    /// The device's Ed25519 public key, hex (64 characters).
    pub public_key: String,
    /// The device controlling it now (its name, as its agent reported it); empty when nobody does. One device
    /// controls a PC at a time (the user, 2026-11-26; as UU Remote): others see it, and taking control ends that one's
    /// session. Empty from servers before it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub controlled_by: String,
}

/// What a server can do beyond the first protocol, listed in `Welcome` (programs send those messages only to a
/// server that names them).
pub mod features {
    /// `ControlState` and `EndControl`; the device list's `controlled_by`.
    pub const CONTROL_STATE: &str = "control_state";
    /// `Login` with `create`: a new name makes the account.
    pub const LOGIN_OR_CREATE: &str = "login_or_create";
    /// Several relays (the user, 2026-11-26: spread the relayed traffic over the servers): `Welcome` lists them for
    /// `RelayRtts`, `GetIceServers` names the session's other device, and `IceServers` carries `backups`.
    pub const RELAYS: &str = "relays";
    /// Punching set by the server (`IceServers::punch`), and the sessions' anonymous path reports (`PathReport`) when it
    /// asks for them.
    pub const PUNCH: &str = "punch";
}

/// A standby account server that keeps a live copy of the accounts: where a program connects when the account server
/// cannot be reached, and its certificate's SHA-256 (the account server, whose certificate is pinned, vouches for it).
/// It takes logins only once it has been made the account server (`queqiao-server serve` on its copy).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standby {
    /// host:port (TCP) of its accounts.
    pub server: String,
    pub pin: String,
}

/// Punching as the account server's operator set it (`ServerMessage::IceServers`), for each session: as UU Remote's
/// server switches its punching, so a change needs no new program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunchSettings {
    /// Punching at all (off: ICE alone).
    pub enabled: bool,
    /// The most rounds a relayed session punches (0: rounds for as long as it is relayed).
    #[serde(default)]
    pub rounds: u32,
    /// The server collects the sessions' anonymous path reports (`ClientMessage::PathReport`).
    #[serde(default)]
    pub reports: bool,
}

impl Default for PunchSettings {
    /// A server before `features::PUNCH`: punching on, no limit, no reports.
    fn default() -> PunchSettings {
        PunchSettings {
            enabled: true,
            rounds: 0,
            reports: false,
        }
    }
}

/// How one session's path went, anonymous: the NAT kinds of the two sides (`punch` classification: "open", "cone",
/// "linear", "random", "unknown", or "none" where there was none), whether it went direct, how (which candidates), and
/// how long that took. No address, port, device, account or time of day goes in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathReport {
    pub local_nat: String,
    pub remote_nat: String,
    pub direct: bool,
    /// "ice" (the candidates ICE gathered), "punch" (an address punching found), "punch-socket" (one of punching's
    /// own sockets), "relay" (never direct within the first minute).
    pub method: String,
    /// From the session's start to its first direct path (0 when relayed).
    pub ms: u32,
}

impl PathReport {
    /// The NAT kinds and methods a report may name (anything else is refused by the server).
    pub const NATS: [&'static str; 6] = ["open", "cone", "linear", "random", "unknown", "none"];
    pub const METHODS: [&'static str; 4] = ["ice", "punch", "punch-socket", "relay"];

    /// Within the fixed words and bounds.
    pub fn valid(&self) -> bool {
        PathReport::NATS.contains(&self.local_nat.as_str())
            && PathReport::NATS.contains(&self.remote_nat.as_str())
            && PathReport::METHODS.contains(&self.method.as_str())
            && self.direct == (self.method != "relay")
            && self.ms <= 600_000
    }
}

/// How long a STUN check to one relay took from this device (`ClientMessage::RelayRtts`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayRtt {
    /// The relay's STUN address as `Welcome` listed it.
    pub relay: String,
    /// The shortest round trip of a few checks, in milliseconds.
    pub ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Login {
        user: String,
        password: String,
        device_id: String,
        device_name: String,
        role: Role,
        /// The device's Ed25519 public key, hex. A password login may register a new key (a reinstalled
        /// device); a resume may not change it.
        public_key: String,
        /// A tag of the PC this program runs on (a salted hash of its machine id, not the id itself): a password
        /// login from a PC reinstalled under a new device id replaces the account's earlier device of the same PC
        /// and name (it stayed in the list, offline, 2026-11-26). Empty from programs before it.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        machine: String,
        /// One form for both (the user, 2026-11-26): a name the server does not know makes a new account with this
        /// password, on a server that takes new accounts (`features::LOGIN_OR_CREATE`); a known name logs in as ever.
        /// Earlier servers ignore it (a new name is then refused as a wrong login).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        create: bool,
    },
    Resume {
        refresh_token: String,
        device_id: String,
        device_name: String,
        role: Role,
        public_key: String,
    },
    /// A new account (only on a server that takes new accounts), then logged in as with `Login`.
    Register {
        user: String,
        password: String,
        device_id: String,
        device_name: String,
        role: Role,
        public_key: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        machine: String,
    },
    /// Forward to the program `to_role` on device `to` (same account only).
    Signal {
        to: String,
        to_role: Role,
        session: String,
        kind: String,
        body: String,
        /// The sender's Ed25519 signature over [`signed_bytes`], hex.
        signature: String,
    },
    /// Revoke this device's refresh token: the next start needs the password again.
    Logout,
    /// The STUN and TURN servers for a session, with fresh relay credentials. `peer`: the session's other device
    /// (its id), so that both ends get the same relay, the one nearest to both (only to a server with
    /// `features::RELAYS`; without it the message is as before).
    GetIceServers {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer: Option<String>,
        /// Tests (`QUEQIAO_FORCE=relay:<IP>`): the relay at this address first, if the server hands it out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefer: Option<String>,
    },
    /// How far each relay of `Welcome`'s list is from this device (only to a server with `features::RELAYS`).
    RelayRtts { rtts: Vec<RelayRtt> },
    /// Viewer: how one session's path went, anonymous (no address, device or account goes in it, and the server keeps
    /// only counts by day). Only to a server whose `IceServers` asked for them (`PunchSettings::reports`), at most
    /// once per `IceServers` it gave.
    PathReport { report: PathReport },
    /// Remove a device of this account: its saved logins end and it is disconnected.
    RemoveDevice { device_id: String },
    /// Change the account's password: every saved login of the account ends and the account's other
    /// connections are closed; this connection gets a new refresh token (`PasswordChanged`).
    ChangePassword {
        old_password: String,
        new_password: String,
    },
    /// Agent: someone of another account may help, with this code (once). Answered by `HelpOpen`; a new code
    /// replaces the old one.
    OpenHelp { code: String },
    /// Agent: no more help requests.
    CloseHelp,
    /// Agent: a new help ID for this device (the old one stops working at once). While help is open, answered by
    /// `HelpOpen` with the new ID.
    RefreshHelpId,
    /// Viewer: help the device with this help ID, with the code its person read out (empty: ask the person to
    /// allow it). Answered by `HelpGranted` (the device to start a session with) or an error.
    RequestHelp { help_id: String, code: String },
    /// Agent: the person's answer to `HelpAsked`.
    HelpAnswer { request: u64, allow: bool },
    /// Agent: this PC's network cards (MAC addresses, "AA-BB-CC-DD-EE-FF"), for Wake-on-LAN.
    DeviceInfo { macs: Vec<String> },
    /// Wake a device of this account that is offline, through another device of the account on its network.
    Wake { device_id: String },
    /// Agent: the device controlling it now (its name), empty when the session ended; the account's device list shows
    /// it (`Device::controlled_by`). Only to a server with `features::CONTROL_STATE`.
    ControlState { controller: String },
    /// End the session controlling the account's device `device_id` (its banner's "Disconnect"): the server tells
    /// that device's agent (`ServerMessage::EndControl`). Only to a server with `features::CONTROL_STATE`.
    EndControl { device_id: String },
}

/// Relay access for one device: TURN long-term credentials that the server's TURN part accepts for new
/// allocations until `expires` (Unix seconds); an allocation made in time lives on while it is refreshed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnAccess {
    /// host:port of the TURN server (UDP).
    pub address: String,
    pub username: String,
    pub password: String,
    pub expires: u64,
    /// The relay's name, for the session window to show where the session goes ("shanghai"); none from servers
    /// before it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome {
        user: String,
        device_id: String,
        /// A new refresh token after a password login (keep it; the old one, if any, is replaced).
        refresh_token: Option<String>,
        devices: Vec<Device>,
        /// What this server can do beyond the first protocol (`features`); none from servers before it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        features: Vec<String>,
        /// This login made the account (`Login` with `create` and a new name): the person is told, so a mistyped
        /// name is noticed.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        created: bool,
        /// The relays' STUN addresses (host:port, UDP), for the device to measure (`RelayRtts`); none from servers
        /// before `features::RELAYS`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        relays: Vec<String>,
        /// The standby account servers, for when this one cannot be reached (the program keeps them with its login).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        standbys: Vec<Standby>,
    },
    Devices {
        devices: Vec<Device>,
    },
    Signal {
        from: String,
        from_name: String,
        from_role: Role,
        session: String,
        kind: String,
        body: String,
        signature: String,
    },
    Error {
        message: String,
    },
    IceServers {
        /// host:port of a STUN server (UDP), if any.
        stun: Option<String>,
        /// The relay to use.
        turn: Option<TurnAccess>,
        /// Other relays in order, for when `turn` does not answer.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        backups: Vec<TurnAccess>,
        /// Punching as the server's operator set it (`queqiao-server punch`); none from servers before
        /// `features::PUNCH` (punching on, no reports).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        punch: Option<PunchSettings>,
    },
    /// Something the person should know.
    Notice {
        message: String,
    },
    /// A device the account has not had logged in to it (each device words this in its own language).
    DeviceJoined {
        name: String,
    },
    /// A device of the account logged in with a new key (its identity changed: a password login may do that).
    DeviceKeyChanged {
        name: String,
    },
    /// The password was changed: keep this refresh token (all earlier ones were revoked).
    PasswordChanged {
        refresh_token: String,
    },
    /// To an agent: help requests are open; the person gives `help_id` and the code to whoever helps.
    HelpOpen {
        help_id: String,
    },
    /// To an agent: help requests are closed (e.g. too many wrong codes were tried).
    HelpClosed {
        reason: String,
    },
    /// To an agent: `device` of account `user` gave the right code; its session request may follow (the code
    /// is used up). Its key is in `device`.
    HelpRequested {
        device: Device,
        user: String,
    },
    /// To an agent taking help requests: `device` of account `user` asks to help without a code; answer with
    /// `HelpAnswer`.
    HelpAsked {
        request: u64,
        device: Device,
        user: String,
    },
    /// To a viewer: the code was right; `device` (of another account) may be asked for a session now.
    HelpGranted {
        device: Device,
    },
    /// To an agent: send Wake-on-LAN magic packets for `macs` (the account's offline device `name`, on this
    /// agent's network).
    WakeRequest {
        name: String,
        macs: Vec<String>,
    },
    /// To an agent: `by`, a device of the account, ended the session controlling this PC (`ClientMessage::EndControl`).
    EndControl {
        by: String,
    },
}

/// What a signal's signature covers: a fixed label, the session, the kind, sender and receiver (device ids and
/// programs) and the body, each on its own line (the label and the ids have no newlines; the body comes last).
pub fn signed_bytes(
    session: &str,
    kind: &str,
    from: &str,
    from_role: Role,
    to: &str,
    to_role: Role,
    body: &str,
) -> Vec<u8> {
    let role = |role: Role| match role {
        Role::Agent => "agent",
        Role::Viewer => "viewer",
    };
    format!(
        "queqiao-signal-v1\n{session}\n{kind}\n{from}\n{}\n{to}\n{}\n{body}",
        role(from_role),
        role(to_role)
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_as_tagged_json() {
        let message = ClientMessage::Signal {
            to: "d1".into(),
            to_role: Role::Agent,
            session: "s".into(),
            kind: "offer".into(),
            body: "v=0".into(),
            signature: "00".into(),
        };
        let text = serde_json::to_string(&message).unwrap();
        assert!(text.contains("\"type\":\"signal\"") && text.contains("\"to_role\":\"agent\""));
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&text).unwrap(),
            message
        );
        let welcome = ServerMessage::Welcome {
            user: "u".into(),
            device_id: "d".into(),
            refresh_token: None,
            devices: vec![],
            features: vec![],
            created: false,
            relays: vec!["198.51.100.7:3478".into()],
            standbys: vec![Standby {
                server: "198.51.100.8:47100".into(),
                pin: "ab".repeat(32),
            }],
        };
        let text = serde_json::to_string(&welcome).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&text).unwrap(),
            welcome
        );
    }

    #[test]
    fn new_fields_stay_out_of_the_way_of_earlier_programs() {
        // An earlier server's Welcome and device (no features, no controller) read as none.
        let earlier = r#"{"type":"welcome","user":"u","device_id":"d","refresh_token":null,"devices":[{"id":"a","name":"PC","controllable":true,"online":true,"public_key":"00"}]}"#;
        match serde_json::from_str::<ServerMessage>(earlier).unwrap() {
            ServerMessage::Welcome {
                features,
                created,
                devices,
                ..
            } => {
                assert!(features.is_empty() && !created);
                assert!(devices[0].controlled_by.is_empty());
            }
            other => panic!("{other:?}"),
        }
        // A login without `create` is written as before (an earlier server reads it the same).
        let login = ClientMessage::Login {
            user: "u".into(),
            password: "p".into(),
            device_id: "d".into(),
            device_name: "n".into(),
            role: Role::Viewer,
            public_key: "00".into(),
            machine: String::new(),
            create: false,
        };
        assert!(!serde_json::to_string(&login).unwrap().contains("create"));
        let control = ClientMessage::ControlState {
            controller: "Office PC".into(),
        };
        let text = serde_json::to_string(&control).unwrap();
        assert!(text.contains("\"type\":\"control_state\""));
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&text).unwrap(),
            control
        );
    }

    #[test]
    fn relays_are_added_without_changing_what_earlier_programs_send_and_read() {
        // Without a peer, GetIceServers is the earlier message exactly, and the earlier message still reads.
        let plain = ClientMessage::GetIceServers {
            peer: None,
            prefer: None,
        };
        assert_eq!(
            serde_json::to_string(&plain).unwrap(),
            r#"{"type":"get_ice_servers"}"#
        );
        assert_eq!(
            serde_json::from_str::<ClientMessage>(r#"{"type":"get_ice_servers"}"#).unwrap(),
            plain
        );
        let named = ClientMessage::GetIceServers {
            peer: Some("d2".into()),
            prefer: Some("198.51.100.8".into()),
        };
        let text = serde_json::to_string(&named).unwrap();
        assert_eq!(serde_json::from_str::<ClientMessage>(&text).unwrap(), named);
        // An earlier server's IceServers has no backups; an earlier program reads a new one (it ignores them).
        let earlier = r#"{"type":"ice_servers","stun":"198.51.100.7:3478","turn":null}"#;
        match serde_json::from_str::<ServerMessage>(earlier).unwrap() {
            ServerMessage::IceServers { backups, punch, .. } => {
                assert!(backups.is_empty() && punch.is_none())
            }
            other => panic!("{other:?}"),
        }
        let access = TurnAccess {
            address: "198.51.100.8:3478".into(),
            username: "1:2:d:agent".into(),
            password: "p".into(),
            expires: 1,
            name: "beijing".into(),
        };
        let new = ServerMessage::IceServers {
            stun: Some("198.51.100.7:3478".into()),
            turn: Some(access.clone()),
            backups: vec![access],
            punch: Some(PunchSettings {
                enabled: false,
                rounds: 2,
                reports: true,
            }),
        };
        let text = serde_json::to_string(&new).unwrap();
        assert!(text.contains("\"backups\""));
        assert_eq!(serde_json::from_str::<ServerMessage>(&text).unwrap(), new);
        let rtts = ClientMessage::RelayRtts {
            rtts: vec![RelayRtt {
                relay: "198.51.100.7:3478".into(),
                ms: 31,
            }],
        };
        let text = serde_json::to_string(&rtts).unwrap();
        assert!(text.contains("\"type\":\"relay_rtts\""));
        assert_eq!(serde_json::from_str::<ClientMessage>(&text).unwrap(), rtts);
    }
}
