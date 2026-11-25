//! What the read path stores for recorded server answers, pinned while
//! the reads moved from `jmap-client` to `jmap-wire` (ADR 0008, step 2,
//! #132): written and passing against `jmap-client` first, then kept
//! unchanged across the move. It stays as the regression test of the
//! mapping from JMAP answers to what the store is given.
//!
//! A server on loopback replays the answers under
//! `crates/jmap-wire/tests/fixtures/<server>` (a directory is replayed
//! when it has `mailbox-get.all.json` and `email-get.sync.json`), Zmail
//! syncs from it through its public functions, and what it would hand
//! the store is compared with the text files in `read_characterisation`,
//! one per server and scenario. `ZMAIL_UPDATE_SNAPSHOTS=1` rewrites
//! them; review the diff, as any change is a change in what users see.

use std::{
    collections::BTreeSet,
    fmt::Write as _,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};
use zmail_core::{
    AuthKind, BlobStore, BlobWriter, CachePolicySummary, IdentitySummary, MailboxSnapshot,
    PendingOp, PendingOpKind, ProtocolKind, SyncTarget,
};

use crate::{JmapSync, apply_pending_ops, discover_and_sync, fetch_message_headers, search_emails};

/// The properties of the light `Email/get` an incremental sync makes for
/// emails whose keywords or mailboxes changed.
const LIGHT_PROPERTIES: [&str; 5] = ["id", "threadId", "mailboxIds", "keywords", "hasAttachment"];
/// How many emails with their bodies the server returns per `Email/get`
/// before it answers `requestTooLarge`, as Apache James does (#389).
const FULL_GET_LIMIT: usize = 5;

/// Blobs kept by name only: the snapshot records the name and the size.
#[derive(Debug, Default)]
struct NamedBlobs;

impl BlobWriter for NamedBlobs {
    fn write_blob(&self, logical_name: &str, _contents: &[u8]) -> std::io::Result<String> {
        Ok(format!("blobs/{logical_name}"))
    }

    fn location(&self) -> &Path {
        Path::new("blobs")
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../jmap-wire/tests/fixtures")
}

/// The arguments of the first method response in a recorded response
/// body.
fn recorded(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
    let body: Value =
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path:?}: {error}"));
    body["methodResponses"][0][1].clone()
}

/// How the server answers instead of replaying, for the error cases.
#[derive(Clone)]
enum Fault {
    /// This method is answered with this method error.
    Method(&'static str, Value),
    /// Every API request gets this status, content type and body.
    Http(u16, &'static str, &'static str),
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) mailboxes: Value,
    /// The answer to the full `Email/get` of a sync.
    full: Value,
    /// The answer to the light `Email/get`, when recorded.
    light: Option<Value>,
    /// Emails the server does not have (yet).
    hidden: BTreeSet<String>,
    /// The answer to `Email/changes`; `cannotCalculateChanges` without.
    pub(crate) changes: Option<Value>,
    fault: Option<Fault>,
    /// Every method call, as `[name, arguments]`.
    pub(crate) calls: Vec<Value>,
}

pub(crate) struct Replay {
    endpoint: String,
    state: Arc<Mutex<State>>,
}

impl Replay {
    fn start(server: &str) -> Self {
        let light = fixtures().join(server).join("email-get.light.json");
        let replay = Self::start_with(server, "all", "sync");
        replay.with_state(|state| state.light = light.exists().then(|| recorded(&light)));
        replay
    }

    /// Replays `mailbox-get.<mailboxes>.json` and `email-get.<emails>.json`
    /// of `server`.
    pub(crate) fn start_with(server: &str, mailboxes: &str, emails: &str) -> Self {
        let directory = fixtures().join(server);
        let state = Arc::new(Mutex::new(State {
            mailboxes: recorded(&directory.join(format!("mailbox-get.{mailboxes}.json"))),
            full: recorded(&directory.join(format!("email-get.{emails}.json"))),
            ..State::default()
        }));
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let base = format!("http://{}", listener.local_addr().expect("an address"));
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

    pub(crate) fn with_state<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        change(&mut self.state.lock().expect("the state"))
    }

    /// The ids of the recorded emails, in the order recorded.
    fn email_ids(&self) -> Vec<String> {
        self.with_state(|state| ids_of(&state.full["list"]))
    }

