//! Messages between a device (agent or viewer) and queqiao-server, as JSON over one TLS WebSocket.
//!
//! A connection starts with `Login` (user name and password) or `Resume` (a refresh token from an earlier
//! login); the server answers `Welcome`. After that the server pushes the account's device list as it
//! changes, and forwards `Signal`s between the account's own devices only (the WebRTC offer and answer
//! of a session). Every device on an account can be both agent (controlled) and viewer (controlling).

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
    },
    Resume {
        refresh_token: String,
        device_id: String,
        device_name: String,
        role: Role,
    },
    /// Forward to the program `to_role` on device `to` (same account only).
    Signal {
        to: String,
        to_role: Role,
        session: String,
        kind: String,
        body: String,
    },
    /// Revoke this device's refresh token: the next start needs the password again.
    Logout,
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
    },
    Error {
        message: String,
    },
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
