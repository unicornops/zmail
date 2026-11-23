//! What the write path sends and how it treats each answer, pinned while
//! the writes moved from `jmap-client` to `jmap-wire` (ADR 0008, step 3,
//! #132): written and passing against `jmap-client` first, then kept
//! across the move but for the two fixes made on purpose (a keyword is
//! cleared with `null`; a move changes only the mailboxes it names),
//! whose pinned text changed in a diff of its own. It stays as the
//! regression test of the write-back queue's JMAP side, the Sent copy's
//! upload and import, and attachment downloads.
//!
//! A server on loopback keeps a few emails, applies `Email/set` updates
//! as RFC 8620 section 5.3 says and records every HTTP request. Zmail
//! writes through its public functions, and what it sent, what it
//! reported and what the server then holds are compared with the text
//! files in `write_characterisation`. `ZMAIL_UPDATE_SNAPSHOTS=1` rewrites
//! them; review the diff, as any change is a change on the wire or in
//! what users are told.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};
use zmail_core::{
    AuthKind, BlobStore, BlobWriter, CachePolicySummary, IdentitySummary, OutgoingMessage,
    PendingOp, PendingOpKind, ProtocolKind, SyncTarget,
};

use crate::{apply_pending_ops, discover_and_sync, submit_outgoing_message};