    /// How many ids each `Email/get` so far asked for, with `full` for
    /// the ones that asked for bodies.
    fn get_sizes(&self) -> Vec<(usize, bool)> {
        self.with_state(|state| {
            state
                .calls
                .iter()
                .filter(|call| call[0] == "Email/get")
                .map(|call| {
                    (
                        call[1]["ids"].as_array().map_or(0, Vec::len),
                        wants_bodies(&call[1]),
                    )
                })
                .collect()
        })
    }

    pub(crate) fn target(&self, checkpoint: Option<String>) -> SyncTarget {
        SyncTarget {
            id: 1,
            display_name: "Recorded".to_owned(),
            protocol: ProtocolKind::Jmap,
            auth: AuthKind::Password,
            endpoint: self.endpoint.clone(),
            username: "me@example.test".to_owned(),
            cache_policy: CachePolicySummary::performance_first(),
            identities: vec![IdentitySummary {
                id: 1,
                display_name: "Me".to_owned(),
                email_address: "primary@example.test".to_owned(),
                signature: String::new(),
                reply_to: String::new(),
                is_primary: true,
            }],
            blobs: BlobStore::new(Arc::new(NamedBlobs)),
            checkpoint,
            oauth2: None,
        }
    }

    /// A sync's result as text, with the server's address (its port
    /// changes from run to run) written as `http://server`.
    fn shown(&self, sync: &JmapSync) -> String {
        format!("{sync:#?}\n").replace(&self.endpoint, "http://server")
    }

    pub(crate) async fn sync(
        &self,
        checkpoint: Option<String>,
        existing: &[MailboxSnapshot],
    ) -> anyhow::Result<JmapSync> {
        discover_and_sync(&self.target(checkpoint), "pw", existing).await
    }
}

fn ids_of(list: &Value) -> Vec<String> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|email| email["id"].as_str().map(str::to_owned))
        .collect()
}

