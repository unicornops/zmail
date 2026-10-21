// Ported from go-proton-api `message.go` (`GetMessageMetadataPage`,
// `countMessages`) and `message_types.go` (`MessageMetadata`,
// `MessageFilter`, `MessageFlag`) at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Also `GetMessage`, `Message`, the read/unread, label/unlabel and delete
// actions (`MessageActionReq`, `LabelMessagesReq`, `LabelMessagesRes`) from
// `message.go` and `message_types.go`, and `UndoActions` from `undo.go` and
// `undo_types.go`.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust. Adds the
// `ConversationID` field Proton returns (go-proton-api does not read it),
// which Zmail uses to group threads. Chunks run one after another rather
// than in parallel. Sending follows later (#237).

//! Message metadata: what a message list needs, without the encrypted body.

use serde::{Deserialize, Serialize};

use crate::{
    attachment::Attachment, boolean::api_bool, client::Client, header_types::Headers,
    label::STARRED_LABEL, manager::Request, response::Error,
};

/// An email address as the API sends it (Go's `mail.Address`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct EmailAddress {
    /// Display name, possibly empty.
    #[serde(rename = "Name", default)]
    pub name: String,
    /// The address.
    #[serde(rename = "Address", default)]
    pub address: String,
}

/// Message flags (go-proton-api's `MessageFlag`, a bit set).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct MessageFlags(pub i64);

impl MessageFlags {
    /// The message was received.
    pub const RECEIVED: i64 = 1 << 0;
    /// The message was sent.
    pub const SENT: i64 = 1 << 1;
    /// Proton-internal (end-to-end encrypted between Proton users).
    pub const INTERNAL: i64 = 1 << 2;
    /// End-to-end encrypted.
    pub const E2E: i64 = 1 << 3;

    /// Whether every bit of `flag` is set.
    pub fn has(self, flag: i64) -> bool {
        self.0 & flag == flag
    }
}

/// A message's metadata (go-proton-api's `MessageMetadata`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct MessageMetadata {
    /// Message ID.
    #[serde(rename = "ID")]
    pub id: String,
    /// The address the message belongs to.
    #[serde(rename = "AddressID", default)]
    pub address_id: String,
    /// Labels and folders the message is in.
    #[serde(rename = "LabelIDs", default, deserialize_with = "null_as_empty")]
    pub label_ids: Vec<String>,
    /// The `Message-ID` header, for messages from outside Proton.
    #[serde(rename = "ExternalID", default)]
    pub external_id: String,
    /// Conversation ID (not read by go-proton-api).
    #[serde(rename = "ConversationID", default)]
    pub conversation_id: String,
    /// Subject (stored in clear by Proton).
    #[serde(rename = "Subject", default)]
    pub subject: String,
    /// Sender.
    #[serde(rename = "Sender", default)]
    pub sender: Option<EmailAddress>,
    /// To.
    #[serde(rename = "ToList", default, deserialize_with = "null_as_empty")]
    pub to_list: Vec<EmailAddress>,
    /// Cc.
    #[serde(rename = "CCList", default, deserialize_with = "null_as_empty")]
    pub cc_list: Vec<EmailAddress>,
    /// Bcc.
    #[serde(rename = "BCCList", default, deserialize_with = "null_as_empty")]
    pub bcc_list: Vec<EmailAddress>,
    /// Reply-To.
    #[serde(rename = "ReplyTos", default, deserialize_with = "null_as_empty")]
    pub reply_tos: Vec<EmailAddress>,
    /// Flags.
    #[serde(rename = "Flags", default)]
    pub flags: MessageFlags,
    /// Received (or sent) time, Unix seconds.
    #[serde(rename = "Time", default)]
    pub time: i64,
    /// Size in bytes.
    #[serde(rename = "Size", default)]
    pub size: i64,
    /// Whether the message is unread.
    #[serde(rename = "Unread", default, deserialize_with = "api_bool")]
    pub unread: bool,
    /// Whether it was replied to.
    #[serde(rename = "IsReplied", default, deserialize_with = "api_bool")]
    pub is_replied: bool,
    /// Whether it was replied to all.
    #[serde(rename = "IsRepliedAll", default, deserialize_with = "api_bool")]
    pub is_replied_all: bool,
    /// Whether it was forwarded.
    #[serde(rename = "IsForwarded", default, deserialize_with = "api_bool")]
    pub is_forwarded: bool,
    /// Number of attachments.
    #[serde(rename = "NumAttachments", default)]
    pub num_attachments: i64,
}

impl MessageMetadata {
    /// Whether the message has been read (`Seen`).
    pub fn seen(&self) -> bool {
        !self.unread
    }

    /// Whether the message is starred (`Starred`).
    pub fn starred(&self) -> bool {
        self.label_ids.iter().any(|label| label == STARRED_LABEL)
    }

    /// Whether the message is a draft (`IsDraft`): neither received nor
    /// sent.
    pub fn is_draft(&self) -> bool {
        self.flags.0 & (MessageFlags::RECEIVED | MessageFlags::SENT) == 0
    }
}

