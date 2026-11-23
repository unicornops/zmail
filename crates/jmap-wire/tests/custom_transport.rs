//! The client over a transport that is not `reqwest`: answers from
//! memory, with no network and no async runtime. This is the test that
//! runs with `--no-default-features`.

use std::{
    collections::VecDeque,
    future::Future,
    sync::Mutex,
    task::{Context, Poll, Waker},
};

use jmap_wire::{
    Client, Credentials, Error, Get, HttpMethod, HttpRequest, HttpResponse, HttpTransport,
    MethodErrorType, RedirectPolicy, RedirectRefusal, Request, TransportError, capability,
    mail::Identity,
};
use serde_json::{Value, json};

/// Answers requests from a queue and keeps what it was sent.
#[derive(Default)]
struct Scripted {
    answers: Mutex<VecDeque<HttpResponse>>,
    sent: Mutex<Vec<HttpRequest>>,
}

impl Scripted {
    fn answering(answers: impl IntoIterator<Item = HttpResponse>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            sent: Mutex::default(),
        }
    }

    fn sent(&self) -> Vec<HttpRequest> {
        self.sent.lock().expect("the log").clone()
    }
}

impl HttpTransport for Scripted {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.sent.lock().expect("the log").push(request);
        self.answers
            .lock()
            .expect("the queue")
            .pop_front()
            .ok_or_else(|| {
                TransportError::new(jmap_wire::TransportErrorKind::Other, "no answer scripted")
            })
    }
}

/// Runs a future whose transport never waits.
fn now<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("the future waited"),
    }
}

fn json_answer(status: u16, body: Value) -> HttpResponse {
    HttpResponse::new(
        status,
        vec![("content-type".to_owned(), "application/json".to_owned())],
        body.to_string().into_bytes(),
    )
}

fn redirect(status: u16, location: &str) -> HttpResponse {
    HttpResponse::new(
        status,
        vec![("Location".to_owned(), location.to_owned())],
        Vec::new(),
    )
}

fn session() -> Value {
    json!({
        "capabilities": {"urn:ietf:params:jmap:core": {}, "urn:ietf:params:jmap:submission": {}},
        "accounts": {"a1": {"name": "ada@example.com"}},
        "primaryAccounts": {"urn:ietf:params:jmap:submission": "a1"},
        "apiUrl": "/jmap/api",
        "state": "s1"
    })
}

#[test]
fn a_client_runs_over_any_transport() {
    let transport = Scripted::answering([
        json_answer(200, session()),
        json_answer(
            200,
            json!({
                "methodResponses": [["Identity/get", {
                    "accountId": "a1", "state": "i1",
                    "list": [{"id": "id1", "email": "ada@example.com"}]
                }, "c0"]],
                "sessionState": "s1"
            }),
        ),
        json_answer(
            200,
            json!({
                "methodResponses": [["error", {"type": "accountNotFound"}, "c0"]],
                "sessionState": "s1"
            }),
        ),
    ]);
    let client = now(Client::builder()
        .credentials(Credentials::bearer("a-token"))
        .connect_with(
            &transport,
            "https://example.com/jmap/session".parse().expect("a URL"),
        ))
    .expect("the session loads");
    // A relative API URL resolves against the session's.
    assert_eq!(client.api_url().as_str(), "https://example.com/jmap/api");
    let account = client
        .primary_account(capability::SUBMISSION)
        .expect("an account")
        .to_owned();

    let identities = now(client.call(Get::<Identity>::new(&account))).expect("answered");
    assert_eq!(identities.list[0].email.as_deref(), Some("ada@example.com"));

    // A method-level error is the typed call's error.
    match now(client.call(Get::<Identity>::new("nobody"))) {
        Err(Error::Method(error)) => {
            assert_eq!(error.error_type, MethodErrorType::AccountNotFound)
        }
        other => panic!("expected a method error, got {other:?}"),
    }

    let sent = transport.sent();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].method, HttpMethod::Get);
    assert_eq!(sent[1].method, HttpMethod::Post);
    assert!(sent.iter().all(|request| {
        request
            .headers
            .contains(&("authorization".to_owned(), "Bearer a-token".to_owned()))
    }));
    assert!(sent[1].max_response_bytes > sent[0].max_response_bytes);
    let body: Value = serde_json::from_slice(&sent[1].body).expect("JSON");
    assert_eq!(
        body["using"],
        json!([
            "urn:ietf:params:jmap:core",
            "urn:ietf:params:jmap:submission"
        ])
    );
}

