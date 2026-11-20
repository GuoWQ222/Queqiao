//! Messages between a device (agent or viewer) and queqiao-server, as JSON over one TLS WebSocket.
//!
//! A connection starts with `Login` (user name and password) or `Resume` (a refresh token from an earlier
//! login); the server answers `Welcome`. After that the server pushes the account's device list as it
//! changes, and forwards `Signal`s between the account's own devices only (the WebRTC offer and answer
//! of a session). Every device on an account can be both agent (controlled) and viewer (controlling).
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
    },
    Resume {
        refresh_token: String,
        device_id: String,
        device_name: String,
        role: Role,
        public_key: String,
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
    /// Something the person should know, e.g. a new device logged in to the account.
    Notice {
        message: String,
    },
    /// The password was changed: keep this refresh token (all earlier ones were revoked).
    PasswordChanged {
        refresh_token: String,
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
        };
        let text = serde_json::to_string(&welcome).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&text).unwrap(),
            welcome
        );
    }
}
