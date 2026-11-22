//! How the panels reach the player thread.

use crate::player::{Cmd, Shared};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Takes a lock even if a worker thread panicked while holding it: the data is plain values, and
/// the window must stay up to say what went wrong.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct Link {
    pub tx: Sender<Cmd>,
    pub shared: Arc<Shared>,
}

impl Link {
    pub fn send(&self, c: Cmd) {
        if self.tx.send(c).is_err() {
            self.say("Le lecteur s'est arrêté : relance l'application.");
        }
    }

    /// Shows a message in the status bar.
    pub fn say(&self, m: impl Into<String>) {
        lock(&self.shared.status).message = m.into();
    }
}