#[test]
fn the_rules_that_always_hold_need_no_cooperation_from_the_transport() {
    let connect = |answers: Vec<HttpResponse>, policy: RedirectPolicy| {
        let transport = Scripted::answering(answers);
        let result = now(Client::builder()
            .credentials(Credentials::basic("ada", "pw"))
            .redirect_policy(policy)
            .connect_with(
                &transport,
                "https://example.com/.well-known/jmap"
                    .parse()
                    .expect("a URL"),
            ))
        .map(|client| client.session_url().to_string());
        (result, transport.sent().len())
    };
    let anywhere = || RedirectPolicy::custom(|_| true);

    // HTTPS to HTTP is refused whatever the policy says, before the
    // credentials go anywhere.
    let (result, requests) = connect(
        vec![redirect(301, "http://example.com/jmap/session?t=s3cret")],
        anywhere(),
    );
    assert!(
        matches!(&result, Err(Error::Redirect { to, reason: RedirectRefusal::Insecure })
            if to == "http://example.com/jmap/session"),
        "{result:?}"
    );
    assert_eq!(requests, 1);

    // A redirect without a Location, or to something that is no URL.
    let (result, _) = connect(
        vec![HttpResponse::new(302, Vec::new(), Vec::new())],
        anywhere(),
    );
    assert!(matches!(
        result,
        Err(Error::Redirect {
            reason: RedirectRefusal::BadLocation,
            ..
        })
    ));

    // A policy that allows another host is followed there.
    let (result, requests) = connect(
        vec![
            redirect(308, "https://api.example.com/jmap/session"),
            json_answer(200, session()),
        ],
        anywhere(),
    );
    assert_eq!(
        result.expect("followed"),
        "https://api.example.com/jmap/session"
    );
    assert_eq!(requests, 2);

    // A 304 or a 300 is not a redirect to follow: it is the answer.
    let (result, _) = connect(vec![redirect(300, "/other")], anywhere());
    assert!(matches!(result, Err(Error::Http { status: 300 })));
}

#[test]
fn a_request_can_be_built_and_read_with_no_client_at_all() {
    let mut request = Request::new();
    let call = request
        .call(Get::<Identity>::new("a1").ids(["id1"]))
        .expect("encodes");
    let wire = serde_json::to_string(&request).expect("encodes");
    assert_eq!(
        wire,
        r#"{"using":["urn:ietf:params:jmap:core","urn:ietf:params:jmap:submission"],"methodCalls":[["Identity/get",{"accountId":"a1","ids":["id1"]},"c0"]]}"#
    );
    let response: jmap_wire::Response = serde_json::from_str(
        r#"{"methodResponses":[["Identity/get",{"accountId":"a1","state":"1","list":[],"notFound":["id1"]},"c0"]],"sessionState":"s"}"#,
    )
    .expect("decodes");
    assert_eq!(response.get(&call).expect("answered").not_found, ["id1"]);
}

/// Answers each request with what a function makes of it.
struct Programmed<F>(F, Mutex<Vec<HttpRequest>>);

impl<F> Programmed<F>
where
    F: Fn(&HttpRequest) -> Result<HttpResponse, TransportError> + Send + Sync,
{
    fn new(answer: F) -> Self {
        Self(answer, Mutex::default())
    }

    /// The JSON bodies of the API requests so far.
    fn posted(&self) -> Vec<Value> {
        self.1
            .lock()
            .expect("the log")
            .iter()
            .filter(|request| request.method == HttpMethod::Post)
            .map(|request| serde_json::from_slice(&request.body).expect("JSON"))
            .collect()
    }
}

impl<F> HttpTransport for Programmed<F>
where
    F: Fn(&HttpRequest) -> Result<HttpResponse, TransportError> + Send + Sync,
{
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let answer = (self.0)(&request);
        self.1.lock().expect("the log").push(request);
        answer
    }
}

fn mail_session() -> Value {
    json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": {"maxObjectsInGet": 500, "maxCallsInRequest": 16,
                                          "maxSizeRequest": 10_000_000},
            "urn:ietf:params:jmap:mail": {}
        },
        "accounts": {"a1": {"name": "ada@example.com"}},
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "a1"},
        "apiUrl": "https://example.com/jmap/api",
        "state": "s1"
    })
}

fn mail_client<T: HttpTransport>(transport: T) -> Client<T> {
    Client::builder()
        .credentials(Credentials::basic("ada", "pw"))
        .with_session(
            transport,
            "https://example.com/jmap/session".parse().expect("a URL"),
            serde_json::from_value(mail_session()).expect("a session"),
        )
        .expect("a client")
}

