//! Decodes every recorded response under `tests/fixtures`, whichever
//! server it is from (see `tests/fixtures/README.md`).

use std::{fs, path::Path};

use jmap_wire::{
    ChangesResponse, GetResponse, MethodError, MethodErrorType, ProblemDetails, ProblemKind,
    QueryChangesResponse, QueryResponse, Response, Session, SetErrorType, SetResponse, capability,
    mail::{
        AsRaw, AsUrls, EmailRecord, EmailSubmissionRecord, IdentityRecord, MailboxRecord, Role,
        ThreadRecord,
    },
};
use serde::de::DeserializeOwned;
use serde_json::Value;

fn read<T: DeserializeOwned>(path: &Path) -> T {
    let text = fs::read_to_string(path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path:?}: {error}"))
}

/// The first method response's arguments, as `T`; its name must be
/// `method`.
fn first<T: DeserializeOwned>(path: &Path, method: &str) -> T {
    let response: Response = read(path);
    let call_id = response
        .method_responses
        .first()
        .unwrap_or_else(|| panic!("{path:?} has no method response"))
        .call_id
        .clone();
    response
        .arguments_as(&call_id, method)
        .unwrap_or_else(|error| panic!("{path:?}: {error}"))
}

fn fixture(server: &str, name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(server)
        .join(name)
}

/// Whether everything in `sent` that is not `null` is in `kept` too:
/// what a record written back must still hold of what the server sent
/// (it may add the defaults of what was left out).
fn holds(kept: &Value, sent: &Value) -> bool {
    match (kept, sent) {
        (_, Value::Null) => true,
        (Value::Object(kept), Value::Object(sent)) => sent.iter().all(|(name, sent)| {
            sent.is_null() || kept.get(name).is_some_and(|kept| holds(kept, sent))
        }),
        (Value::Array(kept), Value::Array(sent)) => {
            kept.len() == sent.len() && kept.iter().zip(sent).all(|(kept, sent)| holds(kept, sent))
        }
        (kept, sent) => kept == sent,
    }
}

/// Decodes `arguments` as `T` and checks that writing it back loses
/// nothing the server sent.
fn typed<T: DeserializeOwned + serde::Serialize>(path: &Path, name: &str, arguments: &Value) -> T {
    let decoded: T = serde_json::from_value(arguments.clone())
        .unwrap_or_else(|error| panic!("{path:?}: {name}: {error}"));
    let written = serde_json::to_value(&decoded).expect("encodes");
    assert!(
        holds(&written, arguments),
        "{path:?}: {name} lost something:\n sent {arguments}\n kept {written}"
    );
    decoded
}

/// Decodes one method response by its name; false when this test has no
/// type for it.
fn decode_method_response(path: &Path, name: &str, arguments: &Value) -> bool {
    match name {
        "Mailbox/get" => {
            typed::<GetResponse<MailboxRecord>>(path, name, arguments);
        }
        "Thread/get" => {
            typed::<GetResponse<ThreadRecord>>(path, name, arguments);
        }
        "Email/get" => {
            typed::<GetResponse<EmailRecord>>(path, name, arguments);
        }
        "Identity/get" => {
            typed::<GetResponse<IdentityRecord>>(path, name, arguments);
        }
        "Mailbox/changes" | "Thread/changes" | "Email/changes" => {
            typed::<ChangesResponse>(path, name, arguments);
        }
        "Mailbox/query" | "Email/query" => {
            typed::<QueryResponse>(path, name, arguments);
        }
        "Mailbox/queryChanges" | "Email/queryChanges" => {
            typed::<QueryChangesResponse>(path, name, arguments);
        }
        "Identity/set" => {
            serde_json::from_value::<SetResponse<IdentityRecord>>(arguments.clone())
                .unwrap_or_else(|error| panic!("{path:?}: {name}: {error}"));
        }
        "EmailSubmission/set" => {
            serde_json::from_value::<SetResponse<EmailSubmissionRecord>>(arguments.clone())
                .unwrap_or_else(|error| panic!("{path:?}: {name}: {error}"));
        }
        "Email/set" => {
            serde_json::from_value::<SetResponse<Value>>(arguments.clone())
                .unwrap_or_else(|error| panic!("{path:?}: {name}: {error}"));
        }
        "error" => {
            serde_json::from_value::<MethodError>(arguments.clone())
                .unwrap_or_else(|error| panic!("{path:?}: {name}: {error}"));
        }
        _ => return false,
    }
    true
}

