//! The calls moved from `jmap-client` to `jmap-wire` (ADR 0008, step 1),
//! compared on the wire: each request is built as Zmail built it with
//! `jmap-client`'s builders and as it builds it now, and the two are the
//! same JSON but for the differences this file states. It goes with
//! `jmap-client` in step 4.
//!
//! The differences, all of them:
//!
//! - `using`: identities no longer list the mail capability their calls
//!   do not use; submission and destroying emails list what they did;
//! - method call ids are `c0` where `jmap-client` wrote `s0`;
//! - creation ids are `k0` where `jmap-client` wrote `c0`.
//!
//! Object members come in another order (alphabetical), which JSON
//! gives no meaning to; the comparison ignores it.

use std::io::{BufRead, BufReader, Write};

use serde_json::{Value, json};
use zmail_core::{AuthKind, OutgoingMessage, identity_sync::IdentityValues};

use crate::{Connection, SUBMITTED, all_recipients, identities};

const CORE: &str = "urn:ietf:params:jmap:core";
const MAIL: &str = "urn:ietf:params:jmap:mail";
const SUBMISSION: &str = "urn:ietf:params:jmap:submission";

/// A connection to a server on loopback that only serves the session,
/// which is all building a `jmap-client` request needs.
async fn connection() -> Connection {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let base = format!("http://{}", listener.local_addr().expect("an address"));
    let url = base.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok() && !line.trim().is_empty() {
                line.clear();
            }
            let body = json!({
                "capabilities": {
                    CORE: {
                        "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
                        "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
                        "maxCallsInRequest": 16, "maxObjectsInGet": 500,
                        "maxObjectsInSet": 500, "collationAlgorithms": []
                    },
                    MAIL: {},
                    SUBMISSION: {}
                },
                "accounts": {"a1": {"name": "rob@example.com", "isPersonal": true,
                                     "isReadOnly": false, "accountCapabilities": {}}},
                "primaryAccounts": {MAIL: "a1"},
                "username": "rob@example.com",
                "apiUrl": format!("{url}/api"),
                "downloadUrl": format!("{url}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
                "uploadUrl": format!("{url}/upload/{{accountId}}/"),
                "eventSourceUrl": format!("{url}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
                "state": "s1"
            })
            .to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    crate::connect_client(&base, "rob@example.com", "pw", AuthKind::Password)
        .await
        .expect("the session loads")
}

/// A `jmap-client` request as the JSON it sends, with its call id and
/// creation id renamed to `jmap-wire`'s and its `using` taken out and
/// returned beside it.
fn old(request: &jmap_client::core::request::Request<'_>) -> (Value, Vec<String>) {
    let text = serde_json::to_string(request)
        .expect("encodes")
        .replace("\"s0\"", "\"c0\"")
        .replace("\"c0\":{", "\"k0\":{")
        .replace("\"#c0\"", "\"#k0\"");
    split(serde_json::from_str(&text).expect("JSON"))
}

/// A `jmap-wire` request as the JSON it sends, `using` beside it.
fn new(request: &jmap_wire::Request) -> (Value, Vec<String>) {
    split(serde_json::to_value(request).expect("encodes"))
}

fn split(mut request: Value) -> (Value, Vec<String>) {
    let using = serde_json::from_value(request["using"].take()).expect("using");
    (request, using)
}

fn values(reply_to: &str) -> IdentityValues {
    IdentityValues {
        name: "Rob".to_owned(),
        reply_to: reply_to.to_owned(),
        signature: "-- Rob".to_owned(),
    }
}

/// What `jmap-client` 0.4.1 was made to send for the old identity calls:
/// `jmap-client`'s typed values for an identity's name, signature and
/// reply-to (`null` when empty).
fn set_old_values(
    identity: &mut jmap_client::identity::Identity<jmap_client::Set>,
    values: &IdentityValues,
) {
    let reply_to = zmail_core::participants::parse_address_list(&values.reply_to)
        .into_iter()
        .filter_map(|participant| match (participant.name, participant.email) {
            (Some(name), Some(email)) => {
                Some(jmap_client::email::EmailAddress::from((name, email)))
            }
            (None, Some(email)) => Some(jmap_client::email::EmailAddress::from(email)),
            (_, None) => None,
        })
        .collect::<Vec<_>>();
    identity
        .name(values.name.clone())
        .text_signature(values.signature.clone())
        .reply_to((!reply_to.is_empty()).then_some(reply_to.into_iter()));
}

#[tokio::test]
async fn identity_calls_differ_only_in_no_longer_listing_mail() {
    let client = connection().await;
    let account = client.account_id();
    let old_using = [CORE, MAIL, SUBMISSION].map(str::to_owned).to_vec();
    let new_using = [CORE, SUBMISSION].map(str::to_owned).to_vec();

    let mut request = client.build();
    request.get_identity();
    request.add_capability(jmap_client::URI::Submission);
    let (was, was_using) = old(&request);
    let (is, is_using) = new(&identities::get_request(account).expect("builds"));
    assert_eq!(is, was);
    assert_eq!(
        is,
        json!({"using": null, "methodCalls": [["Identity/get", {"accountId": "a1"}, "c0"]]})
    );
    assert_eq!(
        (was_using, is_using),
        (old_using.clone(), new_using.clone())
    );

    for reply_to in ["Team <team@example.com>, ops@example.com", ""] {
        let values = values(reply_to);

        let mut request = client.build();
        set_old_values(request.set_identity().update("i1"), &values);
        request.add_capability(jmap_client::URI::Submission);
        let (was, was_using) = old(&request);
        let (is, is_using) =
            new(&identities::update_request(account, "i1", &values).expect("builds"));
        assert_eq!(is, was, "update, reply-to {reply_to:?}");
        assert_eq!(
            (was_using, is_using),
            (old_using.clone(), new_using.clone())
        );

        let mut request = client.build();
        let identity = request.set_identity().create();
        identity.email("help@example.com");
        set_old_values(identity, &values);
        request.add_capability(jmap_client::URI::Submission);
        let (was, was_using) = old(&request);
        let (is, is_using) =
            new(&identities::create_request(account, "help@example.com", &values).expect("builds"));
        assert_eq!(is, was, "create, reply-to {reply_to:?}");
        assert_eq!(
            (was_using, is_using),
            (old_using.clone(), new_using.clone())
        );
    }
    // What the cleared reply-to looks like, so the comparison above is
    // known to compare something: an explicit null, in both.
    let (cleared, _) =
        new(&identities::update_request(account, "i1", &values("")).expect("builds"));
    assert_eq!(
        cleared["methodCalls"][0][1]["update"]["i1"],
        json!({"name": "Rob", "replyTo": null, "textSignature": "-- Rob"})
    );

    let mut request = client.build();
    request.set_identity().destroy(["i1"]);
    request.add_capability(jmap_client::URI::Submission);
    let (was, was_using) = old(&request);
    let (is, is_using) = new(&identities::destroy_request(account, "i1").expect("builds"));
    assert_eq!(is, was);
    assert_eq!(
        (was_using, is_using),
        (old_using.clone(), new_using.clone())
    );
}

#[tokio::test]
async fn destroying_emails_is_the_same_request() {
    let client = connection().await;
    let ids = ["e1", "e2"];
    let mut request = client.build();
    request.set_email().destroy(ids.iter().copied());
    let (was, was_using) = old(&request);
    let (is, is_using) =
        new(&crate::destroy_emails_request(client.account_id(), &ids).expect("builds"));
    assert_eq!(is, was);
    assert_eq!(is_using, was_using);
    assert_eq!(is_using, [CORE, MAIL]);
}

#[tokio::test]
async fn a_submission_is_the_same_request_with_the_same_using() {
    use jmap_client::core::set::SetObject;

    let client = connection().await;
    let message = OutgoingMessage {
        from_email: "rob@example.com".to_owned(),
        from_name: None,
        to: vec!["ada@example.com".to_owned()],
        cc: vec!["Cy <cy@example.com>".to_owned()],
        bcc: vec!["Bea <bea@example.com>".to_owned()],
        reply_to: Vec::new(),
        subject: "Lunch".to_owned(),
        text_body: "Noon?".to_owned(),
        attachments: Vec::new(),
        message_id: Some("lunch@example.com".to_owned()),
        in_reply_to: None,
        references: Vec::new(),
        reply_action: None,
    };

    let mut request = client.build();
    let submissions = request.set_email_submission();
    let create_id = submissions
        .create()
        .email_id("e1")
        .identity_id("i1")
        .envelope(message.from_email.as_str(), all_recipients(&message))
        .create_id()
        .expect("a creation id");
    submissions
        .arguments()
        .on_success_update_email(&create_id)
        .keywords([SUBMITTED]);
    let (was, was_using) = old(&request);

    let (request, create_id) =
        crate::submission_request(client.account_id(), "e1", "i1", &message).expect("builds");
    let (is, is_using) = new(&request);
    assert_eq!(create_id, "k0");
    assert_eq!(is, was);
    // The implicit `Email/set` of `onSuccessUpdateEmail` is a mail
    // method, so mail is still listed, after submission now.
    assert_eq!(was_using, [CORE, MAIL, SUBMISSION]);
    assert_eq!(is_using, [CORE, SUBMISSION, MAIL]);
    assert_eq!(
        is["methodCalls"][0][1],
        json!({
            "accountId": "a1",
            "create": {"k0": {
                "identityId": "i1",
                "emailId": "e1",
                "envelope": {
                    "mailFrom": {"email": "rob@example.com", "parameters": null},
                    "rcptTo": [
                        {"email": "ada@example.com", "parameters": null},
                        {"email": "cy@example.com", "parameters": null},
                        {"email": "bea@example.com", "parameters": null}
                    ]
                }
            }},
            "onSuccessUpdateEmail": {"#k0": {"keywords": {"$submitted": true}}}
        })
    );
}
