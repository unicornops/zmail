// Ported from go-proton-api `message_send.go` (`CreateDraft`,
// `UpdateDraft`, `SendDraft`) and `message_draft_types.go`
// (`DraftTemplate`, `CreateDraftAction`, `CreateDraftReq`,
// `UpdateDraftReq`) at 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust on rPGP. The
// caller passes the address's first key instead of the address keyring.
// Empty address lists and attachment key packets are sent as `null`, as
// Go encodes nil slices.

//! Drafts, and sending them.

use serde::{Serialize, Serializer};

use crate::{
    client::Client, keyring::UnlockedKey, manager::Request, message::EmailAddress,
    message::Message, package::SendDraftReq, response::Error,
};

/// A draft's content (`DraftTemplate`). The body is in the clear here;
/// it is encrypted to the sender's key when the draft is saved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DraftTemplate {
    /// Subject.
    #[serde(rename = "Subject")]
    pub subject: String,
    /// The sending address.
    #[serde(rename = "Sender")]
    pub sender: EmailAddress,
    /// To recipients.
    #[serde(rename = "ToList", serialize_with = "nil_if_empty")]
    pub to_list: Vec<EmailAddress>,
    /// Cc recipients.
    #[serde(rename = "CCList", serialize_with = "nil_if_empty")]
    pub cc_list: Vec<EmailAddress>,
    /// Bcc recipients.
    #[serde(rename = "BCCList", serialize_with = "nil_if_empty")]
    pub bcc_list: Vec<EmailAddress>,
    /// The body.
    #[serde(rename = "Body")]
    pub body: String,
    /// The body's format: `text/plain` or `text/html`.
    #[serde(rename = "MIMEType")]
    pub mime_type: String,
    /// Whether the draft is unread.
    #[serde(rename = "Unread", serialize_with = "api_bool")]
    pub unread: bool,
    /// The `Message-ID` the draft will be sent with, if chosen by the
    /// client.
    #[serde(rename = "ExternalID", skip_serializing_if = "String::is_empty")]
    pub external_id: String,
}

/// What a draft answers (`CreateDraftAction`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(into = "i64")]
pub enum CreateDraftAction {
    /// A reply (`ReplyAction`).
    Reply,
    /// A reply to all (`ReplyAllAction`).
    ReplyAll,
    /// A forward, and Bridge's choice for a new message (`ForwardAction`).
    #[default]
    Forward,
    /// An automatic response (`AutoResponseAction`).
    AutoResponse,
    /// A read receipt (`ReadReceiptAction`).
    ReadReceipt,
}

impl From<CreateDraftAction> for i64 {
    fn from(action: CreateDraftAction) -> Self {
        match action {
            CreateDraftAction::Reply => 0,
            CreateDraftAction::ReplyAll => 1,
            CreateDraftAction::Forward => 2,
            CreateDraftAction::AutoResponse => 3,
            CreateDraftAction::ReadReceipt => 4,
        }
    }
}

/// A new draft (`CreateDraftReq`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CreateDraftReq {
    /// The draft.
    #[serde(rename = "Message")]
    pub message: DraftTemplate,
    /// Key packets of attachments carried over from the parent.
    #[serde(rename = "AttachmentKeyPackets", serialize_with = "nil_if_empty")]
    pub attachment_key_packets: Vec<String>,
    /// The message the draft answers.
    #[serde(rename = "ParentID", skip_serializing_if = "String::is_empty")]
    pub parent_id: String,
    /// How it answers the parent.
    #[serde(rename = "Action")]
    pub action: CreateDraftAction,
}

/// A draft update (`UpdateDraftReq`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UpdateDraftReq {
    /// The draft; an empty body leaves the saved body as it is.
    #[serde(rename = "Message")]
    pub message: DraftTemplate,
    /// Key packets of attachments carried over from the parent.
    #[serde(rename = "AttachmentKeyPackets", serialize_with = "nil_if_empty")]
    pub attachment_key_packets: Vec<String>,
}

fn nil_if_empty<S: Serializer, T: Serialize>(
    values: &[T],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if values.is_empty() {
        serializer.serialize_none()
    } else {
        values.serialize(serializer)
    }
}

fn api_bool<S: Serializer>(value: &bool, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_i64(i64::from(*value))
}

#[derive(serde::Deserialize)]
struct MessageRes {
    #[serde(rename = "Message")]
    message: Message,
}

impl Client {
    /// Saves a new draft, its body encrypted (unsigned) to the address's
    /// first key (`CreateDraft`).
    pub async fn create_draft(
        &self,
        key: &UnlockedKey,
        mut req: CreateDraftReq,
    ) -> Result<Message, Error> {
        req.message.body = key.encrypt_text_armored(&req.message.body)?;
        let res: MessageRes = self
            .json(&Request::post("/mail/v4/messages").json(&req)?)
            .await?;
        Ok(res.message)
    }

    /// Updates a draft (`UpdateDraft`); a new body is encrypted to the
    /// address's first key.
    pub async fn update_draft(
        &self,
        draft_id: &str,
        key: &UnlockedKey,
        mut req: UpdateDraftReq,
    ) -> Result<Message, Error> {
        if !req.message.body.is_empty() {
            req.message.body = key.encrypt_text_armored(&req.message.body)?;
        }
        let res: MessageRes = self
            .json(&Request::put(format!("/mail/v4/messages/{draft_id}")).json(&req)?)
            .await?;
        Ok(res.message)
    }

    /// Sends a draft with its packages (`SendDraft`) and returns the sent
    /// message.
    pub async fn send_draft(&self, draft_id: &str, req: &SendDraftReq) -> Result<Message, Error> {
        #[derive(serde::Deserialize)]
        struct Res {
            #[serde(rename = "Sent")]
            sent: Message,
        }

        let res: Res = self
            .json(&Request::post(format!("/mail/v4/messages/{draft_id}")).json(req)?)
            .await?;
        Ok(res.sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_drafts_as_go_does() {
        let req = CreateDraftReq {
            message: DraftTemplate {
                subject: "Hi".to_owned(),
                sender: EmailAddress {
                    name: "Zmail".to_owned(),
                    address: "zmail@proton.local".to_owned(),
                },
                to_list: vec![EmailAddress {
                    name: String::new(),
                    address: "a@example.com".to_owned(),
                }],
                body: "armored".to_owned(),
                mime_type: "text/plain".to_owned(),
                ..DraftTemplate::default()
            },
            ..CreateDraftReq::default()
        };
        assert_eq!(
            serde_json::to_value(&req).expect("json"),
            serde_json::json!({
                "Message": {
                    "Subject": "Hi",
                    "Sender": {"Name": "Zmail", "Address": "zmail@proton.local"},
                    "ToList": [{"Name": "", "Address": "a@example.com"}],
                    "CCList": null,
                    "BCCList": null,
                    "Body": "armored",
                    "MIMEType": "text/plain",
                    "Unread": 0,
                },
                "AttachmentKeyPackets": null,
                "Action": 2,
            })
        );
    }
}