/// A server that, like Apache James, returns at most five emails with
/// their bodies per call while advertising 500, and whose answers to
/// more than two are over the client's size limit.
#[test]
fn a_long_list_of_ids_is_fetched_in_batches_the_server_and_the_limit_allow() {
    use jmap_wire::{IdBatches, mail::Email};

    let transport = Programmed::new(|request: &HttpRequest| {
        let body: Value = serde_json::from_slice(&request.body).expect("JSON");
        let call = &body["methodCalls"][0];
        let ids = call[1]["ids"].as_array().expect("ids").clone();
        if ids.len() > 5 {
            return Ok(json_answer(
                200,
                json!({"methodResponses": [["error", {"type": "requestTooLarge"}, call[2]]]}),
            ));
        }
        if ids.len() > 2 {
            return Err(TransportError::too_large(request.max_response_bytes));
        }
        let list = ids
            .iter()
            .filter(|id| *id != "gone")
            .map(|id| json!({"id": id, "subject": format!("Subject of {}", id.as_str().unwrap_or_default())}))
            .collect::<Vec<_>>();
        let not_found = ids.iter().filter(|id| *id == "gone").collect::<Vec<_>>();
        Ok(json_answer(
            200,
            json!({"methodResponses": [["Email/get", {
                "accountId": "a1", "state": "em-9", "list": list, "notFound": not_found
            }, call[2]]], "sessionState": "s1"}),
        ))
    });
    let client = mail_client(&transport);
    let mut ids = (0..11).map(|n| format!("e{n}")).collect::<Vec<_>>();
    ids.insert(4, "gone".to_owned());

    let mut batches = IdBatches::for_session(client.session());
    assert_eq!(batches.size(), 500);
    let answer = now(client.get_in_batches(&ids, &mut batches, |batch| {
        Get::<Email>::new("a1")
            .ids(batch)
            .properties(["subject", "bodyValues"])
            .fetch_all_body_values(true)
    }))
    .expect("every email is fetched");

    assert_eq!(answer.list.len(), 11);
    assert_eq!(answer.not_found, ["gone"]);
    assert_eq!(answer.state.as_deref(), Some("em-9"));
    let fetched = answer
        .list
        .iter()
        .map(|email| email.id.clone().expect("an id"))
        .collect::<Vec<_>>();
    assert_eq!(
        fetched,
        ids.iter()
            .filter(|id| *id != "gone")
            .cloned()
            .collect::<Vec<_>>(),
        "in the order asked for"
    );
    let sizes = transport
        .posted()
        .iter()
        .map(|body| {
            body["methodCalls"][0][1]["ids"]
                .as_array()
                .expect("ids")
                .len()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sizes,
        [12, 6, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
        "halved for the server, then for the size limit, and kept there"
    );
    assert_eq!(batches.size(), 1);

    // An error that fewer ids would not cure comes back as it is.
    let failing = Programmed::new(|_: &HttpRequest| {
        Ok(json_answer(
            200,
            json!({"methodResponses": [["error", {"type": "serverFail"}, "c0"]]}),
        ))
    });
    let client = mail_client(&failing);
    let result = now(
        client.get_in_batches(&ids, &mut IdBatches::new(500), |batch| {
            Get::<Email>::new("a1").ids(batch)
        }),
    );
    assert!(
        matches!(&result, Err(Error::Method(error)) if error.error_type == MethodErrorType::ServerFail),
        "{result:?}"
    );
    assert_eq!(failing.posted().len(), 1);
}

/// RFC 8620 section 3.7: a query and a get of what it finds in one
/// request, and references that could not resolve refused before
/// anything is sent.
#[test]
fn a_query_feeds_a_get_in_one_request_and_bad_references_are_not_sent() {
    use jmap_wire::{
        Comparator, Query,
        mail::{Email, EmailFilterCondition, Thread},
    };

    let transport = Programmed::new(|request: &HttpRequest| {
        let body: Value = serde_json::from_slice(&request.body).expect("JSON");
        assert_eq!(
            body["methodCalls"][1][1]["#ids"],
            json!({"resultOf": "c0", "name": "Email/query", "path": "/ids"})
        );
        Ok(json_answer(
            200,
            json!({"methodResponses": [
                ["Email/query", {"accountId": "a1", "queryState": "q1",
                                 "canCalculateChanges": false, "position": 0,
                                 "ids": ["e2", "e1"]}, "c0"],
                ["Email/get", {"accountId": "a1", "state": "em-1", "notFound": [], "list": [
                    {"id": "e2", "subject": "Second", "receivedAt": "2026-11-04T10:00:00Z"},
                    {"id": "e1", "subject": "First", "receivedAt": "2026-11-03T10:00:00Z"}
                ]}, "c1"]
            ], "sessionState": "s1"}),
        ))
    });
    let client = mail_client(&transport);

    let mut request = Request::new();
    let query = request
        .call(
            Query::<Email>::new("a1")
                .filter(EmailFilterCondition::new().in_mailbox("mb-inbox"))
                .sort([Comparator::new("receivedAt").descending()])
                .limit(2),
        )
        .expect("encodes");
    let get = request
        .call(
            Get::<Email>::new("a1")
                .ids_from(query.ids())
                .properties(["subject", "receivedAt"]),
        )
        .expect("encodes");
    let mut response = now(client.send(&request)).expect("answered");
    assert_eq!(response.get(&query).expect("answered").ids, ["e2", "e1"]);
    // Taken, the answer is moved out of the response, once.
    let emails = response.take(&get).expect("answered").list;
    assert_eq!(emails[0].subject.as_deref(), Some("Second"));
    assert_eq!(
        emails[1].received_at.as_ref().map(|date| date.as_str()),
        Some("2026-11-03T10:00:00Z")
    );
    assert!(matches!(response.take(&get), Err(Error::Json { .. })));

    // A reference to a call of another request: found when the call is
    // added, and nothing is sent.
    let mut other = Request::new();
    let foreign = other.call(Query::<Email>::new("a1")).expect("encodes");
    let mut request = Request::new();
    request.call(Get::<Thread>::new("a1")).expect("encodes");
    let result = request.call(Get::<Email>::new("a1").ids_from(foreign.ids()));
    assert!(
        matches!(&result, Err(Error::InvalidResultReference { argument, .. }) if argument == "#ids"),
        "{result:?}"
    );
    assert_eq!(request.method_calls().len(), 1, "the call was not added");

    // A reference by name and by hand: found before sending.
    let mut request = Request::new();
    request.call_raw(
        "Email/get",
        json!({"accountId": "a1",
               "#ids": {"resultOf": "c7", "name": "Email/query", "path": "/ids"}}),
    );
    let sent_before = transport.posted().len();
    assert!(matches!(
        now(client.send(&request)),
        Err(Error::InvalidResultReference { .. })
    ));
    let mut request = Request::new();
    let query = request.call_raw("Email/query", json!({"accountId": "a1"}));
    request.call_raw(
        "Email/get",
        json!({"accountId": "a1", "ids": [], "#ids": query.result_reference("/ids")}),
    );
    assert!(matches!(
        now(client.send(&request)),
        Err(Error::InvalidResultReference { .. })
    ));
    let mut request = Request::new();
    request.call_raw("Email/query", json!({"accountId": "a1"}));
    request.call_raw(
        "Email/get",
        json!({"accountId": "a1",
               "#ids": {"resultOf": "c0", "name": "Email/changes", "path": "/created"}}),
    );
    assert!(matches!(
        now(client.send(&request)),
        Err(Error::InvalidResultReference { .. })
    ));
    assert_eq!(transport.posted().len(), sent_before, "nothing was sent");
}

/// A transport of one's own can be handed a request of one's own.
#[test]
fn a_request_for_a_transport_can_be_built_outside_the_crate() {
    let transport = Scripted::answering([json_answer(200, json!({"ok": true}))]);
    let mut request = HttpRequest::new(
        HttpMethod::Get,
        "https://example.com/download/a1/b1".parse().expect("a URL"),
    );
    request
        .headers
        .push(("accept".to_owned(), "application/octet-stream".to_owned()));
    request.max_response_bytes = 1024;
    assert_eq!(request.timeout, jmap_wire::DEFAULT_TIMEOUT);
    let response = now(transport.execute(request)).expect("answered");
    assert_eq!(response.status, 200);
    assert_eq!(transport.sent()[0].max_response_bytes, 1024);
}

#[cfg(not(feature = "gzip"))]
#[test]
fn without_the_gzip_feature_no_compression_is_asked_for() {
    let transport = Scripted::answering([json_answer(200, session())]);
    now(Client::builder().connect_with(
        &transport,
        "https://example.com/jmap/session".parse().expect("a URL"),
    ))
    .expect("the session loads");
    assert!(
        !transport.sent()[0]
            .headers
            .iter()
            .any(|(name, _)| name == "accept-encoding")
    );
}

#[cfg(feature = "gzip")]
mod gzip {
    use std::io::Write as _;

    use jmap_wire::TransportErrorKind;

    use super::*;

    fn gzipped(body: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(body).expect("compresses");
        encoder.finish().expect("compresses")
    }

    fn gzip_answer(body: Vec<u8>) -> HttpResponse {
        HttpResponse::new(
            200,
            vec![
                ("Content-Type".to_owned(), "application/json".to_owned()),
                ("Content-Encoding".to_owned(), "gzip".to_owned()),
            ],
            body,
        )
    }

    fn echo(padding: usize) -> Vec<u8> {
        json!({
            "methodResponses": [["Core/echo", {"padding": "x".repeat(padding)}, "c0"]],
            "sessionState": "s1"
        })
        .to_string()
        .into_bytes()
    }

    fn send(
        answer: HttpResponse,
        limit: usize,
    ) -> (Result<jmap_wire::Response, Error>, Vec<HttpRequest>) {
        let transport = Scripted::answering([answer]);
        let client = Client::builder()
            .max_response_bytes(limit)
            .with_session(
                &transport,
                "https://example.com/jmap/session".parse().expect("a URL"),
                serde_json::from_value(mail_session()).expect("a session"),
            )
            .expect("a client");
        let mut request = Request::new();
        request.call_raw("Core/echo", json!({}));
        let result = now(client.send(&request));
        (result, transport.sent())
    }

    #[test]
    fn a_gzipped_answer_is_asked_for_and_read() {
        let body = echo(10_000);
        let compressed = gzipped(&body);
        assert!(compressed.len() < 500, "{} bytes", compressed.len());
        let (result, sent) = send(gzip_answer(compressed), 64 * 1024);
        let response = result.expect("decompressed and decoded");
        assert_eq!(
            response.method_responses[0].arguments["padding"]
                .as_str()
                .map(str::len),
            Some(10_000)
        );
        assert!(
            sent[0]
                .headers
                .contains(&("accept-encoding".to_owned(), "gzip".to_owned()))
        );

        // The session is fetched the same way.
        let transport =
            Scripted::answering([gzip_answer(gzipped(mail_session().to_string().as_bytes()))]);
        let client = now(Client::builder().connect_with(
            &transport,
            "https://example.com/jmap/session".parse().expect("a URL"),
        ))
        .expect("the session loads");
        assert_eq!(client.session().state, "s1");
    }

    /// The limit counts what the answer expands to: a megabyte of
    /// padding compresses to about a kilobyte, far under the limit as
    /// sent, and is refused all the same without being held.
    #[test]
    fn a_small_answer_that_expands_past_the_limit_is_refused() {
        let compressed = gzipped(&echo(1024 * 1024));
        assert!(compressed.len() < 4096, "{} bytes", compressed.len());
        let (result, _) = send(gzip_answer(compressed.clone()), 16 * 1024);
        match result {
            Err(error @ Error::Transport(_)) => {
                assert!(error.is_response_too_large(), "{error}");
                assert!(error.to_string().contains("16384"), "{error}");
            }
            other => panic!("expected a size error, got {other:?}"),
        }
        // Exactly at the limit is within it.
        let body = echo(100);
        let (result, _) = send(gzip_answer(gzipped(&body)), body.len());
        result.expect("at the limit");
        let (result, _) = send(gzip_answer(gzipped(&body)), body.len() - 1);
        assert!(result.expect_err("one byte over").is_response_too_large());
    }

    #[test]
    fn an_answer_already_decompressed_or_not_gzip_at_all_is_left_alone() {
        // A transport whose HTTP library decompressed the body and left
        // the header in place.
        let (result, _) = send(gzip_answer(echo(10)), 64 * 1024);
        result.expect("read as it is");
        // Compressed without saying so: not JSON.
        let mut unlabelled = gzip_answer(gzipped(&echo(10)));
        unlabelled.headers.pop();
        let (result, _) = send(unlabelled, 64 * 1024);
        assert!(matches!(result, Err(Error::Json { .. })));
        // Cut short: an error, not a panic.
        let mut cut = gzipped(&echo(10_000));
        cut.truncate(cut.len() / 2);
        let (result, _) = send(gzip_answer(cut), 64 * 1024);
        match result {
            Err(Error::Transport(error)) => assert_eq!(error.kind(), TransportErrorKind::Other),
            other => panic!("expected a transport error, got {other:?}"),
        }
        // An error status with a gzipped problem body is still a problem.
        let mut problem = gzip_answer(gzipped(
            br#"{"type":"urn:ietf:params:jmap:error:limit","limit":"maxSizeRequest"}"#,
        ));
        problem.status = 400;
        problem.headers[0].1 = "application/problem+json".to_owned();
        let (result, _) = send(problem, 64 * 1024);
        assert!(result.expect_err("a problem").is_request_too_large());
    }
}
