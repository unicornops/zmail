//! Submitting and finding an interrupted send's copy against a JMAP server
//! on loopback: the session, `Identity/get`, `Mailbox/get`, uploads,
//! `Email/import`, `Email/query`, `Email/get`, `Email/set` destroy and
//! `EmailSubmission/set` with `onSuccessUpdateEmail`, applied as RFC 8621
//! says. The live harness checks the same against James.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};
use zmail_core::{AuthKind, OutgoingMessage};

use crate::{
    SubmissionRefused, SubmissionTrace, SubmitError, find_submitted_message,
    resubmit_outgoing_message, submission_created, submit_imported_message,
    submit_outgoing_message,
};

/// The server's emails (`id`, `messageId`, `keywords`, `receivedAt` as
/// Unix seconds) and the method calls it answered.
#[derive(Default)]
struct State {
    emails: Vec<Value>,
    /// Uploaded blobs, by position (`b0`, `b1`...).
    blobs: Vec<Vec<u8>>,
    /// How many emails were imported: the last one is `new{imports}`.
    imports: usize,
    /// Every method call, as `[name, arguments]`.
    calls: Vec<Value>,
    submissions: Vec<Value>,
    /// The SetError submissions are refused with, as a server does for a
    /// bad envelope.
    refuse: Option<Value>,
    /// Answer `EmailSubmission/set` with this method error instead.
    method_error: Option<Value>,
    /// Accept submissions and then answer something that is not JMAP, as
    /// when the answer is lost.
    lose_answer: bool,
    /// Refuse to destroy emails.
    refuse_destroy: bool,
}

struct FakeServer {
    endpoint: String,
    state: Arc<Mutex<State>>,
}

impl FakeServer {
    fn start(emails: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        let state = Arc::new(Mutex::new(State {
            emails,
            ..State::default()
        }));
        let (shared, url) = (state.clone(), base.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                serve(stream, &url, &shared);
            }
        });
        Self {
            endpoint: base,
            state,
        }
    }

    async fn find(&self, message_id: &str, claimed_at: Option<i64>) -> SubmissionTrace {
        find_submitted_message(
            &self.endpoint,
            "rob@example.com",
            "pw",
            AuthKind::Password,
            message_id,
            claimed_at,
        )
        .await
        .expect("the lookup runs")
    }

    async fn send(&self, message: &OutgoingMessage) -> anyhow::Result<()> {
        submit_outgoing_message(
            &self.endpoint,
            "rob@example.com",
            "pw",
            AuthKind::Password,
            "rob@example.com",
            message,
        )
        .await
    }

    async fn resend(&self, message: &OutgoingMessage) -> anyhow::Result<()> {
        resubmit_outgoing_message(
            &self.endpoint,
            "rob@example.com",
            "pw",
            AuthKind::Password,
            "rob@example.com",
            message,
            Some(CLAIMED),
        )
        .await
    }

    fn with_state<T>(&self, read: impl FnOnce(&mut State) -> T) -> T {
        read(&mut self.state.lock().expect("the state"))
    }

    /// The arguments of every call of `method` so far.
    fn calls(&self, method: &str) -> Vec<Value> {
        self.with_state(|state| {
            state
                .calls
                .iter()
                .filter(|call| call[0] == method)
                .map(|call| call[1].clone())
                .collect()
        })
    }

    /// The server's emails with `message_id`, and their keywords.
    fn copies(&self, message_id: &str) -> Vec<Value> {
        self.with_state(|state| {
            state
                .emails
                .iter()
                .filter(|email| email["messageId"][0] == message_id)
                .map(|email| email["keywords"].clone())
                .collect()
        })
    }
}