/// What the server does instead of its normal answer.
#[derive(Clone)]
enum Fault {
    /// Every update of an `Email/set` is refused with this SetError.
    NotUpdated(Value),
    /// `Email/set` names the email in neither `updated` nor `notUpdated`.
    Unanswered,
    /// The method named is answered with this method-level error.
    Method(&'static str, Value),
    /// Requests to paths starting so get this status, content type and
    /// body.
    Http(&'static str, u16, &'static str, &'static str),
    /// Requests to paths starting so are closed without an answer.
    Dropped(&'static str),
    /// Every import is refused with this SetError.
    NotCreated(Value),
    /// The import's `created` entry has no `id`.
    CreatedWithoutId,
    /// The upload's answer has no `blobId`.
    UploadWithoutBlobId,
}

#[derive(Default)]
struct State {
    /// Emails by id: `mailboxIds`, `keywords`, and for the sync whatever
    /// else `Email/get` should return.
    emails: BTreeMap<String, Value>,
    /// Blobs by id, for the download endpoint.
    blobs: BTreeMap<String, Vec<u8>>,
    /// Refuse `"keywords/x": false` as a server that keeps to RFC 8621
    /// section 4.1.1 (a keyword's value "MUST be true") does, instead
    /// of reading it as removing the keyword.
    strict: bool,
    fault: Option<Fault>,
    /// Every request, one line each.
    seen: Vec<String>,
    uploads: usize,
}

struct Server {
    endpoint: String,
    state: Arc<Mutex<State>>,
}

/// Attachment blobs kept in memory: the pinned text has their names,
/// sizes and first bytes.
#[derive(Debug, Default)]
struct KeptBlobs(Mutex<Vec<String>>);

impl BlobWriter for KeptBlobs {
    fn write_blob(&self, logical_name: &str, contents: &[u8]) -> std::io::Result<String> {
        let start = String::from_utf8_lossy(&contents[..contents.len().min(12)]).into_owned();
        self.0.lock().expect("the blobs").push(format!(
            "{logical_name}: {} bytes starting {start:?}",
            contents.len()
        ));
        Ok(format!("blobs/{logical_name}"))
    }

    fn location(&self) -> &Path {
        Path::new("blobs")
    }
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
        let mut state = State::default();
        // One email in the Inbox alone, one in the Inbox and under a
        // label, as servers with labels have them.
        state.emails.insert(
            "e1".to_owned(),
            json!({"id": "e1", "mailboxIds": {"m-inbox": true}, "keywords": {"$seen": true}}),
        );
        state.emails.insert(
            "e2".to_owned(),
            json!({"id": "e2", "mailboxIds": {"m-inbox": true, "m-label": true},
                   "keywords": {"$seen": true, "$flagged": true}}),
        );
        let state = Arc::new(Mutex::new(state));
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

    fn with_state<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        change(&mut self.state.lock().expect("the state"))
    }

    fn target(&self, blobs: Arc<KeptBlobs>) -> SyncTarget {
        SyncTarget {
            id: 1,
            display_name: "Recorded".to_owned(),
            protocol: ProtocolKind::Jmap,
            auth: AuthKind::Password,
            endpoint: self.endpoint.clone(),
            username: "rob@example.com".to_owned(),
            cache_policy: CachePolicySummary::performance_first(),
            identities: vec![IdentitySummary {
                id: 1,
                display_name: "Rob".to_owned(),
                email_address: "rob@example.com".to_owned(),
                signature: String::new(),
                reply_to: String::new(),
                is_primary: true,
            }],
            blobs: BlobStore::new(blobs),
            checkpoint: None,
            oauth2: None,
        }
    }

    /// Applies `ops` and returns what was sent after the session and
    /// the mailbox lookup, what was reported, and the emails' mailboxes
    /// and keywords afterwards.
    async fn apply(&self, ops: &[PendingOp]) -> String {
        self.with_state(|state| state.seen.clear());
        let report = apply_pending_ops(&self.target(Arc::default()), "pw", ops).await;
        let mut text = String::new();
        for line in self.with_state(|state| std::mem::take(&mut state.seen)) {
            if !line.starts_with("GET /session") && !line.contains("Mailbox/get") {
                writeln!(text, "  sent: {line}").expect("writes");
            }
        }
        match report {
            Ok(report) => {
                writeln!(text, "  applied: {:?}", report.applied).expect("writes");
                for (id, error) in report.failed {
                    writeln!(text, "  failed {id}: {}", self.shown(&error)).expect("writes");
                }
            }
            Err(error) => {
                writeln!(text, "  error: {}", self.shown(&format!("{error:#}"))).expect("writes");
            }
        }
        self.with_state(|state| {
            for (id, email) in &state.emails {
                writeln!(
                    text,
                    "  {id}: in {} with {}",
                    canonical(&email["mailboxIds"]),
                    canonical(&email["keywords"])
                )
                .expect("writes");
            }
        });
        text
    }

    /// `text` with the server's address (its port changes from run to
    /// run) written as `http://server`.
    fn shown(&self, text: &str) -> String {
        text.replace(&self.endpoint, "http://server")
            .replace(self.endpoint.trim_start_matches("http://"), "server")
    }
}

fn op(id: i64, kind: PendingOpKind, email: &str, from: &str, to: Option<&str>) -> PendingOp {
    PendingOp {
        id,
        account_id: 1,
        kind,
        remote_id: email.to_owned(),
        source_mailbox: from.to_owned(),
        target_mailbox: to.map(str::to_owned),
        attempts: 0,
        last_error: None,
    }
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status} Answer\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(body);
}

fn serve(mut stream: TcpStream, base: &str, state: &Mutex<State>) {
    let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut headers = BTreeMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length = headers
        .get("content-length")
        .and_then(|length| length.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    let mut parts = request_line.split_whitespace();
    let (method, path) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    // What a blob request carried besides its URL: these are not JMAP
    // API calls, so their headers are part of what is pinned.
    let header = |name: &str| headers.get(name).map_or("none", String::as_str).to_owned();
    let carried = format!(
        "authorization: {}, accept: {}, accept-encoding: {}",
        if headers.contains_key("authorization") {
            "sent"
        } else {
            "none"
        },
        header("accept"),
        header("accept-encoding"),
    );

    let mut state = state.lock().expect("the state");
    let fault = state.fault.clone();
    let line = if path == "/session" || path == "/.well-known/jmap" {
        "GET /session".to_owned()
    } else if path.starts_with("/upload/") {
        state.uploads += 1;
        let text = String::from_utf8_lossy(&body);
        format!(
            "{method} {path} ({carried}, content-type: {}): a message of {} lines to {}",
            header("content-type"),
            text.lines().count(),
            text.lines()
                .find_map(|line| line.strip_prefix("To: "))
                .unwrap_or("nobody"),
        )
    } else if path.starts_with("/download/") {
        format!("{method} {path} ({carried})")
    } else {
        let request: Value = serde_json::from_slice(&body).unwrap_or_default();
        format!(
            "{method} {path} using {} {}",
            request["using"],
            canonical(&Value::Array(
                request["methodCalls"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(without_ids)
                    .collect()
            ))
        )
    };
    state.seen.push(line);

    match &fault {
        Some(Fault::Http(prefix, status, content_type, text)) if path.starts_with(prefix) => {
            return respond(&mut stream, *status, content_type, text.as_bytes());
        }
        Some(Fault::Dropped(prefix)) if path.starts_with(prefix) => return,
        _ => {}
    }

    if method == "GET" && (path == "/session" || path == "/.well-known/jmap") {
        let session = json!({
            "capabilities": {
                "urn:ietf:params:jmap:core": {
                    "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
                    "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
                    "maxCallsInRequest": 16, "maxObjectsInGet": 500,
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
        });
        return respond(
            &mut stream,
            200,
            "application/json",
            session.to_string().as_bytes(),
        );
    }
    if path.starts_with("/upload/") {
        let mut answer = json!({"accountId": "a1", "blobId": format!("b{}", state.uploads),
                                "type": header("content-type"), "size": body.len()});
        if matches!(fault, Some(Fault::UploadWithoutBlobId)) {
            answer = json!({"accountId": "a1"});
        }
        return respond(
            &mut stream,
            201,
            "application/json",
            answer.to_string().as_bytes(),
        );
    }
    if let Some(rest) = path.strip_prefix("/download/a1/") {
        let blob_id = rest.split(['/', '?']).next().unwrap_or_default();
        return match state.blobs.get(blob_id) {
            Some(blob) => respond(&mut stream, 200, "application/octet-stream", blob),
            None => respond(
                &mut stream,
                404,
                "application/problem+json",
                br#"{"type":"about:blank","status":404,"detail":"The blob does not exist"}"#,
            ),
        };
    }

    let request: Value = serde_json::from_slice(&body).unwrap_or_default();
    let call = &request["methodCalls"][0];
    let name = call[0].as_str().unwrap_or_default();
    let arguments = match &fault {
        Some(Fault::Method(method, error)) if *method == name => Err(error.clone()),
        _ => answer(&mut state, name, &call[1], fault.as_ref()),
    };
    let response = match arguments {
        Ok(arguments) => json!([name, arguments, call[2]]),
        Err(error) => json!(["error", error, call[2]]),
    };
    let body = json!({"methodResponses": [response], "sessionState": "s1"}).to_string();
    respond(&mut stream, 200, "application/json", body.as_bytes());
}

/// `value` as JSON text with the members of every object in
/// alphabetical order. JSON gives their order no meaning, and
/// `serde_json` keeps it or sorts it depending on a feature that other
/// crates of the workspace turn on, so the pinned text must not depend
/// on it.
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(members) => {
            let mut members = members.iter().collect::<Vec<_>>();
            members.sort_by(|a, b| a.0.cmp(b.0));
            let members = members
                .into_iter()
                .map(|(name, value)| format!("{}:{}", Value::from(name.as_str()), canonical(value)))
                .collect::<Vec<_>>();
            format!("{{{}}}", members.join(","))
        }
        Value::Array(items) => {
            let items = items.iter().map(canonical).collect::<Vec<_>>();
            format!("[{}]", items.join(","))
        }
        other => other.to_string(),
    }
}

/// A method call as it is pinned: without its call id, and with the
/// creation ids of an import written `new0`, `new1`... Both are names
/// the client makes up for the length of one request, which the two
/// libraries choose differently (`wire_parity_tests.rs` states how).
fn without_ids(call: &Value) -> Value {
    let mut arguments = call[1].clone();
    if let Some(emails) = arguments.get_mut("emails").and_then(Value::as_object_mut) {
        *emails = std::mem::take(emails)
            .into_iter()
            .enumerate()
            .map(|(index, (_, email))| (format!("new{index}"), email))
            .collect();
    }
    json!([call[0], arguments])
}

fn answer(
    state: &mut State,
    name: &str,
    arguments: &Value,
    fault: Option<&Fault>,
) -> Result<Value, Value> {
    match name {
        "Identity/get" => Ok(json!({"accountId": "a1", "state": "s1",
            "list": [{"id": "i1", "email": "rob@example.com", "name": "Rob"}], "notFound": []})),
        "Mailbox/get" => Ok(json!({"accountId": "a1", "state": "s1", "list": [
            {"id": "m-inbox", "name": "Inbox", "role": "inbox"},
            {"id": "m-archive", "name": "Archive", "role": "archive"},
            {"id": "m-trash", "name": "Trash", "role": "trash"},
            {"id": "m-sent", "name": "Sent", "role": "sent"},
            {"id": "m-label", "name": "Receipts", "role": null}
        ], "notFound": []})),
        "Email/query" => Ok(json!({"accountId": "a1", "queryState": "q1",
        "canCalculateChanges": false,
        "position": arguments["position"].as_u64().unwrap_or(0),
        "ids": if arguments["position"].as_u64().unwrap_or(0) == 0 {
            state.emails.keys().cloned().collect::<Vec<_>>()
        } else {
            Vec::new()
        }})),
        "Email/get" => {
            let ids = arguments["ids"].as_array().cloned().unwrap_or_default();
            let list = ids
                .iter()
                .filter_map(|id| state.emails.get(id.as_str()?).cloned())
                .collect::<Vec<_>>();
            let not_found = ids
                .iter()
                .filter(|id| !state.emails.contains_key(id.as_str().unwrap_or_default()))
                .cloned()
                .collect::<Vec<_>>();
            Ok(json!({"accountId": "a1", "state": "s1", "list": list, "notFound": not_found}))
        }
        "Email/set" => {
            let mut answer = json!({"accountId": "a1", "oldState": "s1", "newState": "s2"});
            let updates = arguments["update"].as_object().cloned().unwrap_or_default();
            for (id, patch) in updates {
                match fault {
                    Some(Fault::Unanswered) => continue,
                    Some(Fault::NotUpdated(error)) => {
                        answer["notUpdated"][&id] = error.clone();
                        continue;
                    }
                    _ => {}
                }
                let strict = state.strict;
                match state.emails.get_mut(&id) {
                    None => answer["notUpdated"][&id] = json!({"type": "notFound"}),
                    Some(email) => match patched(email, &patch, strict) {
                        Ok(email_now) => {
                            *email = email_now;
                            answer["updated"][&id] = Value::Null;
                        }
                        Err(error) => answer["notUpdated"][&id] = error,
                    },
                }
            }
            let mut destroyed = Vec::new();
            for id in arguments["destroy"].as_array().cloned().unwrap_or_default() {
                let key = id.as_str().unwrap_or_default().to_owned();
                if state.emails.remove(&key).is_some() {
                    destroyed.push(id);
                } else {
                    answer["notDestroyed"][&key] = json!({"type": "notFound"});
                }
            }
            if !destroyed.is_empty() {
                answer["destroyed"] = Value::Array(destroyed);
            }
            Ok(answer)
        }
        "Email/import" => {
            let mut answer = json!({"accountId": "a1", "oldState": "s1", "newState": "s2"});
            let imports = arguments["emails"].as_object().cloned().unwrap_or_default();
            for (creation_id, import) in imports {
                match fault {
                    Some(Fault::NotCreated(error)) => {
                        answer["notCreated"][&creation_id] = error.clone();
                    }
                    Some(Fault::CreatedWithoutId) => {
                        answer["created"][&creation_id] = json!({"threadId": "t9"});
                    }
                    _ => {
                        let id = format!("new{}", state.emails.len() + 1);
                        state.emails.insert(
                            id.clone(),
                            json!({"id": id, "mailboxIds": import["mailboxIds"],
                                   "keywords": import["keywords"]}),
                        );
                        answer["created"][&creation_id] = json!({"id": id,
                            "blobId": import["blobId"], "threadId": "t9", "size": 100});
                    }
                }
            }
            Ok(answer)
        }
        "EmailSubmission/set" => {
            let mut answer = json!({"accountId": "a1", "newState": "s2"});
            for creation_id in arguments["create"].as_object().into_iter().flatten() {
                answer["created"][creation_id.0] = json!({"id": "sub1"});
            }
            Ok(answer)
        }
        _ => Err(json!({"type": "unknownMethod"})),
    }
}

/// `email` with a PatchObject applied (RFC 8620 section 5.3), or the
/// SetError a server gives for it.
fn patched(email: &Value, patch: &Value, strict: bool) -> Result<Value, Value> {
    let mut email = email.clone();
    for (path, value) in patch.as_object().into_iter().flatten() {
        match path.split_once('/') {
            None if matches!(path.as_str(), "mailboxIds" | "keywords") => {
                email[path] = value.clone();
            }
            Some((property @ ("mailboxIds" | "keywords"), key)) => {
                let set = email[property].as_object_mut().expect("a set");
                match value {
                    Value::Bool(true) => {
                        set.insert(key.to_owned(), Value::Bool(true));
                    }
                    Value::Null => {
                        set.remove(key);
                    }
                    Value::Bool(false) if !strict => {
                        set.remove(key);
                    }
                    _ => {
                        return Err(json!({"type": "invalidProperties",
                            "properties": [path],
                            "description": format!("{path} must be true or null")}));
                    }
                }
            }
            _ => return Err(json!({"type": "invalidProperties", "properties": [path]})),
        }
    }
    if email["mailboxIds"]
        .as_object()
        .is_none_or(|ids| ids.is_empty())
    {
        return Err(
            json!({"type": "invalidProperties", "properties": ["mailboxIds"],
            "description": "an email must be in at least one mailbox"}),
        );
    }
    Ok(email)
}

/// Compares `actual` with the pinned text `name`, or writes it when
/// `ZMAIL_UPDATE_SNAPSHOTS` is set.
fn assert_pinned(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/write_characterisation")
        .join(name);
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
        "{name} differs from what is pinned.\n--- pinned\n{pinned}\n--- now\n{actual}"
    );
}

/// Read, unread, flag and unflag, on a server that reads `false` as
/// removing a keyword and on one that keeps to the RFC and refuses it.
#[tokio::test]
async fn keyword_changes_send_what_they_sent_before() {
    let mut text = String::new();
    for (strict, title) in [
        (false, "a server that takes false for removing a keyword"),
        (
            true,
            "a server that takes only true or null (RFC 8620 section 5.3)",
        ),
    ] {
        writeln!(text, "# {title}").expect("writes");
        for (kind, email) in [
            (PendingOpKind::MarkUnread, "e1"),
            (PendingOpKind::MarkRead, "e1"),
            (PendingOpKind::SetFlagged, "e1"),
            (PendingOpKind::ClearFlagged, "e2"),
        ] {
            let server = Server::start();
            server.with_state(|state| state.strict = strict);
            writeln!(text, "{kind:?} {email}").expect("writes");
            text.push_str(&server.apply(&[op(1, kind, email, "Inbox", None)]).await);
        }
    }
    assert_pinned("keywords.txt", &text);
}

/// Moving, archiving, deleting to Trash and undoing them, for an email
/// in one mailbox and for one that is also under a label.
#[tokio::test]
async fn moves_send_what_they_sent_before() {
    let mut text = String::new();
    let moves: [(&str, &str, &str, Option<&str>); 8] = [
        (
            "archive an email in one mailbox",
            "e1",
            "Inbox",
            Some("Archive"),
        ),
        (
            "archive an email that is also under a label",
            "e2",
            "Inbox",
            Some("Archive"),
        ),
        (
            "delete an email that is also under a label",
            "e2",
            "Inbox",
            Some("Trash"),
        ),
        (
            "move an email out of its label only",
            "e2",
            "Receipts",
            Some("Archive"),
        ),
        (
            "move to the mailbox it is in already",
            "e2",
            "Inbox",
            Some("Receipts"),
        ),
        (
            "move from a mailbox the server does not have",
            "e2",
            "Renamed since",
            Some("Archive"),
        ),
        (
            "move to a mailbox the server does not have",
            "e1",
            "Inbox",
            Some("No such mailbox"),
        ),
        ("move without a target", "e1", "Inbox", None),
    ];
    for (title, email, from, to) in moves {
        let server = Server::start();
        writeln!(text, "{title}").expect("writes");
        text.push_str(
            &server
                .apply(&[op(1, PendingOpKind::Move, email, from, to)])
                .await,
        );
    }

    let server = Server::start();
    writeln!(
        text,
        "archive and undo, an email that is also under a label"
    )
    .expect("writes");
    text.push_str(
        &server
            .apply(&[
                op(1, PendingOpKind::Move, "e2", "Inbox", Some("Archive")),
                op(2, PendingOpKind::Move, "e2", "Archive", Some("Inbox")),
            ])
            .await,
    );
    let server = Server::start();
    writeln!(text, "delete and undo, an email that is also under a label").expect("writes");
    text.push_str(
        &server
            .apply(&[
                op(1, PendingOpKind::Move, "e2", "Inbox", Some("Trash")),
                op(2, PendingOpKind::Move, "e2", "Trash", Some("Inbox")),
            ])
            .await,
    );
    assert_pinned("moves.txt", &text);
}

/// How each answer to a keyword change and to a move is treated: which
/// operations are reported applied, and what the failed ones say.
#[tokio::test]
async fn answers_to_changes_are_treated_as_before() {
    let faults: Vec<(&str, Fault)> = vec![
        (
            "notUpdated notFound",
            Fault::NotUpdated(json!({"type": "notFound"})),
        ),
        (
            "notUpdated invalidProperties",
            Fault::NotUpdated(json!({"type": "invalidProperties",
                "properties": ["keywords/$seen"], "description": "not a boolean"})),
        ),
        (
            "notUpdated invalidPatch",
            Fault::NotUpdated(json!({"type": "invalidPatch"})),
        ),
        (
            "notUpdated tooManyKeywords",
            Fault::NotUpdated(json!({"type": "tooManyKeywords"})),
        ),
        (
            "notUpdated of a type from a later RFC",
            Fault::NotUpdated(json!({"type": "mailboxLocked", "description": "try later"})),
        ),
        ("neither updated nor notUpdated", Fault::Unanswered),
        (
            "method error stateMismatch",
            Fault::Method("Email/set", json!({"type": "stateMismatch"})),
        ),
        (
            "method error serverFail",
            Fault::Method(
                "Email/set",
                json!({"type": "serverFail", "description": "Something went wrong"}),
            ),
        ),
        (
            "method error accountReadOnly",
            Fault::Method("Email/set", json!({"type": "accountReadOnly"})),
        ),
        (
            "problem details: limit",
            Fault::Http(
                "/api",
                400,
                "application/problem+json",
                r#"{"type":"urn:ietf:params:jmap:error:limit","status":400,"limit":"maxSizeRequest","detail":"Too large"}"#,
            ),
        ),
        (
            "HTTP 503",
            Fault::Http("/api", 503, "text/plain", "try later"),
        ),
        (
            "not JSON",
            Fault::Http("/api", 200, "text/html", "<html>sign in</html>"),
        ),
        ("the connection drops", Fault::Dropped("/api")),
    ];
    let mut text = String::new();
    for (name, fault) in faults {
        let server = Server::start();
        // The mailbox lookup of a move must still be answered.
        let fault = match fault {
            Fault::Http("/api", ..) | Fault::Dropped("/api") => {
                writeln!(text, "{name}, for a keyword change").expect("writes");
                server.with_state(|state| state.fault = Some(fault));
                text.push_str(
                    &server
                        .apply(&[op(1, PendingOpKind::MarkRead, "e1", "Inbox", None)])
                        .await,
                );
                continue;
            }
            fault => fault,
        };
        server.with_state(|state| state.fault = Some(fault));
        writeln!(text, "{name}").expect("writes");
        text.push_str(
            &server
                .apply(&[
                    op(1, PendingOpKind::MarkRead, "e1", "Inbox", None),
                    op(2, PendingOpKind::Move, "e2", "Inbox", Some("Archive")),
                    op(3, PendingOpKind::SetFlagged, "e1", "Inbox", None),
                ])
                .await,
        );
    }

    // An email the server no longer has.
    let server = Server::start();
    writeln!(text, "an email that is gone").expect("writes");
    text.push_str(
        &server
            .apply(&[
                op(1, PendingOpKind::MarkRead, "gone", "Inbox", None),
                op(2, PendingOpKind::Move, "gone", "Inbox", Some("Archive")),
                op(3, PendingOpKind::Destroy, "gone", "Inbox", None),
            ])
            .await,
    );
    assert_pinned("answers.txt", &text);
}

fn message() -> OutgoingMessage {
    OutgoingMessage {
        from_email: "rob@example.com".to_owned(),
        from_name: None,
        to: vec!["ada@example.com".to_owned()],
        cc: Vec::new(),
        bcc: Vec::new(),
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

/// The Sent copy of a send: the upload, the import, and how each
/// refusal of either is reported. A send that fails here never reaches
/// `EmailSubmission/set`.
#[tokio::test]
async fn the_sent_copy_is_uploaded_and_imported_as_before() {
    let faults: Vec<(&str, Option<Fault>)> = vec![
        ("accepted", None),
        (
            "upload refused with problem details",
            Some(Fault::Http(
                "/upload/",
                413,
                "application/problem+json",
                r#"{"type":"urn:ietf:params:jmap:error:limit","status":413,"limit":"maxSizeUpload","detail":"Too large"}"#,
            )),
        ),
        (
            "upload refused with HTTP 500",
            Some(Fault::Http("/upload/", 500, "text/plain", "broken")),
        ),
        (
            "upload answered with something that is not JSON",
            Some(Fault::Http("/upload/", 200, "text/html", "<html></html>")),
        ),
        (
            "upload answered without a blobId",
            Some(Fault::UploadWithoutBlobId),
        ),
        (
            "the upload's connection drops",
            Some(Fault::Dropped("/upload/")),
        ),
        (
            "import notCreated overQuota",
            Some(Fault::NotCreated(json!({"type": "overQuota"}))),
        ),
        (
            "import notCreated invalidProperties",
            Some(Fault::NotCreated(json!({"type": "invalidProperties",
                "properties": ["mailboxIds"], "description": "no such mailbox"}))),
        ),
        (
            "import notCreated blobNotFound",
            Some(Fault::NotCreated(
                json!({"type": "blobNotFound", "notFound": ["b1"]}),
            )),
        ),
        (
            "import notCreated alreadyExists",
            Some(Fault::NotCreated(
                json!({"type": "alreadyExists", "existingId": "e1"}),
            )),
        ),
        (
            "import created without an id",
            Some(Fault::CreatedWithoutId),
        ),
        (
            "import method error",
            Some(Fault::Method(
                "Email/import",
                json!({"type": "serverFail", "description": "Something went wrong"}),
            )),
        ),
    ];
    let mut text = String::new();
    for (name, fault) in faults {
        let server = Server::start();
        server.with_state(|state| state.fault = fault);
        let sent = submit_outgoing_message(
            &server.endpoint,
            "rob@example.com",
            "pw",
            AuthKind::Password,
            "rob@example.com",
            &message(),
        )
        .await;
        writeln!(text, "{name}").expect("writes");
        for line in server.with_state(|state| std::mem::take(&mut state.seen)) {
            if line.contains("/upload/") || line.contains("Email/import") {
                writeln!(text, "  sent: {}", server.shown(&line)).expect("writes");
            } else if line.contains("EmailSubmission/set") {
                writeln!(text, "  submitted").expect("writes");
            }
        }
        match sent {
            Ok(()) => writeln!(text, "  ok").expect("writes"),
            Err(error) => {
                writeln!(text, "  error: {}", server.shown(&format!("{error:#}"))).expect("writes");
            }
        }
    }
    assert_pinned("import.txt", &text);
}

/// Attachments of a sync: which blobs are asked for and how, what is
/// stored, and what a missing blob, a refusal and an oversized blob do.
#[tokio::test]
async fn attachments_download_as_before() {
    let part = |blob: &str, name: Option<&str>, cid: Option<&str>, size: usize| {
        json!({"partId": blob, "blobId": blob, "type": "application/pdf", "name": name,
               "cid": cid, "size": size, "disposition": "attachment"})
    };
    let server = Server::start();
    server.with_state(|state| {
        state.emails.clear();
        state.emails.insert(
            "e1".to_owned(),
            json!({
                "id": "e1", "threadId": "t1", "mailboxIds": {"m-inbox": true},
                "keywords": {}, "subject": "Reports", "receivedAt": "2026-11-01T10:00:00Z",
                "from": [{"name": "Ada", "email": "ada@example.com"}],
                "hasAttachment": true,
                "attachments": [
                    part("blob-plain", Some("report.pdf"), None, 11),
                    part("blob needing/escapes", Some("a b.pdf"), None, 4),
                    part("blob-inline", None, Some("<logo@example.com>"), 4),
                    part("blob-missing", Some("gone.pdf"), None, 4),
                    part("blob-declared-huge", Some("huge.pdf"), None, 26 * 1024 * 1024),
                    part("blob-really-huge", Some("lies.pdf"), None, 4),
                    json!({"partId": "9", "type": "application/pdf", "name": "no-blob.pdf"}),
                    part("blob-last", Some("last.pdf"), None, 4)
                ]
            }),
        );
        state
            .blobs
            .insert("blob-plain".to_owned(), b"%PDF-report".to_vec());
        // The URL carries it percent-encoded.
        state
            .blobs
            .insert("blob%20needing%2Fescapes".to_owned(), b"%PDF".to_vec());
        state
            .blobs
            .insert("blob-inline".to_owned(), b"\x89PNG".to_vec());
        state.blobs.insert(
            "blob-really-huge".to_owned(),
            vec![b'x'; 25 * 1024 * 1024 + 1],
        );
        state.blobs.insert("blob-last".to_owned(), b"last".to_vec());
    });

    let blobs = Arc::new(KeptBlobs::default());
    let sync = discover_and_sync(&server.target(blobs.clone()), "pw", &[])
        .await
        .expect("the sync runs");
    let mut text = String::new();
    for line in server.with_state(|state| std::mem::take(&mut state.seen)) {
        if line.contains("/download/") {
            writeln!(text, "asked: {}", server.shown(&line)).expect("writes");
        }
    }
    for blob in blobs.0.lock().expect("the blobs").iter() {
        writeln!(text, "stored: {blob}").expect("writes");
    }
    for mailbox in &sync.snapshot.mailboxes {
        for message in mailbox.threads.iter().flat_map(|thread| &thread.messages) {
            for attachment in &message.attachments {
                writeln!(text, "listed in {}: {attachment:?}", mailbox.name).expect("writes");
            }
        }
    }

    // A server that refuses every download: the sync still succeeds.
    for (name, fault) in [
        (
            "HTTP 403",
            Fault::Http("/download/", 403, "text/plain", "no"),
        ),
        ("the connection drops", Fault::Dropped("/download/")),
    ] {
        server.with_state(|state| state.fault = Some(fault));
        let blobs = Arc::new(KeptBlobs::default());
        let sync = discover_and_sync(&server.target(blobs.clone()), "pw", &[])
            .await
            .expect("the sync runs");
        let listed = sync
            .snapshot
            .mailboxes
            .iter()
            .flat_map(|mailbox| &mailbox.threads)
            .flat_map(|thread| &thread.messages)
            .map(|message| message.attachments.len())
            .sum::<usize>();
        writeln!(
            text,
            "every download refused ({name}): the sync succeeds, {} stored, {listed} listed",
            blobs.0.lock().expect("the blobs").len()
        )
        .expect("writes");
    }
    assert_pinned("download.txt", &text);
}
