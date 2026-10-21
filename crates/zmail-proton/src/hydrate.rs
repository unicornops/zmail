//! Hydration: fetching, decrypting and storing message bodies on demand.
//!
//! This follows ADR 0004's rules for decryption, which keep any decryption
//! oracle out of reach of the server:
//!
//! 1. Decryption is decoupled from network scheduling. Every body and
//!    attachment of the batch is downloaded first; only then are they
//!    decrypted, on a blocking worker, so the timing and order of requests
//!    never depend on how long, or whether, a decryption took.
//! 2. Failures are never reported back: nothing is sent to Proton (or
//!    anyone) because a message did not decrypt.
//! 3. Nothing is re-fetched because decryption failed. An undecryptable
//!    message is stored the way Bridge presents it (the armored ciphertext
//!    with a note), so it counts as hydrated and stays as it is.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use zmail_core::{HydratedBody, ProtocolKind, SyncTarget};
use zmail_proton_api::Message;

use crate::{
    ProtonSession,
    message::{build_rfc822, decrypt_message},
};

/// Fetches, decrypts and parses the bodies of the given messages, in the
/// shape the store takes. Messages that no longer exist on the server are
/// left out; the next sync removes them.
pub async fn fetch_message_bodies(
    target: &SyncTarget,
    session: &ProtonSession,
    mailbox_name: &str,
    message_ids: &[String],
) -> Result<Vec<(String, HydratedBody)>> {
    if target.protocol != ProtocolKind::Proton {
        bail!("Proton adapter received a non-Proton sync target");
    }
    if message_ids.is_empty() {
        return Ok(Vec::new());
    }
    let client = session.client();
    let keys = session.unlocked_keys().await?;

    // Rule 1: the network phase finishes before any decryption starts.
    let mut downloaded: Vec<(Message, Vec<Vec<u8>>)> = Vec::with_capacity(message_ids.len());
    for message_id in message_ids {
        let message = match client.get_message(message_id).await {
            Ok(message) => message,
            Err(error) if error.status() == Some(404) => continue,
            Err(error) => return Err(error.into()),
        };
        let mut attachments = Vec::with_capacity(message.attachments.len());
        for attachment in &message.attachments {
            attachments.push(client.get_attachment(&attachment.id).await?);
        }
        downloaded.push((message, attachments));
    }

    let mailbox_name = mailbox_name.to_owned();
    let attachments_dir = PathBuf::from(&target.attachments_dir);
    tokio::task::spawn_blocking(move || {
        downloaded
            .into_iter()
            .map(|(message, attachments)| {
                let id = message.metadata.id.clone();
                let keyring = keys.addresses.get(&message.metadata.address_id);
                let mut decrypted = decrypt_message(keyring, message, attachments);
                let raw = build_rfc822(&mut decrypted);
                let body = zmail_imap::hydrated_body_from_rfc822(
                    &raw,
                    &mailbox_name,
                    &storage_id(&id),
                    &attachments_dir,
                )
                .unwrap_or_else(undecryptable_body);
                (id, body)
            })
            .collect()
    })
    .await
    .context("the decryption task stopped")
}

/// Proton IDs are base64; keep attachment file names to safe characters.
fn storage_id(message_id: &str) -> String {
    message_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// The stored body when even the rebuilt message did not parse.
fn undecryptable_body() -> HydratedBody {
    HydratedBody {
        body_text: Some("This message could not be decrypted.".to_owned()),
        sanitized_html: None,
        has_remote_content: false,
        body_preview: "This message could not be decrypted.".to_owned(),
        attachments: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_ids_are_file_name_safe() {
        assert_eq!(storage_id("aB-c_d=="), "aB-c_d__");
        assert_eq!(storage_id("x/y"), "x_y");
    }
}