/// A full message: metadata, headers and the encrypted body (go-proton-api's
/// `Message`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Message {
    /// The metadata.
    #[serde(flatten)]
    pub metadata: MessageMetadata,
    /// The original header block, as text.
    #[serde(rename = "Header", default)]
    pub header: String,
    /// The headers, parsed and in order.
    #[serde(rename = "ParsedHeaders", default)]
    pub parsed_headers: Headers,
    /// The body: an armored OpenPGP message encrypted to the address key.
    #[serde(rename = "Body", default)]
    pub body: String,
    /// The body's MIME type: `text/plain`, `text/html`, or
    /// `multipart/mixed` for PGP/MIME messages from outside Proton.
    #[serde(rename = "MIMEType", default)]
    pub mime_type: String,
    /// Attachments.
    #[serde(rename = "Attachments", default, deserialize_with = "null_as_empty")]
    pub attachments: Vec<Attachment>,
}

/// Which messages to list (go-proton-api's `MessageFilter`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MessageFilter {
    /// Only these message IDs.
    #[serde(rename = "ID", skip_serializing_if = "Vec::is_empty")]
    pub id: Vec<String>,
    /// Only messages in this label.
    #[serde(rename = "LabelID", skip_serializing_if = "String::is_empty")]
    pub label_id: String,
    /// Only messages of this address.
    #[serde(rename = "AddressID", skip_serializing_if = "String::is_empty")]
    pub address_id: String,
    /// Stop at (and include) this message ID; used as a cursor.
    #[serde(rename = "EndID", skip_serializing_if = "String::is_empty")]
    pub end_id: String,
    /// Newest first.
    #[serde(rename = "Desc", serialize_with = "serialize_api_bool")]
    pub desc: bool,
}

/// Go encodes an empty slice as `null`.
fn null_as_empty<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

fn serialize_api_bool<S: serde::Serializer>(
    value: &bool,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_i64(i64::from(*value))
}

/// One message's result in a label action (`LabelMessageRes`).
#[derive(Debug, Deserialize)]
struct LabelMessageRes {
    #[serde(rename = "Response")]
    response: LabelResponse,
}

#[derive(Debug, Deserialize)]
struct LabelResponse {
    #[serde(rename = "Code")]
    code: i64,
    #[serde(rename = "Error", default)]
    error: String,
}

/// A label action's result (`LabelMessagesRes`).
#[derive(Debug, Deserialize)]
struct LabelMessagesRes {
    #[serde(rename = "Responses", default, deserialize_with = "null_as_empty")]
    responses: Vec<LabelMessageRes>,
    #[serde(rename = "UndoToken", default)]
    undo_token: Option<UndoToken>,
}

/// A token that undoes one label action (`UndoToken`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UndoToken {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "ValidUntil")]
    valid_until: i64,
}

#[derive(Serialize)]
struct MessageActionReq<'a> {
    #[serde(rename = "IDs")]
    ids: &'a [String],
}

#[derive(Serialize)]
struct LabelMessagesReq<'a> {
    #[serde(rename = "LabelID")]
    label_id: &'a str,
    #[serde(rename = "IDs")]
    ids: &'a [String],
}

impl Client {
    /// Marks messages read (`MarkMessagesRead`).
    pub async fn mark_messages_read(&self, message_ids: &[String]) -> Result<(), Error> {
        self.message_action("/mail/v4/messages/read", message_ids)
            .await
    }

    /// Marks messages unread (`MarkMessagesUnread`).
    pub async fn mark_messages_unread(&self, message_ids: &[String]) -> Result<(), Error> {
        self.message_action("/mail/v4/messages/unread", message_ids)
            .await
    }

    /// Deletes messages permanently (`DeleteMessage`).
    pub async fn delete_messages(&self, message_ids: &[String]) -> Result<(), Error> {
        self.message_action("/mail/v4/messages/delete", message_ids)
            .await
    }

    async fn message_action(&self, path: &str, message_ids: &[String]) -> Result<(), Error> {
        for page in message_ids.chunks(crate::paging::MAX_PAGE_SIZE) {
            let request =
                Request::new(reqwest::Method::PUT, path).json(&MessageActionReq { ids: page })?;
            self.send(&request).await?;
        }
        Ok(())
    }

    /// Adds a label, or moves to a folder or system label
    /// (`LabelMessages`). If a chunk fails, the chunks before it are undone.
    pub async fn label_messages(
        &self,
        message_ids: &[String],
        label_id: &str,
    ) -> Result<(), Error> {
        self.label_action("/mail/v4/messages/label", message_ids, label_id)
            .await
    }

    /// Removes a label (`UnlabelMessages`).
    pub async fn unlabel_messages(
        &self,
        message_ids: &[String],
        label_id: &str,
    ) -> Result<(), Error> {
        self.label_action("/mail/v4/messages/unlabel", message_ids, label_id)
            .await
    }

