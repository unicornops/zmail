//! New mail, reported in the status line rather than as an OS
//! notification (ADR 0007 section 7).

use std::sync::mpsc;

use zmail_notify::{NewMailBatch, Notifier};

/// Hands each new-mail batch the sync reports to the UI thread, which
/// shows it in the status line and rings the bell if the user wants.
pub struct TerminalNotifier {
    batches: mpsc::Sender<NewMailBatch>,
}

impl TerminalNotifier {
    /// The notifier, and where its batches arrive.
    pub fn new() -> (Self, mpsc::Receiver<NewMailBatch>) {
        let (batches, receiver) = mpsc::channel();
        (Self { batches }, receiver)
    }
}

impl Notifier for TerminalNotifier {
    fn notify_new_mail(&self, batch: &NewMailBatch) {
        let _ = self.batches.send(batch.clone());
    }
}