fn serve(mut stream: TcpStream, base: &str, state: &Mutex<State>) {
    let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    let answer = if request_line.starts_with("GET") {
        json!({
            "capabilities": {
                "urn:ietf:params:jmap:core": {
                    "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
                    "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
                    "maxCallsInRequest": 16, "maxObjectsInGet": 2,
                    "maxObjectsInSet": 500, "collationAlgorithms": []
                },
                "urn:ietf:params:jmap:mail": {},
                "urn:ietf:params:jmap:submission": {}
            },
            "accounts": {"a1": {"name": "rob@example.com", "isPersonal": true,
                                 "isReadOnly": false, "accountCapabilities": {}}},
            "primaryAccounts": {"urn:ietf:params:jmap:mail": "a1",
                                 "urn:ietf:params:jmap:submission": "a1"},
            "username": "rob@example.com",
            "apiUrl": format!("{base}/api"),
            "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
            "uploadUrl": format!("{base}/upload/{{accountId}}/"),
            "eventSourceUrl": format!("{base}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
            "state": "s1"
        })
    } else if request_line.contains("/upload/") {
        let mut state = state.lock().expect("the state");
        state.blobs.push(body.clone());
        json!({"accountId": "a1", "blobId": format!("b{}", state.blobs.len() - 1),
               "type": "message/rfc822", "size": body.len()})
    } else {
        let request: Value = serde_json::from_slice(&body).expect("a JSON request");
        let call = &request["methodCalls"][0];
        let mut state = state.lock().expect("the state");
        state.calls.push(json!([call[0], call[1]]));
        let responses = match call[0].as_str() {
            Some("Identity/get") => vec![json!(["Identity/get", {"accountId": "a1",
                "state": "s1", "list": [{"id": "i1", "email": "rob@example.com"}]}, call[2]])],
            Some("Mailbox/get") => vec![json!(["Mailbox/get", {"accountId": "a1",
                "state": "s1", "list": [
                    {"id": "m-inbox", "name": "Inbox", "role": "inbox"},
                    {"id": "m-sent", "name": "Sent", "role": "sent"}
                ], "notFound": []}, call[2]])],
            Some("Email/import") => {
                let mut created = serde_json::Map::new();
                let imports = call[1]["emails"].as_object().cloned().unwrap_or_default();
                for (create_id, import) in imports {
                    let blob = import["blobId"]
                        .as_str()
                        .and_then(|blob| blob.strip_prefix('b'))
                        .and_then(|index| index.parse::<usize>().ok())
                        .and_then(|index| state.blobs.get(index).cloned())
                        .expect("an uploaded blob");
                    let message_id = String::from_utf8_lossy(&blob)
                        .lines()
                        .find_map(|line| line.strip_prefix("Message-ID: "))
                        .map(|id| id.trim().trim_start_matches('<').trim_end_matches('>'))
                        .expect("the copy has a Message-ID")
                        .to_owned();
                    state.imports += 1;
                    let id = format!("new{}", state.imports);
                    state
                        .emails
                        .push(json!({"id": id, "messageId": [message_id],
                        "keywords": import["keywords"], "receivedAt": CLAIMED + 60}));
                    created.insert(
                        create_id,
                        json!({"id": id, "blobId": import["blobId"], "threadId": "t1",
                               "size": blob.len()}),
                    );
                }
                vec![json!(["Email/import", {"accountId": "a1", "newState": "s2",
                    "created": created}, call[2]])]
            }
            Some("Email/query") => {
                let after = call[1]["filter"]["after"].as_str().map(|after| {
                    chrono::DateTime::parse_from_rfc3339(after)
                        .expect("a UTCDate")
                        .timestamp()
                });
                let position = call[1]["position"].as_u64().unwrap_or(0) as usize;
                let ids = state
                    .emails
                    .iter()
                    .filter(|email| {
                        after.is_none_or(|after| email["receivedAt"].as_i64() >= Some(after))
                    })
                    .map(|email| email["id"].clone())
                    .skip(position)
                    .take(1)
                    .collect::<Vec<_>>();
                vec![
                    json!(["Email/query", {"accountId": "a1", "queryState": "q1",
                    "canCalculateChanges": false, "position": position, "ids": ids}, call[2]]),
                ]
            }
            Some("Email/get") => {
                let ids = call[1]["ids"].as_array().cloned().unwrap_or_default();
                assert!(ids.len() <= 2, "maxObjectsInGet is kept: {ids:?}");
                let list = state
                    .emails
                    .iter()
                    .filter(|email| ids.contains(&email["id"]))
                    .map(|email| {
                        json!({"id": email["id"], "messageId": email["messageId"],
                               "keywords": email["keywords"]})
                    })
                    .collect::<Vec<_>>();
                vec![json!(["Email/get", {"accountId": "a1", "state": "s1",
                    "list": list, "notFound": []}, call[2]])]
            }
            Some("Email/set") => {
                let mut answer = json!({"accountId": "a1", "newState": "s2"});
                let mut destroyed = Vec::new();
                let ids = call[1]["destroy"].as_array().cloned().unwrap_or_default();
                for id in ids {
                    let key = id.as_str().unwrap_or_default().to_owned();
                    let at = state.emails.iter().position(|email| email["id"] == id);
                    match at {
                        _ if state.refuse_destroy => {
                            answer["notDestroyed"][&key] = json!({"type": "forbidden"});
                        }
                        Some(at) => {
                            state.emails.remove(at);
                            destroyed.push(id);
                        }
                        None => answer["notDestroyed"][&key] = json!({"type": "notFound"}),
                    }
                }
                if !destroyed.is_empty() {
                    answer["destroyed"] = Value::Array(destroyed);
                }
                vec![json!(["Email/set", answer, call[2]])]
            }
            Some("EmailSubmission/set") => {
                state.submissions.push(call[1].clone());
                if let Some(error) = state.method_error.clone() {
                    vec![json!(["error", error, call[2]])]
                } else {
                    let creates = call[1]["create"].as_object().cloned().unwrap_or_default();
                    let mut answer = json!({"accountId": "a1", "newState": "s2"});
                    let mut implicit = Vec::new();
                    for (create_id, submission) in creates {
                        if let Some(refusal) = state.refuse.clone() {
                            answer["notCreated"][&create_id] = refusal;
                            continue;
                        }
                        answer["created"][&create_id] = json!({"id": "sub1"});
                        let patch = &call[1]["onSuccessUpdateEmail"][format!("#{create_id}")];
                        if let Some(email) = state
                            .emails
                            .iter_mut()
                            .find(|email| email["id"] == submission["emailId"])
                            && let Some(keywords) = patch.get("keywords")
                        {
                            email["keywords"] = keywords.clone();
                            implicit.push(submission["emailId"].clone());
                        }
                    }
                    let mut responses = vec![json!(["EmailSubmission/set", answer, call[2]])];
                    if !implicit.is_empty() {
                        let updated = implicit
                            .iter()
                            .map(|id| (id.as_str().unwrap_or_default().to_owned(), Value::Null))
                            .collect::<serde_json::Map<_, _>>();
                        responses.push(json!(["Email/set", {"accountId": "a1",
                            "newState": "s2", "updated": updated}, call[2]]));
                    }
                    responses
                }
            }
            _ => vec![json!(["error", {"type": "unknownMethod"}, call[2]])],
        };
        if call[0] == "EmailSubmission/set" && state.lose_answer {
            // The submission went through; its answer never arrives.
            let _ = stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
            return;
        }
        json!({"methodResponses": responses, "sessionState": "s1"})
    };
    let body = answer.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
}

fn email(id: &str, message_id: &str, keywords: &[&str], received_at: i64) -> Value {
    let keywords = keywords
        .iter()
        .map(|keyword| ((*keyword).to_owned(), Value::Bool(true)))
        .collect::<serde_json::Map<_, _>>();
    json!({"id": id, "messageId": [message_id], "keywords": keywords,
           "receivedAt": received_at})
}

fn message() -> OutgoingMessage {
    OutgoingMessage {
        from_email: "rob@example.com".to_owned(),
        from_name: None,
        to: vec!["ada@example.com".to_owned()],
        cc: Vec::new(),
        bcc: vec!["Bea <bea@example.com>".to_owned()],
        reply_to: Vec::new(),
        subject: "Lunch".to_owned(),
        text_body: "Noon?".to_owned(),
        attachments: Vec::new(),
        message_id: Some("lunch@example.com".to_owned()),
        in_reply_to: None,
        references: Vec::new(),
        reply_action: None,
    }
}

const CLAIMED: i64 = 1_800_000_000;

/// The submission keyword set, as the fake server stores it.
fn keywords(keyword: &str) -> Value {
    json!({ keyword: true })
}

/// The refusal `error` carries, if it is one.
fn refusal(error: &anyhow::Error) -> Option<&SubmissionRefused> {
    error.chain().find_map(|cause| cause.downcast_ref())
}

#[tokio::test]
async fn an_interrupted_send_is_known_by_its_copys_keywords() {
    let server = FakeServer::start(vec![
        email("other", "other@example.com", &["$submitted"], CLAIMED),
        // Imported long before the attempt: not its copy, whatever it
        // says.
        email(
            "old",
            "lunch@example.com",
            &["$submitted"],
            CLAIMED - 3 * 86_400,
        ),
    ]);
    assert_eq!(
        server.find("<lunch@example.com>", Some(CLAIMED)).await,
        SubmissionTrace::None,
        "no copy since the attempt began: it never reached the server"
    );
    assert_eq!(
        server.find("lunch@example.com", None).await,
        SubmissionTrace::Submitted,
        "without a claim time, every email is looked at"
    );

    // The send stopped between the import and the submission.
    server.state.lock().expect("the state").emails.push(email(
        "e1",
        "lunch@example.com",
        &["$submitpending"],
        CLAIMED,
    ));
    assert_eq!(
        server.find("lunch@example.com", Some(CLAIMED)).await,
        SubmissionTrace::Imported("e1".to_owned())
    );

    // Submitting it swaps the keyword in the same call.
    submit_imported_message(
        &server.endpoint,
        "rob@example.com",
        "pw",
        AuthKind::Password,
        "rob@example.com",
        "e1",
        &message(),
    )
    .await
    .expect("submitted");
    let submission = server.state.lock().expect("the state").submissions[0].clone();
    let (create_id, created) = submission["create"]
        .as_object()
        .and_then(|create| create.iter().next())
        .expect("one submission");
    assert_eq!(created["emailId"], "e1");
    assert_eq!(created["identityId"], "i1");
    assert_eq!(created["envelope"]["mailFrom"]["email"], "rob@example.com");
    let recipients = created["envelope"]["rcptTo"]
        .as_array()
        .expect("recipients")
        .iter()
        .map(|address| address["email"].clone())
        .collect::<Vec<_>>();
    assert_eq!(recipients, ["ada@example.com", "bea@example.com"]);
    assert_eq!(
        submission["onSuccessUpdateEmail"],
        json!({format!("#{create_id}"): {"keywords": {"$submitted": true}}}),
        "the copy's keywords say the submission happened"
    );
    assert_eq!(
        server.find("lunch@example.com", Some(CLAIMED)).await,
        SubmissionTrace::Submitted
    );

    // A copy imported before Zmail marked copies proves nothing.
    server.state.lock().expect("the state").emails.push(email(
        "e2",
        "unmarked@example.com",
        &["$seen"],
        CLAIMED,
    ));
    assert_eq!(
        server.find("unmarked@example.com", Some(CLAIMED)).await,
        SubmissionTrace::Unknown
    );
}

#[tokio::test]
async fn a_refused_submission_is_an_error_with_the_servers_reason() {
    let server = FakeServer::start(vec![email(
        "e1",
        "lunch@example.com",
        &["$submitpending"],
        CLAIMED,
    )]);
    server.state.lock().expect("the state").refuse =
        Some(json!({"type": "forbiddenToSend", "description": "not today"}));
    let error = submit_imported_message(
        &server.endpoint,
        "rob@example.com",
        "pw",
        AuthKind::Password,
        "rob@example.com",
        "e1",
        &message(),
    )
    .await
    .expect_err("refused");
    assert!(error.to_string().contains("not today"), "{error:#}");
    // The interrupted send's copy goes with the refusal (#406).
    assert_eq!(
        server.find("lunch@example.com", Some(CLAIMED)).await,
        SubmissionTrace::None
    );
}

#[tokio::test]
async fn a_refused_send_destroys_its_copy_and_says_why() {
    let server = FakeServer::start(Vec::new());
    server.with_state(|state| {
        state.refuse = Some(json!({"type": "forbiddenFrom",
            "description": "rob@example.com may not send as that address"}));
    });
    let error = server.send(&message()).await.expect_err("refused");
    let refused = refusal(&error).expect("a refusal");
    assert_eq!(
        refused,
        &SubmissionRefused {
            error_type: "forbiddenFrom".to_owned(),
            description: Some("rob@example.com may not send as that address".to_owned()),
            unsent_copy: None,
        }
    );
    assert!(refused.is_permanent());
    assert_eq!(
        error.to_string(),
        "the JMAP server refused the submission: rob@example.com may not send as that address"
    );

    // Imported, refused, then destroyed: exactly that copy, nothing else.
    let imported = server.calls("Email/import");
    assert_eq!(imported.len(), 1);
    let import = imported[0]["emails"]
        .as_object()
        .and_then(|emails| emails.values().next())
        .expect("one import")
        .clone();
    assert_eq!(import["mailboxIds"], json!({"m-sent": true}));
    assert_eq!(import["keywords"], keywords("$submitpending"));
    let submitted = server.with_state(|state| state.submissions.clone());
    assert_eq!(submitted.len(), 1);
    let destroys = server.calls("Email/set");
    assert_eq!(
        destroys,
        [json!({"accountId": "a1", "destroy": ["new1"]})],
        "one Email/set, destroying the refused copy only"
    );
    assert!(
        server.copies("lunch@example.com").is_empty(),
        "Sent keeps no copy of a message that was not sent"
    );
}

#[tokio::test]
async fn a_copy_that_cannot_be_destroyed_is_named_in_the_refusal() {
    let server = FakeServer::start(Vec::new());
    server.with_state(|state| {
        state.refuse = Some(json!({"type": "invalidRecipients",
            "invalidRecipients": ["ada@example"]}));
        state.refuse_destroy = true;
    });
    let error = server.send(&message()).await.expect_err("refused");
    let refused = refusal(&error).expect("a refusal");
    assert_eq!(refused.unsent_copy.as_deref(), Some("new1"));
    assert_eq!(
        error.to_string(),
        "the JMAP server refused the submission: invalidRecipients",
        "a refusal without a description gives its type"
    );
    assert_eq!(
        server.copies("lunch@example.com"),
        [keywords("$submitpending")],
        "the copy is still marked unsent"
    );

    // The next attempt removes it before it imports its own.
    server.with_state(|state| {
        state.refuse = None;
        state.refuse_destroy = false;
    });
    server.resend(&message()).await.expect("sent");
    assert_eq!(
        server.calls("Email/set").last(),
        Some(&json!({"accountId": "a1", "destroy": ["new1"]}))
    );
    assert_eq!(
        server.copies("lunch@example.com"),
        [keywords("$submitted")],
        "one copy, the one that went"
    );
    assert_eq!(server.with_state(|state| state.submissions.len()), 2);
}

#[tokio::test]
async fn a_send_whose_answer_is_lost_keeps_its_copy_and_is_not_sent_again() {
    let server = FakeServer::start(Vec::new());
    server.with_state(|state| state.lose_answer = true);
    let error = server.send(&message()).await.expect_err("no answer");
    assert!(
        refusal(&error).is_none(),
        "no answer is no refusal: {error:#}"
    );
    assert!(
        server.calls("Email/set").is_empty(),
        "a copy that may have gone is never destroyed"
    );
    assert_eq!(server.copies("lunch@example.com"), [keywords("$submitted")]);

    // The retry finds the submission happened: nothing more is sent.
    server.with_state(|state| state.lose_answer = false);
    server.resend(&message()).await.expect("settled");
    assert_eq!(server.calls("Email/import").len(), 1, "no second import");
    assert_eq!(
        server.with_state(|state| state.submissions.len()),
        1,
        "no second submission"
    );
    assert!(server.calls("Email/set").is_empty());
    assert_eq!(server.copies("lunch@example.com"), [keywords("$submitted")]);
}

#[tokio::test]
async fn a_retry_replaces_the_copy_that_was_never_submitted() {
    let server = FakeServer::start(vec![
        email("e1", "lunch@example.com", &["$submitpending"], CLAIMED),
        // An older Zmail's copy says nothing either way; it stays.
        email("e2", "lunch@example.com", &["$seen"], CLAIMED),
        email("e3", "other@example.com", &["$submitpending"], CLAIMED),
    ]);
    server.resend(&message()).await.expect("sent");
    assert_eq!(
        server.calls("Email/set"),
        [json!({"accountId": "a1", "destroy": ["e1"]})],
        "only this message's unsent copy is destroyed"
    );
    let submission = server.with_state(|state| state.submissions.clone());
    assert_eq!(submission.len(), 1);
    let created = submission[0]["create"]
        .as_object()
        .and_then(|create| create.values().next())
        .expect("one submission")
        .clone();
    assert_eq!(created["emailId"], "new1", "the new import is the one sent");
    let mut copies = server.copies("lunch@example.com");
    copies.sort_by_key(Value::to_string);
    assert_eq!(copies, [keywords("$seen"), keywords("$submitted")]);
}

#[tokio::test]
async fn a_retry_sends_nothing_while_the_unsent_copy_cannot_be_removed() {
    let server = FakeServer::start(vec![email(
        "e1",
        "lunch@example.com",
        &["$submitpending"],
        CLAIMED,
    )]);
    server.with_state(|state| state.refuse_destroy = true);
    let error = server.resend(&message()).await.expect_err("not removed");
    assert!(refusal(&error).is_none(), "{error:#}");
    assert!(server.calls("Email/import").is_empty());
    assert!(server.with_state(|state| state.submissions.is_empty()));
    assert_eq!(
        server.copies("lunch@example.com"),
        [keywords("$submitpending")]
    );
}

#[test]
fn submission_answers_say_whether_the_server_refused() {
    let answer = |arguments: Value| json!(["EmailSubmission/set", arguments, "c0"]);
    let refused = |response: Value| match submission_created(Some(&response), "k1") {
        Err(SubmitError::Refused(refused)) => Some(refused),
        Err(SubmitError::Unknown(_)) => None,
        Ok(()) => panic!("{response} is no refusal"),
    };

    assert!(
        submission_created(
            Some(&answer(json!({"created": {"k1": {"id": "s1"}}}))),
            "k1"
        )
        .is_ok()
    );

    // Refusals RFC 8621 (section 7.5) defines, and the shapes Apache James
    // 3.9 gives for a From the user may not send as and an address it
    // cannot parse: the message is not sent, and sending it again as it is
    // cannot work.
    for (error_type, description) in [
        (
            "forbiddenMailFrom",
            "Attempt to send a mail whose MimeMessage From and Sender fields not allowed \
             for connected user: List(other@localhost)",
        ),
        (
            "forbiddenFrom",
            "Attempt to send a mail whose envelope From not allowed for connected user: \
             other@localhost",
        ),
        (
            "invalidArguments",
            "'/envelope/rcptTo(0)/email' property is not valid: Invalid mailAddress: \
             local-part (user account) ended with a \".\", which is invalid in address \
             'nobody.@localhost'",
        ),
        ("noRecipients", "Attempt to send a mail with no recipients"),
        ("invalidRecipients", "ada@example"),
        ("tooManyRecipients", "at most 50"),
        ("forbiddenToSend", "sending is suspended"),
        ("invalidEmail", "no From"),
        ("tooLarge", "over 25 MB"),
    ] {
        let refused = refused(answer(json!({"notCreated": {"k1": {
            "type": error_type, "description": description}}})))
        .unwrap_or_else(|| panic!("{error_type} is a refusal"));
        assert_eq!(refused.error_type, error_type);
        assert!(refused.is_permanent(), "{error_type}");
        assert_eq!(
            refused.to_string(),
            format!("the JMAP server refused the submission: {description}")
        );
    }
    // Refused for now: nothing went, and it may go later.
    for error_type in ["rateLimit", "serverFail", "somethingNew"] {
        let refused = refused(answer(json!({"notCreated": {"k1": {"type": error_type}}})))
            .unwrap_or_else(|| panic!("{error_type} is a refusal"));
        assert!(!refused.is_permanent(), "{error_type}");
    }

    // A method error leaves the server as it was (RFC 8620, section 3.6.2).
    let method_error = refused(json!(["error", {"type": "forbidden"}, "c0"])).expect("refused");
    assert!(method_error.is_permanent());
    let unavailable =
        refused(json!(["error", {"type": "serverUnavailable"}, "c0"])).expect("refused");
    assert!(!unavailable.is_permanent());

    // Answers that do not say whether it went.
    for unknown in [
        json!(["error", {"type": "serverPartialFail"}, "c0"]),
        answer(json!({"created": {"other": {"id": "s1"}}})),
        answer(json!({})),
        json!(["Email/set", {}, "c0"]),
        json!("garbage"),
    ] {
        assert!(refused(unknown.clone()).is_none(), "{unknown} is unknown");
    }
    assert!(matches!(
        submission_created(None, "k1"),
        Err(SubmitError::Unknown(_))
    ));
}