    async fn label_action(
        &self,
        path: &str,
        message_ids: &[String],
        label_id: &str,
    ) -> Result<(), Error> {
        let mut tokens = Vec::new();
        for chunk in message_ids.chunks(crate::paging::MAX_PAGE_SIZE) {
            let request = Request::new(reqwest::Method::PUT, path).json(&LabelMessagesReq {
                label_id,
                ids: chunk,
            })?;
            let res: LabelMessagesRes = self.json(&request).await?;
            if let Some(failed) = res.responses.iter().find(|item| item.response.code != 1000) {
                let reason = failed.response.error.clone();
                self.undo_actions(&tokens).await?;
                return Err(Error::Api(crate::response::ApiError {
                    status: 200,
                    code: crate::response::Code(failed.response.code),
                    message: format!("failed to change labels: {reason}"),
                    details: None,
                }));
            }
            tokens.extend(res.undo_token);
        }
        Ok(())
    }

    /// Undoes earlier label actions (`UndoActions`); expired tokens are an
    /// error, as upstream.
    async fn undo_actions(&self, tokens: &[UndoToken]) -> Result<(), Error> {
        let now = chrono::Utc::now().timestamp();
        for token in tokens {
            if token.valid_until < now {
                return Err(Error::Config("undo token expired".to_owned()));
            }
            self.send(&Request::post("/mail/v4/undoactions").json(token)?)
                .await?;
        }
        Ok(())
    }

    /// Returns a full message (`GetMessage`).
    pub async fn get_message(&self, message_id: &str) -> Result<Message, Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "Message")]
            message: Message,
        }
        let res: Res = self
            .json(&Request::get(format!("/mail/v4/messages/{message_id}")))
            .await?;
        Ok(res.message)
    }

    /// Returns one page of message metadata (`GetMessageMetadataPage`),
    /// asking again while the server reports the result as stale.
    pub async fn get_message_metadata_page(
        &self,
        page: usize,
        page_size: usize,
        filter: &MessageFilter,
    ) -> Result<Vec<MessageMetadata>, Error> {
        #[derive(Serialize)]
        struct Req<'a> {
            #[serde(flatten)]
            filter: &'a MessageFilter,
            #[serde(rename = "Page")]
            page: usize,
            #[serde(rename = "PageSize")]
            page_size: usize,
            #[serde(rename = "Sort")]
            sort: &'static str,
        }
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "Messages", default, deserialize_with = "null_as_empty")]
            messages: Vec<MessageMetadata>,
            #[serde(rename = "Stale", default, deserialize_with = "api_bool")]
            stale: bool,
        }
        let request = Request::post("/mail/v4/messages")
            .header("X-HTTP-Method-Override", "GET")
            .json(&Req {
                filter,
                page,
                page_size,
                sort: "ID",
            })?;
        loop {
            let res: Res = self.json(&request).await?;
            if !res.stale {
                return Ok(res.messages);
            }
        }
    }

    /// Counts the messages matching `filter` (`countMessages`).
    pub async fn count_messages(&self, filter: &MessageFilter) -> Result<usize, Error> {
        #[derive(Serialize)]
        struct Req<'a> {
            #[serde(flatten)]
            filter: &'a MessageFilter,
            // Upstream sends the limit as a string (`json:",string"`).
            #[serde(rename = "Limit")]
            limit: &'static str,
        }
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "Total", default)]
            total: usize,
        }
        let request = Request::post("/mail/v4/messages")
            .header("X-HTTP-Method-Override", "GET")
            .json(&Req { filter, limit: "0" })?;
        let res: Res = self.json(&request).await?;
        Ok(res.total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_metadata() {
        let meta: MessageMetadata = serde_json::from_value(serde_json::json!({
            "ID": "m1", "AddressID": "a1", "LabelIDs": ["0", "5", "10"],
            "ConversationID": "c1", "Subject": "Hello",
            "Sender": {"Name": "Alice", "Address": "alice@example.com"},
            "ToList": [{"Name": "", "Address": "bob@example.com"}],
            "CCList": null, "Flags": 1, "Time": 1_700_000_000, "Size": 1234,
            "Unread": 1, "IsReplied": 0, "IsRepliedAll": 0, "IsForwarded": 1,
            "NumAttachments": 2,
        }))
        .expect("metadata");
        assert!(meta.starred());
        assert!(!meta.seen());
        assert!(!meta.is_draft());
        assert!(meta.is_forwarded);
        assert_eq!(
            meta.sender.as_ref().map(|s| s.address.as_str()),
            Some("alice@example.com")
        );
        assert!(meta.cc_list.is_empty());

        let draft: MessageMetadata =
            serde_json::from_value(serde_json::json!({"ID": "d", "Flags": 0})).expect("draft");
        assert!(draft.is_draft());
    }

    #[test]
    fn encodes_filters_like_upstream() {
        let filter = MessageFilter {
            end_id: "m9".to_owned(),
            desc: true,
            ..MessageFilter::default()
        };
        assert_eq!(
            serde_json::to_value(&filter).expect("json"),
            serde_json::json!({"EndID": "m9", "Desc": 1})
        );
    }
}
