//! Reactor API key in the desktop operating system's credential store.
//!
//! The UI only receives a saved/not-saved result. No key enters preferences, projects, or the
//! control-channel state, and credential-store calls run off the render thread.

use std::sync::mpsc::{Receiver, channel};

use filmcraft_ui_egui::ReactorKeyAction;
use zeroize::Zeroize;

const SERVICE: &str = "ai.storyteller.filmcraft.reactor";
const ACCOUNT: &str = "api-key";

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(|e| format!("Could not open credential store: {e}"))
}

pub(crate) fn read_key() -> Result<zeroize::Zeroizing<String>, String> {
    let key = entry()?.get_password().map_err(|error| match error {
        keyring::Error::NoEntry => "Add a Reactor API key in Settings first.".into(),
        other => format!("Could not read Reactor API key: {other}"),
    })?;
    Ok(zeroize::Zeroizing::new(key))
}

fn apply(action: ReactorKeyAction) -> Result<bool, String> {
    let entry = entry()?;
    match action {
        ReactorKeyAction::Check => match entry.get_password() {
            Ok(mut key) => {
                key.zeroize();
                Ok(true)
            }
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(format!("Could not read Reactor key status: {e}")),
        },
        ReactorKeyAction::Save(mut key) => {
            if key.trim().is_empty() || key.len() > 4096 {
                key.zeroize();
                return Err("Enter a valid Reactor API key.".into());
            }
            let result = entry.set_password(&key).map_err(|e| format!("Could not save Reactor API key: {e}"));
            key.zeroize();
            result.map(|()| true)
        }
        ReactorKeyAction::Remove => match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(format!("Could not remove Reactor API key: {e}")),
        },
    }
}

pub fn request(action: ReactorKeyAction) -> Receiver<Result<bool, String>> {
    let (tx, rx) = channel();
    let worker_tx = tx.clone();
    let started = std::thread::Builder::new().name("reactor-credential-store".into()).spawn(move || {
        let result = std::panic::catch_unwind(|| apply(action)).unwrap_or_else(|_| Err("Credential store failed unexpectedly.".into()));
        let _ = worker_tx.send(result);
    });
    if let Err(error) = started {
        let _ = tx.send(Err(format!("Could not start credential-store worker: {error}")));
    }
    rx
}
