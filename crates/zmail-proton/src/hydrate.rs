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

use anyhow::{Context, Result, bail};
use zmail_core::{HydratedBody, MessageHeaders, ProtocolKind, SyncTarget};
use zmail_proton_api::{Client, Message};

use crate::{
    ProtonSession,
    message::{INTERNAL_ID_DOMAIN, build_rfc822, decrypt_message},
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
    let downloaded = download(client, message_ids).await?;

    let mailbox_name = mailbox_name.to_owned();
    // Attachments are written through the store's blob store, which
    // encrypts them (ADR 0005): decrypted Proton mail never sits on disk in
    // plaintext.
    let blobs = target.blobs.clone();
    tokio::task::spawn_blocking(move || {
        downloaded
            .into_iter()
            .map(|(message, attachments)| {
                let id = message.metadata.id.clone();
                let keyring = keys.addresses.get(&message.metadata.address_id);
                let mut decrypted = decrypt_message(keyring, message, attachments);
                let raw = build_rfc822(&mut decrypted);
                let mut body = zmail_imap::hydrated_body_from_rfc822(
                    &raw,
                    &mailbox_name,
                    &storage_id(&id),
                    &blobs,
                )
                .unwrap_or_else(undecryptable_body);
                drop_own_reference(&mut body.headers, &id);
                (id, body)
            })
            .collect()
    })
    .await
    .context("the decryption task stopped")
}

/// The network phase: each message with its attachments' encrypted data.
async fn download(client: &Client, message_ids: &[String]) -> Result<Vec<(Message, Vec<Vec<u8>>)>> {
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
    Ok(downloaded)
}

/// Bridge adds the message's own internal ID to the References it builds
/// (`AddMessageIDReference`), for its IMAP clients. A message's own id is
/// not one of the messages it refers to, and a reply would carry it to
/// outside recipients, so it is left out of the stored References.
fn drop_own_reference(headers: &mut MessageHeaders, message_id: &str) {
    let own = format!("{message_id}@{INTERNAL_ID_DOMAIN}");
    headers.references.retain(|reference| *reference != own);
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
        raw_html: None,
        has_remote_content: false,
        body_preview: "This message could not be decrypted.".to_owned(),
        attachments: Vec::new(),
        // Read as far as they go: nothing is fetched again because a
        // message did not decrypt (rule 3).
        headers: MessageHeaders {
            captured: true,
            ..MessageHeaders::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use zmail_proton_api::{
        Manager,
        mock::{MockServer, Reply},
    };

    use super::*;

    const MIB: i64 = 1 << 20;

    /// A message whose attachments declare these encrypted sizes.
    fn message_reply(id: &str, sizes: &[i64]) -> Reply {
        let attachments: Vec<serde_json::Value> = sizes
            .iter()
            .enumerate()
            .map(|(index, size)| serde_json::json!({"ID": format!("{id}-a{index}"), "Size": size}))
            .collect();
        Reply::json(
            200,
            serde_json::json!({"Code": 1000, "Message": {
                "ID": id, "AddressID": "addr-1", "Body": "-----BEGIN PGP MESSAGE-----",
                "MIMEType": "text/plain", "Attachments": attachments,
            }}),
        )
    }

    /// A mock server with the messages of `sizes`, each attachment's data
    /// a few bytes.
    async fn server(messages: &'static [(&'static str, &'static [i64])]) -> MockServer {
        MockServer::start(move |request, _| {
            if let Some(id) = request.path.strip_prefix("/mail/v4/messages/")
                && let Some((_, sizes)) = messages.iter().find(|(known, _)| *known == id)
            {
                return message_reply(id, sizes);
            }
            if request.path.starts_with("/mail/v4/attachments/") {
                return Reply::bytes(200, "encrypted");
            }
            Reply::json(404, serde_json::json!({"Code": 2501, "Error": "not found"}))
        })
        .await
    }

    async fn downloaded(server: &MockServer, ids: &[&str]) -> Result<Vec<(Message, Vec<Vec<u8>>)>> {
        let manager =
            Manager::new(zmail_proton_api::mock::config(&server.url, None)).expect("manager");
        let client = manager.new_client("uid", "access", "refresh");
        let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
        tokio::time::timeout(Duration::from_secs(20), download(&client, &ids))
            .await
            .expect("bounded: finished in time")
    }

    fn attachment_requests(server: &MockServer, message_id: &str) -> usize {
        let prefix = format!("/mail/v4/attachments/{message_id}-");
        server
            .requests()
            .iter()
            .filter(|request| request.path.starts_with(&prefix))
            .count()
    }

    // Issue #462: a batch stops at its byte budget; the messages after it
    // are left for the next hydration.
    #[tokio::test]
    async fn a_batch_stops_at_its_byte_budget() {
        static MESSAGES: [(&str, &[i64]); 2] = [
            ("m1", &[100 * MIB, 50 * MIB]),
            ("m2", &[100 * MIB, 50 * MIB]),
        ];
        let server = server(&MESSAGES).await;
        let batch = downloaded(&server, &["m1", "m2"]).await.expect("m1 fits");
        let ids: Vec<&str> = batch
            .iter()
            .map(|(message, _)| message.metadata.id.as_str())
            .collect();
        assert_eq!(ids, ["m1"], "m2 would take the batch over its budget");
        assert_eq!(batch[0].1.len(), 2);
        assert_eq!(attachment_requests(&server, "m2"), 0);
    }

    #[tokio::test]
    async fn a_message_larger_than_the_budget_is_refused() {
        static MESSAGES: [(&str, &[i64]); 1] = [("big", &[200 * MIB, 200 * MIB])];
        let server = server(&MESSAGES).await;
        let error = downloaded(&server, &["big"]).await.expect_err("too large");
        assert!(format!("{error:#}").contains("too large"), "{error:#}");
        assert_eq!(attachment_requests(&server, "big"), 0, "nothing downloaded");
    }

    #[test]
    fn a_messages_own_internal_id_is_not_one_of_its_references() {
        let mut headers = MessageHeaders {
            captured: true,
            message_id: Some("ext@example.com".to_owned()),
            references: vec![
                "root@example.com".to_owned(),
                "other@protonmail.internalid".to_owned(),
                "msg-1@protonmail.internalid".to_owned(),
            ],
            ..MessageHeaders::default()
        };
        drop_own_reference(&mut headers, "msg-1");
        assert_eq!(
            headers.references,
            ["root@example.com", "other@protonmail.internalid"]
        );
    }

    #[test]
    fn storage_ids_are_file_name_safe() {
        assert_eq!(storage_id("aB-c_d=="), "aB-c_d__");
        assert_eq!(storage_id("x/y"), "x_y");
    }
}
