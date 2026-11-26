//! The desktop's keyring on Linux, through the Secret Service (`org.freedesktop.secrets` on the session bus: GNOME
//! Keyring, KWallet, KeePassXC): where this person's key for sealed data (`config::protect`: the saved login's refresh
//! token, the device key, the fixed help code) is kept when the desktop has one. As Windows keeps it with DPAPI, the
//! keyring keeps it encrypted with the person's login password and out of the home folder: a copy of the home folder
//! (a backup, a stolen disk) no longer holds the key beside what it seals (the security review of 2026-11-26).
//!
//! One item in the default collection, found by its attributes. A locked keyring is unlocked the keyring's own way (it
//! asks the person), waited for at most PROMPT_WAIT.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const SERVICE: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_INTERFACE: &str = "org.freedesktop.Secret.Service";
const DEFAULT_COLLECTION: &str = "/org/freedesktop/secrets/aliases/default";
/// How long a keyring's own unlock prompt may wait for the person.
const PROMPT_WAIT: Duration = Duration::from_secs(120);

/// The Secret Service's secret: (session, parameters, value, content type).
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

/// The item's attributes: how it is found again.
fn attributes() -> HashMap<&'static str, &'static str> {
    HashMap::from([("application", "queqiao"), ("purpose", "sealing key")])
}

struct Keyring {
    connection: Connection,
    service: Proxy<'static>,
    session: OwnedObjectPath,
}

fn error(context: &str, error: impl std::fmt::Display) -> String {
    format!("the keyring ({context}): {error}")
}

impl Keyring {
    fn open() -> Result<Keyring, String> {
        let connection = Connection::session().map_err(|e| error("session bus", e))?;
        let service = Proxy::new(&connection, SERVICE, SERVICE_PATH, SERVICE_INTERFACE)
            .map_err(|e| error("service", e))?;
        // "plain": the secret goes over the session bus, which only this person's programs reach.
        let (_, session): (OwnedValue, OwnedObjectPath) = service
            .call("OpenSession", &("plain", Value::from("")))
            .map_err(|e| error("OpenSession", e))?;
        Ok(Keyring {
            connection,
            service,
            session,
        })
    }

    /// Waits for the person to answer a keyring prompt (`/` when there is none): whether they let it go ahead.
    fn prompt(&self, prompt: &OwnedObjectPath) -> Result<(), String> {
        if prompt.as_str() == "/" {
            return Ok(());
        }
        let proxy = Proxy::new(
            &self.connection,
            SERVICE,
            prompt.as_str().to_string(),
            "org.freedesktop.Secret.Prompt",
        )
        .map_err(|e| error("prompt", e))?;
        // Listening before asking, so the answer cannot come first.
        let mut completed = proxy
            .receive_signal("Completed")
            .map_err(|e| error("prompt", e))?;
        let (sender, answer) = mpsc::channel();
        std::thread::spawn(move || {
            let dismissed = completed
                .next()
                .and_then(|message| message.body().deserialize::<(bool, OwnedValue)>().ok())
                .is_none_or(|(dismissed, _)| dismissed);
            let _ = sender.send(dismissed);
        });
        proxy
            .call::<_, _, ()>("Prompt", &("",))
            .map_err(|e| error("prompt", e))?;
        match answer.recv_timeout(PROMPT_WAIT) {
            Ok(false) => Ok(()),
            Ok(true) => Err("the keyring was not unlocked (its prompt was dismissed)".into()),
            Err(_) => Err("the keyring was not unlocked in time".into()),
        }
    }

    /// The item holding the key, unlocked; None when there is none.
    fn item(&self) -> Result<Option<OwnedObjectPath>, String> {
        let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = self
            .service
            .call("SearchItems", &(attributes(),))
            .map_err(|e| error("SearchItems", e))?;
        if let Some(item) = unlocked.into_iter().next() {
            return Ok(Some(item));
        }
        let Some(item) = locked.into_iter().next() else {
            return Ok(None);
        };
        let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = self
            .service
            .call("Unlock", &(vec![ObjectPath::from(&item)],))
            .map_err(|e| error("Unlock", e))?;
        self.prompt(&prompt)?;
        Ok(Some(item))
    }

    fn secret(&self, item: &OwnedObjectPath) -> Result<Vec<u8>, String> {
        let secrets: HashMap<OwnedObjectPath, Secret> = self
            .service
            .call(
                "GetSecrets",
                &(
                    vec![ObjectPath::from(item)],
                    ObjectPath::from(&self.session),
                ),
            )
            .map_err(|e| error("GetSecrets", e))?;
        secrets
            .into_values()
            .next()
            .map(|(_, _, value, _)| value)
            .ok_or_else(|| "the keyring gave no secret for the item".into())
    }
}

/// This person's key from the keyring: Ok(None) when the keyring is there but holds none, Err when there is no keyring
/// (no desktop session, none running) or it could not be used (locked and not unlocked).
pub(crate) fn load() -> Result<Option<Vec<u8>>, String> {
    let keyring = Keyring::open()?;
    match keyring.item()? {
        Some(item) => keyring.secret(&item).map(Some),
        None => Ok(None),
    }
}

/// Keeps `key` in the keyring's default collection (replacing an earlier one).
pub(crate) fn store(key: &[u8]) -> Result<(), String> {
    let keyring = Keyring::open()?;
    let collection = Proxy::new(
        &keyring.connection,
        SERVICE,
        DEFAULT_COLLECTION,
        "org.freedesktop.Secret.Collection",
    )
    .map_err(|e| error("collection", e))?;
    let properties: HashMap<&str, Value> = HashMap::from([
        (
            "org.freedesktop.Secret.Item.Label",
            Value::from("Queqiao (the key of its saved login)"),
        ),
        (
            "org.freedesktop.Secret.Item.Attributes",
            Value::from(attributes()),
        ),
    ]);
    let secret = (
        ObjectPath::from(&keyring.session),
        Vec::<u8>::new(),
        key.to_vec(),
        "application/octet-stream",
    );
    let (_, prompt): (OwnedObjectPath, OwnedObjectPath) = collection
        .call("CreateItem", &(properties, secret, true))
        .map_err(|e| error("CreateItem", e))?;
    // A locked collection asks first; the item is made once the person lets it.
    keyring.prompt(&prompt)
}
