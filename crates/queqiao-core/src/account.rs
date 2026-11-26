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
    /// The STUN and TURN servers for a session, with fresh relay credentials.
    GetIceServers,
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
        turn: Option<TurnAccess>,
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
}