#[test]
fn every_fixture_of_every_server_decodes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut decoded = 0;
    for server in fs::read_dir(&root).expect("the fixtures directory") {
        let server = server.expect("an entry").path();
        if !server.is_dir() {
            continue;
        }
        for file in fs::read_dir(&server).expect("a server directory") {
            let path = file.expect("an entry").path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("a file name");
            if !name.ends_with(".json") {
                continue;
            }
            let kind = name.split('.').next().unwrap_or_default();
            match kind {
                "session" => {
                    let session: Session = read(&path);
                    assert!(session.has_capability(capability::CORE), "{path:?}");
                    assert!(session.core().is_some(), "{path:?}");
                    // Nothing the server sent is dropped.
                    assert_eq!(
                        serde_json::to_value(&session).expect("encodes"),
                        read::<Value>(&path),
                        "{path:?}"
                    );
                }
                "problem" => {
                    read::<ProblemDetails>(&path);
                }
                // Any other file is a response body: every method
                // response in it is decoded by its name, and the first
                // must be what the file's name says.
                kind => {
                    let response: Response = read(&path);
                    let first = response
                        .method_responses
                        .first()
                        .unwrap_or_else(|| panic!("{path:?} has no method response"));
                    let expected = match kind {
                        "method-error" => "error",
                        "mailbox-get" => "Mailbox/get",
                        "mailbox-changes" => "Mailbox/changes",
                        "mailbox-query" => "Mailbox/query",
                        "thread-get" => "Thread/get",
                        "email-get" => "Email/get",
                        "email-query" => "Email/query",
                        "email-changes" => "Email/changes",
                        "email-querychanges" => "Email/queryChanges",
                        "email-set" => "Email/set",
                        "identity-get" => "Identity/get",
                        "identity-set" => "Identity/set",
                        "emailsubmission-set" => "EmailSubmission/set",
                        "chained" => first.name.as_str(),
                        other => panic!("{path:?}: no decoder for fixtures named {other}"),
                    };
                    assert_eq!(first.name, expected, "{path:?}");
                    for answer in &response.method_responses {
                        assert!(
                            decode_method_response(&path, &answer.name, &answer.arguments),
                            "{path:?}: no decoder for {}",
                            answer.name
                        );
                    }
                }
            }
            decoded += 1;
        }
    }
    assert!(decoded >= 40, "only {decoded} fixtures were found");
}

#[test]
fn the_rfc_examples_say_what_the_rfcs_say() {
    let session: Session = read(&fixture("rfc", "session.json"));
    assert_eq!(session.primary_account(capability::MAIL), Some("A13824"));
    assert_eq!(
        session.capabilities["https://example.com/apis/foobar"]["maxFoosFinangled"],
        42
    );

    let unknown: ProblemDetails = read(&fixture("rfc", "problem.unknown-capability.json"));
    assert_eq!(unknown.kind(), ProblemKind::UnknownCapability);
    let limit: ProblemDetails = read(&fixture("rfc", "problem.limit.json"));
    assert_eq!(limit.limit.as_deref(), Some("maxSizeRequest"));

    let identities: GetResponse<IdentityRecord> =
        first(&fixture("rfc", "identity-get.json"), "Identity/get");
    assert_eq!(identities.list.len(), 2);
    assert_eq!(identities.list[1].may_delete, Some(true));

    let refused: SetResponse<EmailSubmissionRecord> = first(
        &fixture("rfc", "emailsubmission-set.forbidden-to-send.json"),
        "EmailSubmission/set",
    );
    assert_eq!(
        refused.not_created["k1490"].error_type,
        SetErrorType::ForbiddenToSend
    );

    let response: Response = read(&fixture("rfc", "method-error.json"));
    let error: MethodError =
        serde_json::from_value(response.method_responses[0].arguments.clone()).expect("decodes");
    assert_eq!(error.error_type, MethodErrorType::UnknownMethod);
}

