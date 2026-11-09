// Ported from go-proton-api `label.go` (`GetLabels`, `CreateLabel`,
// `UpdateLabel`, `DeleteLabel`) and `label_types.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust. Modified on
// 2026-11-09: creating, updating and deleting labels ported (the live
// tests change labels from another client with them).

//! Labels and folders, and Proton's system labels.

use serde::{Deserialize, Deserializer, Serialize};

use crate::{client::Client, manager::Request, response::Error};

/// The Inbox.
pub const INBOX_LABEL: &str = "0";
/// Every draft, wherever it is.
pub const ALL_DRAFTS_LABEL: &str = "1";
/// Every sent message, wherever it is.
pub const ALL_SENT_LABEL: &str = "2";
/// Trash.
pub const TRASH_LABEL: &str = "3";
/// Spam.
pub const SPAM_LABEL: &str = "4";
/// All mail.
pub const ALL_MAIL_LABEL: &str = "5";
/// Archive.
pub const ARCHIVE_LABEL: &str = "6";
/// Sent.
pub const SENT_LABEL: &str = "7";
/// Drafts.
pub const DRAFTS_LABEL: &str = "8";
/// Outbox.
pub const OUTBOX_LABEL: &str = "9";
/// Starred.
pub const STARRED_LABEL: &str = "10";
/// Scheduled messages.
pub const ALL_SCHEDULED_LABEL: &str = "12";

/// What a label is (go-proton-api's `LabelType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "i64")]
pub enum LabelType {
    /// A user label (a message can have many).
    Label,
    /// A contact group.
    ContactGroup,
    /// A user folder (a message is in at most one).
    Folder,
    /// A system label such as the Inbox.
    System,
    /// A type this client does not know.
    Other(i64),
}

impl From<i64> for LabelType {
    fn from(value: i64) -> Self {
        match value {
            1 => Self::Label,
            2 => Self::ContactGroup,
            3 => Self::Folder,
            4 => Self::System,
            other => Self::Other(other),
        }
    }
}

impl LabelType {
    fn query_value(self) -> i64 {
        match self {
            Self::Label => 1,
            Self::ContactGroup => 2,
            Self::Folder => 3,
            Self::System => 4,
            Self::Other(value) => value,
        }
    }
}

/// A label, folder or system label (go-proton-api's `Label`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Label {
    /// Label ID; system labels have the fixed IDs above.
    #[serde(rename = "ID")]
    pub id: String,
    /// Parent folder ID, for nested folders.
    #[serde(rename = "ParentID", default)]
    pub parent_id: String,
    /// Display name.
    #[serde(rename = "Name")]
    pub name: String,
    /// Full path, split on `/` like upstream (empty when the API sends
    /// none).
    #[serde(rename = "Path", default, deserialize_with = "split_path")]
    pub path: Vec<String>,
    /// Colour, such as `#7272a7`.
    #[serde(rename = "Color", default)]
    pub color: String,
    /// Type.
    #[serde(rename = "Type")]
    pub kind: LabelType,
}

fn split_path<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let path = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    if path.is_empty() {
        return Ok(Vec::new());
    }
    Ok(path.split('/').map(str::to_owned).collect())
}

/// A new label or folder (go-proton-api's `CreateLabelReq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreateLabelReq {
    /// Name.
    #[serde(rename = "Name")]
    pub name: String,
    /// Colour, such as `#7272a7`.
    #[serde(rename = "Color")]
    pub color: String,
    /// Label or folder.
    #[serde(rename = "Type", serialize_with = "serialize_label_type")]
    pub kind: LabelType,
    /// Parent folder, for a nested folder.
    #[serde(rename = "ParentID", skip_serializing_if = "String::is_empty")]
    pub parent_id: String,
}

/// A label's new name, colour and parent (go-proton-api's
/// `UpdateLabelReq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateLabelReq {
    /// Name.
    #[serde(rename = "Name")]
    pub name: String,
    /// Colour, such as `#7272a7`.
    #[serde(rename = "Color")]
    pub color: String,
    /// Parent folder, for a nested folder.
    #[serde(rename = "ParentID", skip_serializing_if = "String::is_empty")]
    pub parent_id: String,
}

fn serialize_label_type<S: serde::Serializer>(
    kind: &LabelType,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_i64(kind.query_value())
}

#[derive(Deserialize)]
struct LabelRes {
    #[serde(rename = "Label")]
    label: Label,
}

impl Client {
    /// Creates a label or folder (`CreateLabel`).
    pub async fn create_label(&self, req: &CreateLabelReq) -> Result<Label, Error> {
        let request = Request::post("/core/v4/labels").json(req)?;
        let res: LabelRes = self.json(&request).await?;
        Ok(res.label)
    }

    /// Renames, recolours or moves a label (`UpdateLabel`).
    pub async fn update_label(&self, label_id: &str, req: &UpdateLabelReq) -> Result<Label, Error> {
        let request = Request::put(format!("/core/v4/labels/{label_id}")).json(req)?;
        let res: LabelRes = self.json(&request).await?;
        Ok(res.label)
    }

    /// Deletes a label (`DeleteLabel`).
    pub async fn delete_label(&self, label_id: &str) -> Result<(), Error> {
        self.send(&Request::delete(format!("/core/v4/labels/{label_id}")))
            .await?;
        Ok(())
    }

    /// Returns the labels of the given types, in that order (`GetLabels`).
    pub async fn get_labels(&self, types: &[LabelType]) -> Result<Vec<Label>, Error> {
        #[derive(Deserialize)]
        struct Res {
            #[serde(rename = "Labels")]
            labels: Vec<Label>,
        }
        let mut labels = Vec::new();
        for kind in types {
            let request = Request::get("/core/v4/labels").query("Type", kind.query_value());
            let res: Res = self.json(&request).await?;
            labels.extend(res.labels);
        }
        Ok(labels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_labels() {
        let labels: Vec<Label> = serde_json::from_value(serde_json::json!([
            {"ID": "0", "Name": "Inbox", "Path": "Inbox", "Type": 4, "Color": ""},
            {"ID": "f1", "ParentID": "f0", "Name": "Sub", "Path": "Work/Sub", "Type": 3, "Color": "#fff"},
            {"ID": "x", "Name": "Odd", "Type": 9},
        ]))
        .expect("labels");
        assert_eq!(labels[0].kind, LabelType::System);
        assert_eq!(labels[1].path, ["Work", "Sub"]);
        assert_eq!(labels[1].parent_id, "f0");
        assert_eq!(labels[1].kind, LabelType::Folder);
        assert_eq!(labels[2].kind, LabelType::Other(9));
        assert!(labels[2].path.is_empty());
    }

    #[test]
    fn encodes_label_requests_like_go_proton_api() {
        let create = CreateLabelReq {
            name: "Trips".to_owned(),
            color: "#7272a7".to_owned(),
            kind: LabelType::Folder,
            parent_id: String::new(),
        };
        assert_eq!(
            serde_json::to_value(&create).expect("json"),
            serde_json::json!({"Name": "Trips", "Color": "#7272a7", "Type": 3})
        );
        let update = UpdateLabelReq {
            name: "Holidays".to_owned(),
            color: "#7272a7".to_owned(),
            parent_id: "f0".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&update).expect("json"),
            serde_json::json!({"Name": "Holidays", "Color": "#7272a7", "ParentID": "f0"})
        );
    }
}
