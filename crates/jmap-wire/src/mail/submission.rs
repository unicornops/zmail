//! Email submissions (RFC 8621 section 7): sending an Email.

use std::{borrow::Cow, collections::BTreeMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::UtcDate;
use crate::{
    capability,
    error::error_types,
    method::{GetObject, Object, PatchObject, Properties, SetArguments, SetObject},
};

/// The EmailSubmission data type (RFC 8621 section 7), for
/// [`Get`](crate::Get)`<EmailSubmission>` and
/// [`Set`](crate::Set)`<EmailSubmission>`.
///
/// Its methods need the submission capability. A `/set` that also
/// changes or destroys the Email on success
/// ([`EmailSubmissionSetArguments`]) makes the server run an `Email/set`
/// too, so it needs the mail capability as well.
///
/// ```
/// use jmap_wire::{
///     PatchObject, Request, Set, capability,
///     mail::{EmailSubmission, Envelope, NewEmailSubmission},
/// };
///
/// let mut set = Set::<EmailSubmission>::new("u1");
/// let creation_id = set.create(
///     NewEmailSubmission::new("identity-1", "email-1")
///         .envelope(Envelope::new("john@example.com", ["jane@example.com"])),
/// );
/// let mut request = Request::new();
/// assert_eq!(request.using(), [capability::CORE]);
///
/// // Once sent, take the draft keyword off the email.
/// set.arguments().on_success_update_email(
///     format!("#{creation_id}"),
///     PatchObject::new().remove(PatchObject::path(["keywords", "$draft"])),
/// );
/// request.call(set)?;
/// assert_eq!(
///     request.using(),
///     [capability::CORE, capability::SUBMISSION, capability::MAIL]
/// );
/// # Ok::<(), jmap_wire::Error>(())
/// ```
#[derive(Debug, Clone, Copy)]
pub enum EmailSubmission {}

impl Object for EmailSubmission {
    const NAME: &'static str = "EmailSubmission";
    const CAPABILITY: &'static str = capability::SUBMISSION;
}

impl GetObject for EmailSubmission {
    type Record = EmailSubmissionRecord;
}

impl SetObject for EmailSubmission {
    type Create = NewEmailSubmission;
    type Created = EmailSubmissionRecord;
    type Arguments = EmailSubmissionSetArguments;
}

/// The arguments `EmailSubmission/set` has beyond the standard ones
/// (RFC 8621 section 7.5): what to do to each submission's Email once
/// the submission is created. The server does it with an implicit
/// `Email/set`, whose answer follows the submission's under the same
/// call id ([`Response::responses_to`](crate::Response::responses_to)).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailSubmissionSetArguments {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    on_success_update_email: BTreeMap<String, PatchObject>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    on_success_destroy_email: Vec<String>,
}

impl EmailSubmissionSetArguments {
    /// When the submission `submission` is created, changes its Email as
    /// `patch` says. `submission` is an EmailSubmission id, or `#` and a
    /// creation id of this call.
    pub fn on_success_update_email(
        &mut self,
        submission: impl Into<String>,
        patch: PatchObject,
    ) -> &mut Self {
        self.on_success_update_email
            .insert(submission.into(), patch);
        self
    }

    /// When the submission `submission` is created, destroys its Email.
    /// `submission` is an EmailSubmission id, or `#` and a creation id
    /// of this call.
    pub fn on_success_destroy_email(&mut self, submission: impl Into<String>) -> &mut Self {
        self.on_success_destroy_email.push(submission.into());
        self
    }
}

impl SetArguments for EmailSubmissionSetArguments {
    fn using(&self) -> Vec<Cow<'static, str>> {
        if self.on_success_update_email.is_empty() && self.on_success_destroy_email.is_empty() {
            Vec::new()
        } else {
            vec![Cow::Borrowed(capability::MAIL)]
        }
    }
}

/// An address of an SMTP envelope with its parameters (RFC 8621 section
/// 7): a `Mailbox` of RFC 5321, without a display name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    /// The address.
    pub email: String,
    /// The `MAIL FROM` or `RCPT TO` parameters, by name; a value is
    /// `None` for a parameter that takes none. Written as `null` when
    /// there are no parameters.
    #[serde(default)]
    pub parameters: Option<BTreeMap<String, Option<String>>>,
}

impl Address {
    /// An address without parameters.
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            parameters: None,
        }
    }
}

/// The SMTP envelope of a submission (RFC 8621 section 7). Without one,
/// the server makes it from the Email's headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    /// The return path, `MAIL FROM`.
    pub mail_from: Address,
    /// The recipients, `RCPT TO`.
    pub rcpt_to: Vec<Address>,
}

impl Envelope {
    /// An envelope from `mail_from` to `rcpt_to`, none with parameters.
    pub fn new(
        mail_from: impl Into<String>,
        rcpt_to: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            mail_from: Address::new(mail_from),
            rcpt_to: rcpt_to.into_iter().map(Address::new).collect(),
        }
    }
}

/// A submission to create (RFC 8621 section 7.5): send an Email as an
/// Identity.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NewEmailSubmission(Properties);