#[test]
fn james_shaped_answers_are_read_despite_their_nulls() {
    let session: Session = read(&fixture("james-3.9", "session.json"));
    assert!(session.has_capability("urn:apache:james:params:jmap:mail:quota"));
    assert!(!session.has_capability("urn:ietf:params:jmap:contacts"));
    assert_eq!(
        session.primary_account(capability::SUBMISSION),
        session.primary_account(capability::MAIL)
    );

    let identities: GetResponse<IdentityRecord> =
        first(&fixture("james-3.9", "identity-get.json"), "Identity/get");
    assert_eq!(identities.state, None);
    assert!(identities.not_found.is_empty());
    assert_eq!(identities.list[0].email.as_deref(), Some("jmap@localhost"));
    assert_eq!(identities.list[0].extra["sortOrder"], 100);

    let refused: SetResponse<EmailSubmissionRecord> = first(
        &fixture("james-3.9", "emailsubmission-set.invalid-arguments.json"),
        "EmailSubmission/set",
    );
    // A method error type used as a SetError type: kept as it came.
    assert_eq!(
        refused.not_created["k0"].error_type,
        SetErrorType::Other("invalidArguments".to_owned())
    );

    let destroyed: SetResponse<Value> = first(
        &fixture("james-3.9", "email-set.destroyed.json"),
        "Email/set",
    );
    assert_eq!(destroyed.destroyed.len(), 1);
    assert_eq!(
        destroyed.not_destroyed["not-an-id"].error_type,
        SetErrorType::NotFound
    );
}

/// The examples of RFC 8621 sections 2.6, 3.1.1, 4.2.1 and 4.10.
#[test]
fn the_rfc_examples_of_reading_mail_say_what_the_rfc_says() {
    let mailboxes: GetResponse<MailboxRecord> =
        first(&fixture("rfc", "mailbox-get.json"), "Mailbox/get");
    assert_eq!(mailboxes.list[0].role, Some(Role::Inbox));
    assert_eq!(mailboxes.list[0].unread_threads, Some(5128));
    assert_eq!(mailboxes.list[1].name.as_deref(), Some("Important mail"));

    let changes: ChangesResponse =
        first(&fixture("rfc", "mailbox-changes.json"), "Mailbox/changes");
    assert_eq!(changes.updated, ["MB23cfa8094c0f41e6"]);
    assert_eq!(
        changes.updated_properties().map(|names| names.len()),
        Some(4)
    );

    let threads: GetResponse<ThreadRecord> =
        first(&fixture("rfc", "thread-get.json"), "Thread/get");
    assert_eq!(threads.list[1].email_ids, Some(vec!["82cf7bb".to_owned()]));

    // Section 4.2.1: two of eight properties are of the body, cut off at
    // 256 octets, and one is a header field as URLs.
    let emails: GetResponse<EmailRecord> = first(&fixture("rfc", "email-get.json"), "Email/get");
    assert_eq!(emails.not_found, ["f123u456"]);
    let email = &emails.list[0];
    assert_eq!(email.thread_id.as_deref(), Some("ef1314a"));
    assert_eq!(email.mailbox_list(), ["f123"]);
    assert_eq!(
        email.from.as_ref().expect("from")[0].email,
        "joe@example.com"
    );
    assert_eq!(email.subject.as_deref(), Some("Dinner on Thursday?"));
    assert_eq!(
        email.received_at.as_ref().map(|date| date.as_str()),
        Some("2013-10-13T14:12:00Z")
    );
    assert_eq!(
        email.header::<AsUrls>("List-POST"),
        Some(vec!["mailto:partytime@lists.example.com".to_owned()])
    );
    let html = email.html_body.as_ref().expect("htmlBody");
    assert_eq!(html[0].content_type.as_deref(), Some("text/html"));
    assert_eq!((html[0].size, html[1].size), (Some(283_331), Some(10_343)));
    let value = email
        .body_value(html[0].part_id.as_deref().expect("a part id"))
        .expect("fetched");
    assert!(value.is_truncated() && !value.is_encoding_problem());
    assert_eq!(value.value.as_deref(), Some("<html><body><p>Hello ..."));
    assert!(!email.body_value("2").expect("fetched").is_truncated());
    // What was not asked for is not there, and that is no error.
    assert!(email.keywords.is_none() && email.text_body.is_none() && email.preview.is_none());

    // Section 4.10: the listing, and what changed later.
    let response: Response = read(&fixture("rfc", "chained.query-get-thread-get.json"));
    let query: QueryResponse = response.arguments_as("0", "Email/query").expect("decodes");
    assert_eq!((query.total, query.ids.len()), (Some(115), 2));
    let listing: GetResponse<EmailRecord> =
        response.arguments_as("3", "Email/get").expect("decodes");
    let email = &listing.list[0];
    assert!(email.has_keyword("$seen") && email.has_keyword("$flagged"));
    assert_eq!(
        (email.size, email.has_attachment),
        (Some(175_047), Some(true))
    );

    let response: Response = read(&fixture("rfc", "chained.changes-querychanges.json"));
    let changes: QueryChangesResponse = response
        .arguments_as("11", "Email/queryChanges")
        .expect("decodes");
    assert_eq!(changes.added[0].index, 0);
    assert_eq!(changes.removed, ["M9bd17497e2a99cb345fc1d0a"]);
}

