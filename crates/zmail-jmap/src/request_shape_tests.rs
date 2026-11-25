//! Every JMAP request Zmail builds, as the JSON it sends, pinned in
//! `request_shapes/requests.txt`.
//!
//! While Zmail moved from `jmap-client` to `jmap-wire` (ADR 0008, #132)
//! each of these requests was built both ways and compared
//! (`wire_parity_tests.rs`, gone with `jmap-client`). The text was
//! written while that comparison still held, so it is what `jmap-client`
//! sent but for the differences the comparison stated:
//!
//! - `using` lists Core and what the request's own calls need:
//!   identities no longer list mail;
//! - method call ids are `c0` where `jmap-client` wrote `s0`, and
//!   creation ids `k0` where it wrote `c0` or `i0`;
//! - clearing a keyword patches it to `null` where `jmap-client` wrote
//!   `false` (RFC 8620 section 5.3);
//! - a move patches the two mailboxes it names where the whole
//!   `mailboxIds` set was replaced. Deleting to the trash or the junk
//!   mailbox, and a move whose source the server does not know, still
//!   replace the set.
//!
//! `ZMAIL_UPDATE_SNAPSHOTS=1` rewrites the file; review the diff, as any
//! change is a change on the wire. Object members are written in
//! alphabetical order, which JSON gives no meaning to.

use std::{fmt::Write as _, path::Path};

use jmap_wire::mail::EmailUpdate;
use zmail_core::{OutgoingMessage, identity_sync::IdentityValues};

use crate::{identities, write_characterisation_tests::canonical};

const ACCOUNT: &str = "a1";

/// A request as it is pinned: one line of canonical JSON.
fn shown(request: &jmap_wire::Request) -> String {
    canonical(&serde_json::to_value(request).expect("encodes"))
}

/// A request with `method` as its only call.
fn wire(method: impl jmap_wire::Method) -> jmap_wire::Request {
    let mut request = jmap_wire::Request::new();
    request.call(method).expect("encodes");
    request
}

fn mailbox(id: &str, name: &str, role: &str) -> crate::JmapMailbox {
    crate::JmapMailbox {
        id: id.to_owned(),
        name: name.to_owned(),
        own_name: name.to_owned(),
        role: Some(role.to_owned()),
        sort_order: 0,
        parent: None,
    }
}

