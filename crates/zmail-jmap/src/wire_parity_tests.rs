//! The calls moved from `jmap-client` to `jmap-wire` (ADR 0008, steps 1
//! and 2), compared on the wire: each request is built as Zmail built it
//! with `jmap-client`'s builders and as it builds it now, and the two
//! are the same JSON but for the differences this file states. It goes
//! with `jmap-client` in step 4.
//!
//! The differences, all of them:
//!
//! - `using`: identities no longer list the mail capability their calls
//!   do not use; submission and destroying emails list what they did;
//! - method call ids are `c0` where `jmap-client` wrote `s0`;
//! - creation ids are `k0` where `jmap-client` wrote `c0`.
//!
//! The reads (`Mailbox/get`, `Email/query`, `Email/get`,
//! `Email/changes`) differ in the call id alone: their arguments and
//! their `using` (core and mail) are what `jmap-client` sent.
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

// ---- The reads (ADR 0008, step 2) ------------------------------------

/// A `jmap-wire` request with `method` as its only call.
fn wire(method: impl jmap_wire::Method) -> jmap_wire::Request {
    let mut request = jmap_wire::Request::new();
    request.call(method).expect("encodes");
    request
}

/// The arguments of a request's only method call.
fn arguments(request: &Value) -> &Value {
    &request["methodCalls"][0][1]
}

#[tokio::test]
async fn mailbox_get_is_the_same_request() {
    let client = connection().await;
    let mut request = client.build();
    request.get_mailbox();
    let (was, was_using) = old(&request);
    let (is, is_using) = new(&wire(crate::mailboxes_get(client.account_id())));
    assert_eq!(is, was);
    assert_eq!(
        is,
        json!({"using": null, "methodCalls": [["Mailbox/get", {"accountId": "a1"}, "c0"]]})
    );
    assert_eq!(
        (was_using, is_using),
        (
            vec![CORE.to_owned(), MAIL.to_owned()],
            vec![CORE.to_owned(), MAIL.to_owned()]
        )
    );
}

#[tokio::test]
async fn email_queries_are_the_same_requests() {
    use jmap_client::email::query::{Comparator, Filter};

    let client = connection().await;
    let account = client.account_id();

    // The sync's paging, oldest first.
    for position in [0, 250] {
        let mut request = client.build();
        request
            .query_email()
            .sort([Comparator::received_at()])
            .position(position)
            .limit(crate::QUERY_PAGE_SIZE);
        let (was, was_using) = old(&request);
        let (is, is_using) = new(&wire(crate::sync_query(account, i64::from(position))));
        assert_eq!(is, was, "position {position}");
        assert_eq!(is_using, was_using);
    }
    let (first_page, _) = new(&wire(crate::sync_query(account, 0)));
    assert_eq!(
        arguments(&first_page),
        &json!({
            "accountId": "a1",
            "sort": [{"property": "receivedAt", "isAscending": true}],
            "position": 0,
            "limit": 250
        })
    );

    // Finding an interrupted send's copy, newest first, since a time or
    // not.
    for since in [Some(1_792_060_920), None] {
        let mut request = client.build();
        let query = request.query_email();
        if let Some(since) = since {
            query.filter(Filter::after(since - crate::SUBMISSION_CLOCK_SLACK));
        }
        query
            .sort([Comparator::received_at().descending()])
            .position(250)
            .limit(crate::QUERY_PAGE_SIZE);
        let (was, was_using) = old(&request);
        let (is, is_using) = new(&wire(crate::copies_query(account, since, 250)));
        assert_eq!(is, was, "since {since:?}");
        assert_eq!(is_using, was_using);
    }
    let (since, _) = new(&wire(crate::copies_query(account, Some(1_792_060_920), 0)));
    assert_eq!(
        arguments(&since)["filter"],
        json!({"after": "2026-10-14T10:42:00Z"})
    );
}