impl NewEmailSubmission {
    /// Sends the Email `email_id` as the Identity `identity_id`. Either
    /// may be `#` and a creation id from earlier in the request.
    pub fn new(identity_id: impl Into<String>, email_id: impl Into<String>) -> Self {
        let mut properties = Properties::new();
        properties.set("identityId", identity_id.into());
        properties.set("emailId", email_id.into());
        Self(properties)
    }

    /// The SMTP envelope to send with, instead of the one the server
    /// would make from the Email's headers.
    pub fn envelope(mut self, envelope: Envelope) -> Self {
        self.0.set_as("envelope", &envelope);
        self
    }

    /// Sets any other property.
    pub fn property(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.set(name, value);
        self
    }
}

error_types! {
    /// Whether a submission can still be cancelled (RFC 8621 section 7,
    /// `undoStatus`).
    UndoStatus {
        /// It may be possible to cancel it.
        Pending => "pending",
        /// It has gone, or the server cannot unsend.
        Final => "final",
        /// It was cancelled and will not be delivered.
        Canceled => "canceled",
    }
}

/// What is known of the delivery to one recipient (RFC 8621 section 7,
/// `deliveryStatus`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DeliveryStatus {
    /// The SMTP reply for the recipient.
    #[serde(default)]
    pub smtp_reply: String,
    /// `queued`, `yes`, `no` or `unknown`.
    #[serde(default)]
    pub delivered: String,
    /// `unknown` or `yes`.
    #[serde(default)]
    pub displayed: String,
}