#[test]
fn every_request_is_the_json_it_was() {
    let mut text = String::new();
    let mut pin = |title: &str, request: &jmap_wire::Request| {
        writeln!(text, "# {title}\n{}", shown(request)).expect("writes");
    };

    // Identities.
    pin(
        "Identity/get",
        &identities::get_request(ACCOUNT).expect("builds"),
    );
    for reply_to in ["Team <team@example.com>, ops@example.com", ""] {
        let values = IdentityValues {
            name: "Rob".to_owned(),
            reply_to: reply_to.to_owned(),
            signature: "-- Rob".to_owned(),
        };
        pin(
            &format!("Identity/set update, reply-to {reply_to:?}"),
            &identities::update_request(ACCOUNT, "i1", &values).expect("builds"),
        );
        pin(
            &format!("Identity/set create, reply-to {reply_to:?}"),
            &identities::create_request(ACCOUNT, "help@example.com", &values).expect("builds"),
        );
    }
    pin(
        "Identity/set destroy",
        &identities::destroy_request(ACCOUNT, "i1").expect("builds"),
    );

    // Sending.
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
    let (import, creation_id) = crate::import_request(ACCOUNT, "b1", &["m-sent".to_owned()]);
    assert_eq!(creation_id, "k0");
    pin("Email/import of the Sent copy", &wire(import));
    let (submission, creation_id) =
        crate::submission_request(ACCOUNT, "e1", "i1", &message).expect("builds");
    assert_eq!(creation_id, "k0");
    pin("EmailSubmission/set", &submission);
    pin(
        "Email/set destroy",
        &crate::destroy_emails_request(ACCOUNT, &["e1", "e2"]).expect("builds"),
    );

    // The reads.
    let ids = ["e1".to_owned(), "e2".to_owned()];
    pin("Mailbox/get", &wire(crate::mailboxes_get(ACCOUNT)));
    for position in [0, 250] {
        pin(
            &format!("Email/query, the sync's page at {position}"),
            &wire(crate::sync_query(ACCOUNT, position)),
        );
    }
    for since in [Some(1_792_060_920), None] {
        pin(
            &format!("Email/query, an interrupted send's copies since {since:?}"),
            &wire(crate::copies_query(ACCOUNT, since, 250)),
        );
    }
    pin(
        "Email/get, the sync",
        &wire(crate::emails_get(ACCOUNT, &ids)),
    );
    pin(
        "Email/get, the header backfill",
        &wire(crate::headers_get(ACCOUNT, &ids)),
    );
    pin(
        "Email/get, the light fetch after a change",
        &wire(crate::changes::updates_get(ACCOUNT, &ids)),
    );
    pin(
        "Email/get, an interrupted send's copies",
        &wire(crate::copies_get(ACCOUNT, &ids)),
    );
    pin(
        "Email/get, the state before a full sync",
        &wire(crate::changes::email_state_get(ACCOUNT)),
    );
    pin(
        "Email/changes",
        &wire(crate::changes::email_changes(ACCOUNT, "em-41")),
    );

    // Server search.
    let mailboxes = [
        mailbox("mb-inbox", "Inbox", "inbox"),
        mailbox("mb-junk", "Bulk", "junk"),
    ];
    let now = zmail_core::time::clock_at("2026-06-15T12:00:00Z").expect("a time");
    for search in [
        "budget",
        "budget from:alice to:bob subject:plan",
        "-is:unread is:read is:flagged has:attachment",
        "in:inbox -in:spam -in:nowhere",
        "after:2026-06-01 before:2026-06-10 -after:2025-01-01",
        "\"exact phrase\" -budget",
        "",
    ] {
        let query = zmail_core::search::SearchQuery::parse(search);
        let filter = crate::search_filter(&query, &mailboxes, &now).expect("a filter");
        pin(
            &format!("Email/query, the search {search:?}"),
            &wire(crate::search_query(ACCOUNT, filter, 40)),
        );
    }
    let nowhere = zmail_core::search::SearchQuery::parse("in:nowhere");
    assert!(
        crate::search_filter(&nowhere, &mailboxes, &now).is_none(),
        "a search in a mailbox the server does not have is not sent"
    );

    // The write-back queue.
    for keyword in ["$seen", "$flagged"] {
        pin(
            &format!("Email/set, setting {keyword}"),
            &wire(crate::email_update(
                ACCOUNT,
                "e1",
                EmailUpdate::new().set_keyword(keyword),
            )),
        );
        pin(
            &format!("Email/set, clearing {keyword}"),
            &wire(crate::email_update(
                ACCOUNT,
                "e1",
                EmailUpdate::new().clear_keyword(keyword),
            )),
        );
    }
    let inbox = mailbox("m-inbox", "Inbox", "inbox");
    for (title, source, target) in [
        (
            "a move to the archive",
            Some(&inbox),
            ("m-archive", "Archive", "archive"),
        ),
        (
            "a delete to the trash",
            Some(&inbox),
            ("m-trash", "Trash", "trash"),
        ),
        ("a move to junk", Some(&inbox), ("m-junk", "Junk", "junk")),
        (
            "a move from a mailbox the server does not have",
            None,
            ("m-archive", "Archive", "archive"),
        ),
    ] {
        let target = mailbox(target.0, target.1, target.2);
        pin(
            &format!("Email/set, {title}"),
            &wire(crate::email_update(
                ACCOUNT,
                "e1",
                crate::move_update(source, &target),
            )),
        );
    }

    assert_pinned(&text);
}

/// Compares `actual` with the pinned text, or writes it when
/// `ZMAIL_UPDATE_SNAPSHOTS` is set.
fn assert_pinned(actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/request_shapes/requests.txt");
    if std::env::var_os("ZMAIL_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
        std::fs::write(&path, actual).expect("the pinned text is written");
        return;
    }
    // A checkout that turned the line ends into CRLF pins the same text.
    let pinned = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| {
            panic!("{path:?}: {error}; run with ZMAIL_UPDATE_SNAPSHOTS=1 to write it")
        })
        .replace("\r\n", "\n");
    assert!(
        pinned == actual,
        "the requests differ from what is pinned.\n--- pinned\n{pinned}\n--- now\n{actual}"
    );
}

/// What the move to `jmap-wire` changed in decoding, by design: answers
/// `jmap-client` refused whole, failing the sync, are read, each odd
/// value costing only its own property. The emails are those of
/// `crates/jmap-wire/tests/fixtures/shapes/email-get.lenient.json`, of
/// which `jmap-client` read only the one whose every property is null.
#[test]
fn answers_with_odd_values_are_read() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../jmap-wire/tests/fixtures/shapes/email-get.lenient.json");
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("the fixture")).expect("JSON");
    let emails = body["methodResponses"][0][1]["list"]
        .as_array()
        .expect("a list")
        .clone();
    assert_eq!(emails.len(), 4);
    for email in emails {
        let read: jmap_wire::mail::EmailRecord =
            serde_json::from_value(email.clone()).expect("jmap-wire reads any object");
        assert_eq!(read.id.as_deref(), email["id"].as_str());
    }
    // One property that no RFC has was enough to refuse an email.
    let future = serde_json::json!({"id": "e1", "futureProperty": true});
    let read: jmap_wire::mail::EmailRecord = serde_json::from_value(future).expect("read");
    assert_eq!(read.extra["futureProperty"], true);
}