/// The filter a search was given while it was built with `jmap-client`,
/// kept here to compare with.
fn old_search_filter(
    query: &zmail_core::search::SearchQuery,
    mailboxes: &[crate::JmapMailbox],
    now: &zmail_core::time::ZonedDateTime,
) -> Option<jmap_client::core::query::Filter<jmap_client::email::query::Filter>> {
    use jmap_client::{core::query::Filter as Operator, email::query::Filter};
    use zmail_core::search::{SearchTerm, day_start_unix, mailbox_matches};

    let today = zmail_core::time::Date::of(now);
    let zone = now.time_zone();
    let mut conditions: Vec<Operator<Filter>> = Vec::new();
    for clause in &query.clauses {
        let condition: Operator<Filter> = match &clause.term {
            SearchTerm::Text(text)
            | SearchTerm::From(text)
            | SearchTerm::To(text)
            | SearchTerm::Subject(text) => {
                if !text.is_searchable() {
                    continue;
                }
                let value = text.value.clone();
                match &clause.term {
                    SearchTerm::Text(_) => Filter::text(value),
                    SearchTerm::From(_) => Filter::from(value),
                    SearchTerm::To(_) => Filter::to(value),
                    _ => Filter::subject(value),
                }
                .into()
            }
            SearchTerm::HasAttachment => Filter::has_attachment(true).into(),
            SearchTerm::Unread => Filter::not_keyword("$seen").into(),
            SearchTerm::Read => Filter::has_keyword("$seen").into(),
            SearchTerm::Flagged => Filter::has_keyword("$flagged").into(),
            SearchTerm::Mailbox(value) => {
                let ids = mailboxes
                    .iter()
                    .filter(|mailbox| {
                        mailbox_matches(value, &mailbox.name, mailbox.role.as_deref())
                    })
                    .map(|mailbox| Filter::in_mailbox(mailbox.id.clone()))
                    .collect::<Vec<_>>();
                if ids.is_empty() {
                    if clause.negated {
                        continue;
                    }
                    return None;
                }
                Operator::or(ids)
            }
            SearchTerm::Before(spec) | SearchTerm::After(spec) => {
                let Some(boundary) = spec
                    .resolve(today)
                    .and_then(|day| day_start_unix(day, zone))
                else {
                    continue;
                };
                if matches!(clause.term, SearchTerm::Before(_)) {
                    Filter::before(boundary).into()
                } else {
                    Filter::after(boundary).into()
                }
            }
        };
        conditions.push(if clause.negated {
            Operator::not([condition])
        } else {
            condition
        });
    }
    Some(Operator::and(conditions))
}

#[tokio::test]
async fn a_server_search_is_the_same_request() {
    use jmap_client::email::query::Comparator;

    let client = connection().await;
    let mailboxes =
        [("mb-inbox", "Inbox", "inbox"), ("mb-junk", "Bulk", "junk")].map(|(id, name, role)| {
            crate::JmapMailbox {
                id: id.to_owned(),
                name: name.to_owned(),
                role: Some(role.to_owned()),
                sort_order: 0,
                parent: None,
            }
        });
    let now = zmail_core::time::clock_at("2026-06-15T12:00:00Z").expect("a time");
    let searches = [
        "budget",
        "budget from:alice to:bob subject:plan",
        "-is:unread is:read is:flagged has:attachment",
        "in:inbox -in:spam -in:nowhere",
        "after:2026-06-01 before:2026-06-10 -after:2025-01-01",
        "\"exact phrase\" -budget",
        "",
    ];
    for search in searches {
        let query = zmail_core::search::SearchQuery::parse(search);
        let was_filter = old_search_filter(&query, &mailboxes, &now).expect("a filter");
        let is_filter = crate::search_filter(&query, &mailboxes, &now).expect("a filter");

        let mut request = client.build();
        request
            .query_email()
            .filter(was_filter)
            .sort([Comparator::received_at().descending()])
            .limit(40);
        let (was, was_using) = old(&request);
        let (is, is_using) = new(&wire(crate::search_query(
            client.account_id(),
            is_filter,
            40,
        )));
        assert_eq!(is, was, "{search:?}");
        assert_eq!(is_using, was_using);
    }
    let nowhere = zmail_core::search::SearchQuery::parse("in:nowhere");
    assert!(old_search_filter(&nowhere, &mailboxes, &now).is_none());
    assert!(crate::search_filter(&nowhere, &mailboxes, &now).is_none());
}