/// The hand-written shapes (`tests/fixtures/shapes`): what the RFC
/// allows a server to leave out, null or add.
#[test]
fn the_awkward_shapes_are_read_property_by_property() {
    let mailboxes: GetResponse<MailboxRecord> =
        first(&fixture("shapes", "mailbox-get.all.json"), "Mailbox/get");
    let by_id = |id: &str| {
        mailboxes
            .list
            .iter()
            .find(|mailbox| mailbox.id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("no mailbox {id}"))
    };
    assert_eq!(
        by_id("mb-archive").sort_order,
        None,
        "left out: the default"
    );
    assert!(
        !by_id("mb-archive")
            .my_rights
            .as_ref()
            .expect("rights")
            .may_delete
    );
    assert_eq!(by_id("mb-zmail").parent_id.as_deref(), Some("mb-projects"));
    assert_eq!(by_id("mb-bulk").role, Some(Role::Other("JUNK".to_owned())));
    assert_eq!(by_id("mb-nameless").name, None);

    let emails: GetResponse<EmailRecord> =
        first(&fixture("shapes", "email-get.sync.json"), "Email/get");
    let by_id = |id: &str| {
        emails
            .list
            .iter()
            .find(|email| email.id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("no email {id}"))
    };
    assert_eq!(
        by_id("e1")
            .header_all::<AsRaw>("Delivered-To")
            .map(|all| all.len()),
        Some(3)
    );
    assert_eq!(by_id("e2").from, None, "null");
    assert_eq!(by_id("e3").keyword_list(), ["$answered", "$flagged"]);
    assert_eq!(by_id("e3").attachments.as_ref().map(Vec::len), Some(8));
    assert_eq!(by_id("e5").thread_id, None, "left out");
    assert_eq!(by_id("e6").mailbox_list(), ["mb-inbox", "mb-unknown"]);
    assert!(by_id("e7").body_value("2").expect("fetched").is_truncated());
    assert_eq!(
        by_id("e9").header_all::<AsRaw>("Delivered-To"),
        Some(Vec::new())
    );

    // What `jmap-client` refused whole: values of other types than the
    // RFC's, and properties of later specifications.
    let lenient: GetResponse<EmailRecord> =
        first(&fixture("shapes", "email-get.lenient.json"), "Email/get");
    assert_eq!(lenient.list.len(), 4);
    let odd = &lenient.list[0];
    assert_eq!(odd.subject.as_deref(), Some("Odd types"));
    assert_eq!((odd.size, odd.received_at.as_ref()), (None, None));
    assert_eq!(odd.extra["size"], "12 kB");
    assert_eq!(odd.extra["receivedAt"], 1_793_722_505);
    assert_eq!(odd.from.as_ref().expect("from")[0].email, "");
    assert_eq!(odd.extra["futureProperty"]["nested"], true);
    assert!(
        lenient.list[1]
            .body_value("1")
            .expect("a value")
            .value
            .is_none()
    );
    assert_eq!(lenient.state, None);
}