fn wants_bodies(arguments: &Value) -> bool {
    arguments["properties"]
        .as_array()
        .is_some_and(|properties| properties.iter().any(|property| property == "bodyValues"))
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
    let mut respond = |status: &str, content_type: &str, body: &[u8]| {
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(body);
    };

    let path = request_line.split(' ').nth(1).unwrap_or_default();
    if let Some(blob) = path.strip_prefix("/download/a1/") {
        // A blob is its id, so a snapshot shows which was fetched.
        let blob_id = blob.split('/').next().unwrap_or_default();
        if blob_id == "b-missing" {
            respond("404 Not Found", "text/plain", b"no such blob");
        } else {
            respond(
                "200 OK",
                "application/octet-stream",
                format!("contents of {blob_id}").as_bytes(),
            );
        }
        return;
    }
    if request_line.starts_with("GET") {
        let session = json!({
            "capabilities": {
                "urn:ietf:params:jmap:core": {
                    "maxSizeUpload": 50_000_000, "maxConcurrentUpload": 4,
                    "maxSizeRequest": 10_000_000, "maxConcurrentRequests": 4,
                    "maxCallsInRequest": 16, "maxObjectsInGet": 500,
                    "maxObjectsInSet": 500, "collationAlgorithms": []
                },
                "urn:ietf:params:jmap:mail": {}
            },
            "accounts": {"a1": {"name": "me@example.test", "isPersonal": true,
                                 "isReadOnly": false, "accountCapabilities": {}}},
            "primaryAccounts": {"urn:ietf:params:jmap:mail": "a1"},
            "username": "me@example.test",
            "apiUrl": format!("{base}/api"),
            "downloadUrl": format!("{base}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
            "uploadUrl": format!("{base}/upload/{{accountId}}/"),
            "eventSourceUrl": format!("{base}/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
            "state": "s1"
        });
        respond("200 OK", "application/json", session.to_string().as_bytes());
        return;
    }

    let mut state = state.lock().expect("the state");
    if let Some(Fault::Http(status, content_type, body)) = &state.fault {
        respond(&format!("{status} Refused"), content_type, body.as_bytes());
        return;
    }
    let request: Value = serde_json::from_slice(&body).expect("a JSON request");
    let call = &request["methodCalls"][0];
    state.calls.push(json!([call[0], call[1]]));
    let name = call[0].as_str().unwrap_or_default();
    let arguments = &call[1];
    let answer = match &state.fault {
        Some(Fault::Method(method, error)) if *method == name => Err(error.clone()),
        _ => answer(&state, name, arguments),
    };
    let response = match answer {
        Ok(arguments) => json!([name, arguments, call[2]]),
        Err(error) => json!(["error", error, call[2]]),
    };
    let body = json!({"methodResponses": [response], "sessionState": "s1"});
    respond("200 OK", "application/json", body.to_string().as_bytes());
}

/// The answer to one method call, or the method error.
fn answer(state: &State, name: &str, arguments: &Value) -> Result<Value, Value> {
    let visible = |email: &&Value| {
        email["id"]
            .as_str()
            .is_some_and(|id| !state.hidden.contains(id))
    };
    match name {
        "Mailbox/get" => {
            let mut answer = state.mailboxes.clone();
            // The WebSocket heartbeat aside, Zmail asks for every mailbox.
            if arguments["ids"].as_array().is_some_and(Vec::is_empty) {
                answer["list"] = json!([]);
            }
            Ok(answer)
        }
        "Email/query" => {
            let position = arguments["position"].as_u64().unwrap_or(0) as usize;
            let limit = arguments["limit"].as_u64().unwrap_or(u64::MAX) as usize;
            let ids = state.full["list"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(visible)
                .map(|email| email["id"].clone())
                .skip(position)
                .take(limit)
                .collect::<Vec<_>>();
            // The query's state follows what the server holds.
            let query_state = format!("q-{}", state.hidden.len());
            Ok(json!({"accountId": "a1", "queryState": query_state,
                      "canCalculateChanges": false, "position": position, "ids": ids}))
        }
        "Email/get" => {
            let ids = arguments["ids"].as_array().cloned().unwrap_or_default();
            let full = wants_bodies(arguments);
            if full && ids.len() > FULL_GET_LIMIT {
                return Err(json!({"type": "requestTooLarge",
                    "description": "Too many items in an email read at level FULL"}));
            }
            let properties = arguments["properties"].as_array().cloned();
            let light = properties.as_ref().is_some_and(|properties| {
                properties.len() == LIGHT_PROPERTIES.len()
                    && LIGHT_PROPERTIES
                        .iter()
                        .all(|light| properties.iter().any(|property| property == light))
            });
            let source = match &state.light {
                Some(recorded) if light => recorded,
                _ => &state.full,
            };
            let list = source["list"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(visible)
                .filter(|email| ids.contains(&email["id"]))
                .map(|email| match &properties {
                    // The recorded answers are replayed as they are; any
                    // other request gets the properties it asked for.
                    Some(properties) if !full && !light => Value::Object(
                        email
                            .as_object()
                            .into_iter()
                            .flatten()
                            .filter(|(name, _)| {
                                *name == "id" || properties.iter().any(|wanted| wanted == *name)
                            })
                            .map(|(name, value)| (name.clone(), value.clone()))
                            .collect(),
                    ),
                    _ => email.clone(),
                })
                .collect::<Vec<_>>();
            let not_found = ids
                .iter()
                .filter(|id| !list.iter().any(|email| email["id"] == **id))
                .cloned()
                .collect::<Vec<_>>();
            Ok(
                json!({"accountId": "a1", "state": source["state"], "list": list,
                      "notFound": not_found}),
            )
        }
        "Email/changes" => state
            .changes
            .clone()
            .ok_or_else(|| json!({"type": "cannotCalculateChanges"})),
        "Email/set" => {
            let updated = arguments["update"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(id, _)| (id.clone(), Value::Null))
                .collect::<serde_json::Map<_, _>>();
            Ok(
                json!({"accountId": "a1", "oldState": "em-1", "newState": "em-2",
                      "updated": updated}),
            )
        }
        // No submission capability: the sync goes on without identities.
        _ => Err(json!({"type": "unknownMethod"})),
    }
}

/// Compares `actual` with the pinned text `name`, or writes it when
/// `ZMAIL_UPDATE_SNAPSHOTS` is set.
fn assert_pinned(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/read_characterisation")
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

/// The servers whose recorded answers can be replayed as a sync.
fn replayable() -> Vec<String> {
    let mut servers = std::fs::read_dir(fixtures())
        .expect("the fixtures directory")
        .filter_map(|entry| {
            let path = entry.expect("an entry").path();
            (path.join("mailbox-get.all.json").exists()
                && path.join("email-get.sync.json").exists())
            .then(|| path.file_name()?.to_str().map(str::to_owned))
            .flatten()
        })
        .collect::<Vec<_>>();
    servers.sort();
    servers
}

/// A full sync of each recorded server: every mailbox, thread and
/// message as the store is given them.
#[tokio::test]
async fn a_full_sync_stores_what_it_stored_before() {
    let servers = replayable();
    assert!(
        servers.iter().any(|server| server == "shapes"),
        "{servers:?}"
    );
    for server in servers {
        let replay = Replay::start(&server);
        let sync = replay.sync(None, &[]).await.expect("the sync runs");
        assert_pinned(&format!("{server}.full.txt"), &replay.shown(&sync));

        // More emails than the server returns with their bodies in one
        // call: the batch halves until accepted, and every email arrives.
        let emails = replay.email_ids().len();
        let full = replay
            .get_sizes()
            .into_iter()
            .filter(|(_, full)| *full)
            .map(|(size, _)| size)
            .collect::<Vec<_>>();
        if emails > FULL_GET_LIMIT {
            assert!(full[0] > FULL_GET_LIMIT, "{server}: {full:?}");
        }
        let accepted = full.iter().filter(|size| **size <= FULL_GET_LIMIT);
        assert_eq!(accepted.sum::<usize>(), emails, "{server}: {full:?}");

        // Unchanged states: the mirror is returned as it is.
        let again = replay
            .sync(
                Some(sync.snapshot.checkpoint.clone()),
                &sync.snapshot.mailboxes,
            )
            .await
            .expect("the second sync runs");
        assert_eq!(
            again.snapshot.mailboxes, sync.snapshot.mailboxes,
            "{server}"
        );
        assert_eq!(
            again.snapshot.detail,
            "JMAP mailbox state unchanged for Recorded"
        );
    }
}

/// An incremental sync of each recorded server: the first emails arrive
/// later, the next is destroyed, and the ones in the light answer
/// changed their keywords or mailboxes.
#[tokio::test]
async fn an_incremental_sync_stores_what_it_stored_before() {
    for server in replayable() {
        let replay = Replay::start(&server);
        let ids = replay.email_ids();
        if ids.len() < 3 {
            continue;
        }
        // Of a larger set, the second late email stays unreported.
        let late_count = if ids.len() > 3 { 2 } else { 1 };
        let (late, destroyed) = (&ids[..late_count], &ids[late_count]);
        replay.with_state(|state| state.hidden.extend(late.iter().cloned()));
        let first = replay.sync(None, &[]).await.expect("the first sync runs");

        let updated = replay.with_state(|state| {
            state.hidden.clear();
            state.hidden.insert(destroyed.clone());
            let updated = state
                .light
                .as_ref()
                .map(|light| ids_of(&light["list"]))
                .unwrap_or_default()
                .into_iter()
                .filter(|id| id != destroyed)
                .collect::<Vec<_>>();
            state.changes = Some(json!({
                "accountId": "a1", "oldState": state.full["state"], "newState": "em-next",
                "hasMoreChanges": false, "created": [late[0]], "updated": updated,
                "destroyed": [destroyed]
            }));
            updated
        });
        let second = replay
            .sync(
                Some(first.snapshot.checkpoint.clone()),
                &first.snapshot.mailboxes,
            )
            .await
            .expect("the incremental sync runs");
        assert_pinned(&format!("{server}.incremental.txt"), &replay.shown(&second));
        assert!(
            second
                .snapshot
                .detail
                .starts_with("JMAP incremental sync applied"),
            "{server}: {}",
            second.snapshot.detail
        );
        assert!(!updated.is_empty() || replay.with_state(|state| state.light.is_none()));

        // A server that cannot say what changed: the full sync runs.
        replay.with_state(|state| {
            state.changes = None;
            state.hidden.clear();
        });
        let fallback = replay
            .sync(
                Some(first.snapshot.checkpoint.clone()),
                &first.snapshot.mailboxes,
            )
            .await
            .expect("the fallback sync runs");
        assert!(
            fallback
                .snapshot
                .detail
                .starts_with("JMAP sync complete against"),
            "{server}: {}",
            fallback.snapshot.detail
        );
    }
}

/// The other reads: reply headers for emails stored without them, a
/// server search, and the mailbox a move resolves by name.
#[tokio::test]
async fn headers_search_and_moves_read_what_they_read_before() {
    for server in replayable() {
        let replay = Replay::start(&server);
        let target = replay.target(None);
        let mut ids = replay.email_ids();
        ids.push("not-an-email".to_owned());

        let mut text = String::new();
        let headers = fetch_message_headers(&target, "pw", &ids)
            .await
            .expect("the headers are fetched");
        writeln!(text, "headers: {headers:#?}").expect("writes");

        let now = zmail_core::time::clock_at("2026-11-10T12:00:00Z").expect("a time");
        let query = zmail_core::search::SearchQuery::parse("report from:cy -is:unread");
        let hits = search_emails(&target, "pw", &query, &now, 3)
            .await
            .expect("the search runs");
        writeln!(text, "hits: {hits:#?}").expect("writes");
        let nowhere = zmail_core::search::SearchQuery::parse("report in:nowhere");
        assert!(
            search_emails(&target, "pw", &nowhere, &now, 3)
                .await
                .expect("the search runs")
                .is_empty()
        );

        // A move names its mailbox as the mirror does; the id comes from
        // `Mailbox/get`.
        let first_mailbox = replay.with_state(|state| state.mailboxes["list"][0].clone());
        let ops = [
            PendingOp {
                id: 1,
                account_id: 1,
                kind: PendingOpKind::Move,
                remote_id: ids[0].clone(),
                source_mailbox: String::new(),
                target_mailbox: first_mailbox["name"].as_str().map(str::to_owned),
                attempts: 0,
                last_error: None,
            },
            PendingOp {
                id: 2,
                account_id: 1,
                kind: PendingOpKind::Move,
                remote_id: ids[0].clone(),
                source_mailbox: String::new(),
                target_mailbox: Some("No such mailbox".to_owned()),
                attempts: 0,
                last_error: None,
            },
        ];
        let report = apply_pending_ops(&target, "pw", &ops)
            .await
            .expect("the operations run");
        writeln!(text, "moves: {report:#?}").expect("writes");
        let moved = replay.with_state(|state| {
            state
                .calls
                .iter()
                .find(|call| call[0] == "Email/set")
                .map(|call| call[1]["update"].clone())
        });
        writeln!(text, "moved to: {}", moved.unwrap_or_default()).expect("writes");
        assert_pinned(&format!("{server}.other.txt"), &text);
    }
}

/// What users are told when the server refuses a read or cannot be
/// read.
#[tokio::test]
async fn refused_reads_say_what_they_said_before() {
    let faults: [(&str, Fault); 9] = [
        (
            "Mailbox/get accountNotFound",
            Fault::Method("Mailbox/get", json!({"type": "accountNotFound"})),
        ),
        (
            "Email/get serverFail",
            Fault::Method(
                "Email/get",
                json!({"type": "serverFail", "description": "Something went wrong"}),
            ),
        ),
        (
            "Email/query invalidArguments",
            Fault::Method(
                "Email/query",
                json!({"type": "invalidArguments", "description": "bad sort"}),
            ),
        ),
        (
            "Email/query unsupportedFilter",
            Fault::Method("Email/query", json!({"type": "unsupportedFilter"})),
        ),
        (
            "Email/get an error type of a later RFC",
            Fault::Method("Email/get", json!({"type": "rateLimited"})),
        ),
        (
            "problem details: limit",
            Fault::Http(
                400,
                "application/problem+json",
                r#"{"type":"urn:ietf:params:jmap:error:limit","status":400,"limit":"maxCallsInRequest","detail":"Too many calls"}"#,
            ),
        ),
        (
            "problem details: unknown capability",
            Fault::Http(
                400,
                "application/problem+json",
                r#"{"type":"urn:ietf:params:jmap:error:unknownCapability","status":400,"detail":"The request used unsupported capabilities"}"#,
            ),
        ),
        ("HTTP 503", Fault::Http(503, "text/plain", "try later")),
        (
            "not JSON",
            Fault::Http(200, "text/html", "<html>sign in</html>"),
        ),
    ];
    let mut text = String::new();
    for (name, fault) in faults {
        let replay = Replay::start("shapes");
        replay.with_state(|state| state.fault = Some(fault));
        let target = replay.target(None);
        let sync = replay.sync(None, &[]).await;
        let now = zmail_core::time::clock_at("2026-11-10T12:00:00Z").expect("a time");
        let query = zmail_core::search::SearchQuery::parse("report");
        let search = search_emails(&target, "pw", &query, &now, 3).await;
        let headers = fetch_message_headers(&target, "pw", &["e1".to_owned()]).await;
        let shown = |error: Option<anyhow::Error>| {
            error.map_or("ok".to_owned(), |error| {
                format!("{error:#}").replace(&replay.endpoint, "http://server")
            })
        };
        writeln!(text, "{name}").expect("writes");
        writeln!(text, "  sync:    {}", shown(sync.err())).expect("writes");
        writeln!(text, "  search:  {}", shown(search.err())).expect("writes");
        writeln!(text, "  headers: {}", shown(headers.err())).expect("writes");
    }
    assert_pinned("refusals.txt", &text);
}