#[tokio::test]
async fn email_gets_are_the_same_requests() {
    use jmap_client::email::{Header, Property};

    let client = connection().await;
    let account = client.account_id();
    let ids = ["e1".to_owned(), "e2".to_owned()];
    let old_header_properties = || {
        vec![
            Property::MessageId,
            Property::InReplyTo,
            Property::References,
            Property::Cc,
            Property::Header(Header::as_raw("Delivered-To", true)),
            Property::Header(Header::as_raw("X-Original-To", true)),
        ]
    };

    // The sync's fetch of whole emails.
    let mut properties = vec![
        Property::Id,
        Property::ThreadId,
        Property::MailboxIds,
        Property::Keywords,
        Property::From,
        Property::ReplyTo,
        Property::To,
        Property::Subject,
        Property::Preview,
        Property::HasAttachment,
        Property::Attachments,
        Property::BodyValues,
        Property::TextBody,
        Property::HtmlBody,
        Property::ReceivedAt,
    ];
    properties.extend(old_header_properties());
    let mut request = client.build();
    let get = request.get_email();
    get.ids(ids.iter()).properties(properties);
    get.arguments().fetch_all_body_values(true);
    let (was, was_using) = old(&request);
    let (is, is_using) = new(&wire(crate::emails_get(account, &ids)));
    assert_eq!(is, was, "the sync's Email/get");
    assert_eq!(is_using, was_using);
    assert_eq!(
        arguments(&is),
        &json!({
            "accountId": "a1",
            "ids": ["e1", "e2"],
            "properties": [
                "id", "threadId", "mailboxIds", "keywords", "from", "replyTo", "to", "subject",
                "preview", "hasAttachment", "attachments", "bodyValues", "textBody", "htmlBody",
                "receivedAt", "messageId", "inReplyTo", "references", "cc",
                "header:Delivered-To:all", "header:X-Original-To:all"
            ],
            "fetchAllBodyValues": true
        })
    );

    // The header backfill.
    let mut properties = vec![Property::Id];
    properties.extend(old_header_properties());
    let mut request = client.build();
    request.get_email().ids(ids.iter()).properties(properties);
    let (was, _) = old(&request);
    let (is, _) = new(&wire(crate::headers_get(account, &ids)));
    assert_eq!(is, was, "the header backfill");

    // The light fetch after `Email/changes`.
    let mut request = client.build();
    request.get_email().ids(ids.iter()).properties([
        Property::Id,
        Property::ThreadId,
        Property::MailboxIds,
        Property::Keywords,
        Property::HasAttachment,
    ]);
    let (was, _) = old(&request);
    let (is, _) = new(&wire(crate::changes::updates_get(account, &ids)));
    assert_eq!(is, was, "the light fetch");

    // The lookup of an interrupted send's copies.
    let mut request = client.build();
    request.get_email().ids(ids.iter()).properties([
        Property::Id,
        Property::MessageId,
        Property::Keywords,
    ]);
    let (was, _) = old(&request);
    let (is, _) = new(&wire(crate::copies_get(account, &ids)));
    assert_eq!(is, was, "the copies lookup");

    // The state read before a full sync: no ids.
    let mut request = client.build();
    request.get_email().ids(Vec::<String>::new());
    let (was, _) = old(&request);
    let (is, _) = new(&wire(crate::changes::email_state_get(account)));
    assert_eq!(is, was, "the state read");
    assert_eq!(arguments(&is), &json!({"accountId": "a1", "ids": []}));
}

#[tokio::test]
async fn email_changes_is_the_same_request() {
    let client = connection().await;
    let mut request = client.build();
    request.changes_email("em-41").max_changes(500);
    let (was, was_using) = old(&request);
    let (is, is_using) = new(&wire(crate::changes::email_changes(
        client.account_id(),
        "em-41",
    )));
    assert_eq!(is, was);
    assert_eq!(is_using, was_using);
    assert_eq!(
        arguments(&is),
        &json!({"accountId": "a1", "sinceState": "em-41", "maxChanges": 500})
    );
}

/// What the move changes in decoding, by design: answers `jmap-client`
/// refused whole, failing the sync, are read by `jmap-wire`, each odd
/// value costing only its own property. The emails are those of
/// `crates/jmap-wire/tests/fixtures/shapes/email-get.lenient.json`.
#[test]
fn answers_jmap_client_refused_are_read() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../jmap-wire/tests/fixtures/shapes/email-get.lenient.json");
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("the fixture")).expect("JSON");
    let emails = body["methodResponses"][0][1]["list"]
        .as_array()
        .expect("a list")
        .clone();
    let refused_before = emails
        .iter()
        .filter(|email| {
            serde_json::from_value::<jmap_client::email::Email>((*email).clone()).is_err()
        })
        .map(|email| email["id"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(
        refused_before,
        ["x1", "x2", "x3"],
        "jmap-client read only the email whose every property is null"
    );
    for email in emails {
        let read: jmap_wire::mail::EmailRecord =
            serde_json::from_value(email.clone()).expect("jmap-wire reads any object");
        assert_eq!(read.id.as_deref(), email["id"].as_str());
    }

    // One property that no RFC has was enough to refuse an email.
    let future = json!({"id": "e1", "futureProperty": true});
    assert!(serde_json::from_value::<jmap_client::email::Email>(future.clone()).is_err());
    let read: jmap_wire::mail::EmailRecord = serde_json::from_value(future).expect("read");
    assert_eq!(read.extra["futureProperty"], true);
}
