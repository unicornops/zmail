//! Submitting and finding an interrupted send's copy against a JMAP server
//! on loopback: the session, `Identity/get`, `Email/query`, `Email/get`
//! and `EmailSubmission/set` with `onSuccessUpdateEmail`, applied as
//! RFC 8621 says. The live harness checks the same against James.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};
use zmail_core::{AuthKind, OutgoingMessage};

use crate::{SubmissionTrace, find_submitted_message, submit_imported_message};

/// The server's emails (`id`, `messageId`, `keywords`, `receivedAt` as
/// Unix seconds) and the `EmailSubmission/set` calls it answered.
#[derive(Default)]
struct State {
    emails: Vec<Value>,
    submissions: Vec<Value>,
    /// Refuse submissions, as a server does for a bad envelope.
    refuse: bool,
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
    } else {
        let request: Value = serde_json::from_slice(&body).expect("a JSON request");
        let call = &request["methodCalls"][0];
        let mut state = state.lock().expect("the state");
        let responses = match call[0].as_str() {
            Some("Identity/get") => vec![json!(["Identity/get", {"accountId": "a1",
                "state": "s1", "list": [{"id": "i1", "email": "rob@example.com"}]}, call[2]])],
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
            Some("EmailSubmission/set") => {
                state.submissions.push(call[1].clone());
                let creates = call[1]["create"].as_object().cloned().unwrap_or_default();
                let mut answer = json!({"accountId": "a1", "newState": "s2"});
                let mut implicit = Vec::new();
                for (create_id, submission) in creates {
                    if state.refuse {
                        answer["notCreated"][&create_id] = json!({"type": "forbiddenToSend",
                            "description": "not today"});
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
            _ => vec![json!(["error", {"type": "unknownMethod"}, call[2]])],
        };
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
    server.state.lock().expect("the state").refuse = true;
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
    assert_eq!(
        server.find("lunch@example.com", Some(CLAIMED)).await,
        SubmissionTrace::Imported("e1".to_owned())
    );
}