/// A submission as the server returns it (RFC 8621 section 7). Every
/// property is optional, as for
/// [`IdentityRecord`](super::IdentityRecord).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct EmailSubmissionRecord {
    /// The submission's id.
    #[serde(default)]
    pub id: Option<String>,
    /// The Identity it was sent as.
    #[serde(default)]
    pub identity_id: Option<String>,
    /// The Email that was sent.
    #[serde(default)]
    pub email_id: Option<String>,
    /// The Thread of that Email.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// The SMTP envelope it was sent with.
    #[serde(default)]
    pub envelope: Option<Envelope>,
    /// When it was, or will be, released for delivery.
    #[serde(default)]
    pub send_at: Option<UtcDate>,
    /// Whether it can still be cancelled.
    #[serde(default)]
    pub undo_status: Option<UndoStatus>,
    /// The delivery status by recipient address, where the server
    /// tracks it.
    #[serde(default)]
    pub delivery_status: Option<BTreeMap<String, DeliveryStatus>>,
    /// The blobs of the delivery status notifications received.
    #[serde(default)]
    pub dsn_blob_ids: Option<Vec<String>>,
    /// The blobs of the disposition notifications received.
    #[serde(default)]
    pub mdn_blob_ids: Option<Vec<String>>,
    /// Properties this crate has no field for.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{Error, Request, Response, Set, error::SetErrorType};

    /// The request of RFC 8621 section 7.5.1, with the one recipient the
    /// RFC spells out.
    fn rfc_request() -> (Request, crate::Call<Set<EmailSubmission>>) {
        let mut set = Set::<EmailSubmission>::new("ue411d190");
        set.create_as(
            "k1490",
            NewEmailSubmission::new("I64588216", "M7f6ed5bcfd7e2604d1753f6c")
                .envelope(Envelope::new("john@example.com", ["jane@example.com"])),
        );
        set.arguments().on_success_update_email(
            "#k1490",
            PatchObject::new()
                .remove(PatchObject::path([
                    "mailboxIds",
                    "7cb4e8ee-df87-4757-b9c4-2ea1ca41b38e",
                ]))
                .set(
                    PatchObject::path(["mailboxIds", "73dbcb4b-bffc-48bd-8c2a-a2e91ca672f6"]),
                    true,
                )
                .remove(PatchObject::path(["keywords", "$draft"])),
        );
        let mut request = Request::new();
        let call = request.call(set).expect("encodes");
        (request, call)
    }

    #[test]
    fn the_submission_of_rfc_8621_section_7_5_1_is_written_as_the_rfc_has_it() {
        let (request, _) = rfc_request();
        assert_eq!(
            serde_json::to_value(&request).expect("encodes"),
            json!({
                "using": [capability::CORE, capability::SUBMISSION, capability::MAIL],
                "methodCalls": [["EmailSubmission/set", {
                    "accountId": "ue411d190",
                    "create": {
                        "k1490": {
                            "identityId": "I64588216",
                            "emailId": "M7f6ed5bcfd7e2604d1753f6c",
                            "envelope": {
                                "mailFrom": {"email": "john@example.com", "parameters": null},
                                "rcptTo": [{"email": "jane@example.com", "parameters": null}]
                            }
                        }
                    },
                    "onSuccessUpdateEmail": {
                        "#k1490": {
                            "mailboxIds/7cb4e8ee-df87-4757-b9c4-2ea1ca41b38e": null,
                            "mailboxIds/73dbcb4b-bffc-48bd-8c2a-a2e91ca672f6": true,
                            "keywords/$draft": null
                        }
                    }
                }, "c0"]]
            })
        );
    }

    /// The two responses of RFC 8621 section 7.5.1: accepted, with the
    /// implicit `Email/set` answer under the same call id, and refused.
    #[test]
    fn the_answers_of_rfc_8621_section_7_5_1_are_read() {
        let (_, call) = rfc_request();
        let accepted: Response = serde_json::from_value(json!({
            "methodResponses": [
                ["EmailSubmission/set", {
                    "accountId": "ue411d190",
                    "oldState": "012421s6-8nrq-4ps4-n0p4-9330r951ns21",
                    "newState": "355421f6-8aed-4cf4-a0c4-7377e951af36",
                    "created": {"k1490": {"id": "ES-3bab7f9a-623e-4acf-99a5-2e67facb02a0"}}
                }, "c0"],
                ["Email/set", {
                    "accountId": "ue411d190",
                    "oldState": "778193",
                    "newState": "778197",
                    "updated": {"M7f6ed5bcfd7e2604d1753f6c": null}
                }, "c0"]
            ],
            "sessionState": "s"
        }))
        .expect("decodes");
        let answer = accepted.get(&call).expect("answered");
        assert_eq!(
            answer.created["k1490"].id.as_deref(),
            Some("ES-3bab7f9a-623e-4acf-99a5-2e67facb02a0")
        );
        let implicit = accepted
            .responses_to(call.call_id())
            .nth(1)
            .expect("the implicit Email/set");
        assert_eq!(implicit.name, "Email/set");

        let refused: Response = serde_json::from_value(json!({
            "methodResponses": [["EmailSubmission/set", {
                "accountId": "ue411d190",
                "oldState": "012421s6-8nrq-4ps4-n0p4-9330r951ns21",
                "newState": "012421s6-8nrq-4ps4-n0p4-9330r951ns21",
                "notCreated": {"k1490": {
                    "type": "forbiddenToSend",
                    "description": "Verzeihung, wegen verdaechtiger Aktivitaeten Ihres \
                                    Benutzerkontos haben wir den Versand von Nachrichten gesperrt."
                }}
            }, "c0"]],
            "sessionState": "s"
        }))
        .expect("decodes");
        let answer = refused.get(&call).expect("answered");
        assert!(answer.created.is_empty());
        let error = &answer.not_created["k1490"];
        assert_eq!(error.error_type, SetErrorType::ForbiddenToSend);
        assert!(
            error
                .description
                .as_deref()
                .is_some_and(|text| text.starts_with("Verzeihung"))
        );

        let failed: Response = serde_json::from_value(json!({
            "methodResponses": [["error", {"type": "accountNotFound"}, "c0"]],
            "sessionState": "s"
        }))
        .expect("decodes");
        assert!(matches!(failed.get(&call), Err(Error::Method(_))));
    }

    #[test]
    fn a_submission_alone_needs_only_the_submission_capability() {
        let mut set = Set::<EmailSubmission>::new("a1");
        set.create(NewEmailSubmission::new("i1", "e1"));
        let mut request = Request::new();
        request.call(set).expect("encodes");
        assert_eq!(request.using(), [capability::CORE, capability::SUBMISSION]);

        let mut set = Set::<EmailSubmission>::new("a1");
        set.create(NewEmailSubmission::new("i1", "e1"));
        set.arguments().on_success_destroy_email("#k0");
        let mut request = Request::new();
        request.call(set).expect("encodes");
        assert_eq!(
            request.using(),
            [capability::CORE, capability::SUBMISSION, capability::MAIL]
        );
        assert_eq!(
            request.method_calls()[0].arguments["onSuccessDestroyEmail"],
            json!(["#k0"])
        );
    }

    #[test]
    fn a_submission_record_reads_its_status_and_keeps_unknown_ones() {
        let record: EmailSubmissionRecord = serde_json::from_value(json!({
            "id": "ES1",
            "sendAt": "2026-11-23T10:00:00Z",
            "undoStatus": "final",
            "deliveryStatus": {"jane@example.com": {
                "smtpReply": "250 2.0.0 OK", "delivered": "yes", "displayed": "unknown"
            }},
            "envelope": {"mailFrom": {"email": "john@example.com",
                                      "parameters": {"RET": "HDRS", "SMTPUTF8": null}},
                         "rcptTo": []}
        }))
        .expect("decodes");
        assert_eq!(
            record.send_at.as_ref().map(UtcDate::as_str),
            Some("2026-11-23T10:00:00Z")
        );
        assert_eq!(record.undo_status, Some(UndoStatus::Final));
        let status = &record.delivery_status.expect("status")["jane@example.com"];
        assert_eq!(status.delivered, "yes");
        let parameters = record
            .envelope
            .expect("envelope")
            .mail_from
            .parameters
            .expect("some");
        assert_eq!(parameters["SMTPUTF8"], None);

        let later: EmailSubmissionRecord =
            serde_json::from_value(json!({"undoStatus": "scheduled"})).expect("decodes");
        assert_eq!(
            later.undo_status,
            Some(UndoStatus::Other("scheduled".to_owned()))
        );
    }
}