/// What Apache James 3.9 answered the harness's probes (captures; see
/// `tests/fixtures/james-3.9/README.md`).
#[test]
fn what_james_answered_is_read_in_full() {
    use jmap_wire::mail::{AsAddresses, AsDate, AsGroupedAddresses, AsMessageIds, AsText};

    let james = |name: &str| fixture("james-3.9", name);
    let session: Session = read(&james("session.json"));
    let core = session.core().expect("core limits");
    assert_eq!(core.max_objects_in_get, Some(500));
    assert_eq!(core.max_calls_in_request, Some(16));
    assert_eq!(core.max_size_request, Some(10_000_000));

    let mailboxes: GetResponse<MailboxRecord> =
        first(&james("mailbox-get.all.json"), "Mailbox/get");
    let inbox = &mailboxes.list[0];
    assert_eq!(inbox.role, Some(Role::Inbox));
    assert_eq!(inbox.parent_id, None, "left out, not null");
    assert!(inbox.my_rights.as_ref().expect("rights").may_read_items);
    let some: GetResponse<MailboxRecord> =
        first(&james("mailbox-get.some-properties.json"), "Mailbox/get");
    assert_eq!(some.list[0].name.as_deref(), Some("INBOX"));
    assert_eq!(some.list[0].total_emails, None, "not asked for");

    // The sync's answer: a multipart message with an inline image and
    // an attachment, an HTML-only one and a bare one.
    let emails: GetResponse<EmailRecord> = first(&james("email-get.sync.json"), "Email/get");
    assert_eq!(emails.list.len(), 3);
    let mixed = &emails.list[0];
    assert_eq!(
        mixed.subject.as_deref(),
        Some("jmap-wire probe caf\u{e9} mixed")
    );
    assert_eq!(
        mixed.keywords,
        Some(Default::default()),
        "no keywords: {{}}"
    );
    assert_eq!(mixed.to.as_ref().expect("to")[0].name, None, "left out");
    assert_eq!(
        mixed.to.as_ref().expect("to")[2].name.as_deref(),
        Some("Bo Example")
    );
    let attachments = mixed.attachments.as_ref().expect("attachments");
    assert_eq!(
        attachments[0].cid.as_deref(),
        Some("probe-logo@phase2b.test")
    );
    assert_eq!(attachments[0].name, None);
    assert_eq!(attachments[1].name.as_deref(), Some("report.csv"));
    assert_eq!(attachments[1].language.as_ref().map(Vec::len), Some(2));
    let text = &mixed.text_body.as_ref().expect("textBody")[0];
    let html = &mixed.html_body.as_ref().expect("htmlBody")[0];
    assert_eq!(text.content_type.as_deref(), Some("text/plain"));
    assert_eq!(html.content_type.as_deref(), Some("text/html"));
    assert_eq!(
        mixed
            .body_value(text.part_id.as_deref().expect("a part id"))
            .and_then(|value| value.value.as_deref()),
        Some("Plain alternative of the probe.")
    );
    assert_eq!(
        mixed.header_all::<AsRaw>("Delivered-To").expect("fetched"),
        [" jmap@localhost"]
    );
    assert_eq!(mixed.header_all::<AsRaw>("X-Original-To"), Some(Vec::new()));
    // RFC 8621 section 4.1.4: with no text/plain, the HTML part is the
    // text body too.
    let html_only = &emails.list[1];
    assert_eq!(
        html_only.text_body.as_ref().expect("textBody")[0]
            .content_type
            .as_deref(),
        Some("text/html")
    );
    let bare = &emails.list[2];
    assert_eq!((bare.subject.as_ref(), bare.cc.as_ref()), (None, None));

    // The whole structure, with body values cut off at 16 octets.
    let structure: GetResponse<EmailRecord> =
        first(&james("email-get.structure.json"), "Email/get");
    let email = &structure.list[0];
    assert_eq!(
        email.sender.as_ref().expect("sender")[0].email,
        "robot@phase2b.test"
    );
    assert_eq!(
        email.sent_at.as_ref().map(|date| date.as_str()),
        Some("2026-11-03T15:15:05Z")
    );
    assert!(email.size.is_some() && email.blob_id.is_some());
    assert!(
        email
            .headers
            .as_ref()
            .is_some_and(|headers| headers.len() > 10)
    );
    let root = email.body_structure.as_deref().expect("bodyStructure");
    assert!(root.is_multipart() && root.blob_id.is_none());
    assert_eq!(
        root.header::<AsRaw>("Content-Type").as_deref(),
        Some(" multipart/mixed; boundary=\"mixed-b\"")
    );
    let parts = root.sub_parts.as_ref().expect("subParts");
    assert_eq!(parts.len(), 2);
    let related = &parts[0].sub_parts.as_ref().expect("alternatives")[1];
    assert_eq!(related.content_type.as_deref(), Some("multipart/related"));
    assert_eq!(
        related.sub_parts.as_ref().expect("related parts")[1]
            .cid
            .as_deref(),
        Some("probe-logo@phase2b.test")
    );
    let cut = email.body_value("3").expect("fetched");
    assert!(cut.is_truncated());
    assert_eq!(cut.value.as_deref(), Some("Plain alternativ"));

    // Header fields in every form, of a message that has them and one
    // that does not.
    let forms: GetResponse<EmailRecord> = first(&james("email-get.header-forms.json"), "Email/get");
    let (with, without) = (&forms.list[0], &forms.list[1]);
    assert_eq!(
        with.header::<AsRaw>("Subject").as_deref(),
        Some(" =?UTF-8?Q?jmap-wire_probe_caf=C3=A9?= mixed")
    );
    assert_eq!(
        with.header::<AsText>("Subject").as_deref(),
        Some("jmap-wire probe caf\u{e9} mixed")
    );
    assert_eq!(
        with.header_all::<AsText>("subject").map(|all| all.len()),
        Some(1)
    );
    assert_eq!(
        with.header_all::<AsText>("Comments").expect("fetched"),
        ["first comment", "second comment"]
    );
    assert_eq!(
        with.header::<AsRaw>("X-Probe").as_deref(),
        Some(" two"),
        "the last"
    );
    assert_eq!(
        with.header_all::<AsRaw>("X-Probe").expect("fetched"),
        [" one", " two"]
    );
    assert_eq!(
        with.header_all::<AsText>("X-Probe").expect("fetched"),
        ["one", "two"]
    );
    assert_eq!(with.header::<AsAddresses>("To").map(|to| to.len()), Some(3));
    let groups = with.header::<AsGroupedAddresses>("To").expect("fetched");
    assert_eq!(groups[0].name, None);
    assert_eq!(groups[1].name.as_deref(), Some("Friends"));
    assert_eq!(groups[1].addresses.len(), 2);
    assert_eq!(
        with.header_all::<AsAddresses>("Cc").map(|all| all.len()),
        Some(1)
    );
    assert_eq!(
        with.header::<AsMessageIds>("References")
            .map(|ids| ids.len()),
        Some(2)
    );
    assert_eq!(
        with.header::<AsDate>("Date").expect("fetched").as_str(),
        "2026-11-03T15:15:05Z"
    );
    assert_eq!(
        with.header::<AsUrls>("List-Unsubscribe")
            .map(|urls| urls.len()),
        Some(2)
    );
    assert_eq!(with.header::<AsRaw>("X-Absent"), None);
    assert_eq!(with.header_all::<AsRaw>("X-Absent"), Some(Vec::new()));
    assert_eq!(with.header::<AsAddresses>("X-Absent"), None);
    assert_eq!(without.header::<AsText>("Subject"), None);
    assert_eq!(without.header::<AsDate>("Date"), None);
    assert_eq!(without.header_all::<AsText>("Comments"), Some(Vec::new()));
    assert_eq!(without.header::<AsUrls>("List-Post"), None);

    let not_found: GetResponse<EmailRecord> =
        first(&james("email-get.not-found.json"), "Email/get");
    assert_eq!(not_found.not_found, ["not-an-email-id"]);
    assert_eq!(not_found.list.len(), 1);

    let page: QueryResponse = first(&james("email-query.page.json"), "Email/query");
    assert_eq!((page.ids.len(), page.position, page.total), (2, 0, None));
    assert!(!page.can_calculate_changes && page.query_state.is_some());
    let filtered: QueryResponse = first(&james("email-query.filtered.json"), "Email/query");
    assert_eq!(filtered.limit, Some(256), "the limit James applied");

    let changes: ChangesResponse = first(&james("email-changes.none.json"), "Email/changes");
    assert!(changes.is_empty() && !changes.has_more_changes);
    assert_eq!(changes.new_state, changes.old_state);

    let threads: GetResponse<ThreadRecord> = first(&james("thread-get.json"), "Thread/get");
    assert_eq!(threads.list.len(), 3);

    // A state James never gave is `invalidArguments` there, where the
    // RFC has `cannotCalculateChanges`: a caller falls back on either.
    let response: Response = read(&james("method-error.email-changes-cannot-calculate.json"));
    let error: MethodError =
        serde_json::from_value(response.method_responses[0].arguments.clone()).expect("decodes");
    assert_eq!(error.error_type, MethodErrorType::InvalidArguments);
    let response: Response = read(&james("method-error.email-querychanges.json"));
    let error: MethodError =
        serde_json::from_value(response.method_responses[0].arguments.clone()).expect("decodes");
    assert_eq!(error.error_type, MethodErrorType::UnknownMethod);
}
