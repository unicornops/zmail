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
