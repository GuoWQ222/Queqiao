//! What logind says about the PC's own screen (seat0): the session on it now, whose it is and of what kind, and
//! whether it is locked. Asked over the system bus (org.freedesktop.login1) each time, never cached.

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

/// A login session as logind describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub path: String,
    pub uid: u32,
    pub user: String,
    /// "x11", "wayland", "tty", "mir" or "unspecified".
    pub kind: String,
    /// "user", "greeter", "lock-screen" or "background".
    pub class: String,
    /// The X server of an X11 session (":0"); empty otherwise.
    pub display: String,
    pub remote: bool,
}

impl Session {
    /// A person's own desktop (not the sign-in screen, not a text console, not a remote session).
    pub fn is_desktop(&self) -> bool {
        self.class == "user" && matches!(self.kind.as_str(), "x11" | "wayland") && !self.remote
    }

    /// The sign-in screen, or a lock screen run as a session of its own (LightDM's).
    pub fn is_sign_in(&self) -> bool {
        matches!(self.class.as_str(), "greeter" | "lock-screen")
    }
}

pub struct Logind {
    connection: Connection,
}

fn text(value: Option<&OwnedValue>) -> String {
    match value.map(|v| &**v) {
        Some(Value::Str(text)) => text.to_string(),
        Some(Value::ObjectPath(path)) => path.to_string(),
        _ => String::new(),
    }
}

fn flag(value: Option<&OwnedValue>) -> bool {
    matches!(value.map(|v| &**v), Some(Value::Bool(true)))
}

/// The fields of a structure property (`(so)`, `(uo)`).
fn fields(value: Option<&OwnedValue>) -> Vec<Value<'static>> {
    match value.map(|v| &**v) {
        Some(Value::Structure(structure)) => structure
            .fields()
            .iter()
            .filter_map(|field| field.try_to_owned().ok().map(Value::from))
            .collect(),
        _ => Vec::new(),
    }
}

impl Logind {
    pub fn connect() -> Result<Logind, String> {
        Connection::system()
            .map(|connection| Logind { connection })
            .map_err(|e| format!("the system bus: {e}"))
    }

    fn properties(
        &self,
        path: &str,
        interface: &str,
    ) -> Result<HashMap<String, OwnedValue>, String> {
        let reply = self
            .connection
            .call_method(
                Some("org.freedesktop.login1"),
                path,
                Some("org.freedesktop.DBus.Properties"),
                "GetAll",
                &(interface,),
            )
            .map_err(|e| format!("logind {path}: {e}"))?;
        reply
            .body()
            .deserialize::<HashMap<String, OwnedValue>>()
            .map_err(|e| format!("logind {path}: {e}"))
    }

    /// The session on seat0's screen now; None while there is none (starting up, switching).
    pub fn active(&self) -> Result<Option<Session>, String> {
        let seat = self.properties(
            "/org/freedesktop/login1/seat/seat0",
            "org.freedesktop.login1.Seat",
        )?;
        let active = fields(seat.get("ActiveSession"));
        let path = match active.get(1) {
            Some(Value::ObjectPath(path)) => path.to_string(),
            _ => return Ok(None),
        };
        // "/" when there is none.
        if path.len() <= 1 {
            return Ok(None);
        }
        self.session(&path).map(Some)
    }

    pub fn session(&self, path: &str) -> Result<Session, String> {
        let session = self.properties(path, "org.freedesktop.login1.Session")?;
        let uid = match fields(session.get("User")).first() {
            Some(Value::U32(uid)) => *uid,
            _ => return Err(format!("logind {path}: no user")),
        };
        Ok(Session {
            id: text(session.get("Id")),
            path: path.to_string(),
            uid,
            user: text(session.get("Name")),
            kind: text(session.get("Type")),
            class: text(session.get("Class")),
            display: text(session.get("Display")),
            remote: flag(session.get("Remote")),
        })
    }

    /// Whether the session is locked (LockedHint, which the desktop's screen locker sets).
    pub fn locked(&self, path: &str) -> Result<bool, String> {
        let session = self.properties(path, "org.freedesktop.login1.Session")?;
        Ok(flag(session.get("LockedHint")))
    }
}
