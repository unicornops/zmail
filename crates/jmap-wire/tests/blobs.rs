//! Uploading and downloading blobs (RFC 8620 section 6) over a
//! transport that answers from memory: what is asked of the transport
//! (URL, headers, limits, timeout), where the credentials go, and how
//! each answer comes back. `tests/transport.rs` does the same over
//! sockets with the default transport. This file runs with
//! `--no-default-features`.

use std::{
    future::Future,
    sync::Mutex,
    task::{Context, Poll, Waker},
    time::Duration,
};

use jmap_wire::{
    BlobDownload, Client, ClientBuilder, Credentials, DEFAULT_BLOB_TIMEOUT,
    DEFAULT_MAX_DOWNLOAD_BYTES, Error, HttpMethod, HttpRequest, HttpResponse, HttpTransport,
    RedirectPolicy, RedirectRefusal, TransportError, TransportErrorKind,
};
use serde_json::{Value, json};

/// Answers each request with what a function returns, and keeps the
/// requests.
struct Programmed<F>(F, Mutex<Vec<HttpRequest>>);

impl<F> Programmed<F>
where
    F: Fn(&HttpRequest) -> Result<HttpResponse, TransportError> + Send + Sync,
{
    fn new(answer: F) -> Self {
        Self(answer, Mutex::default())
    }

    fn sent(&self) -> Vec<HttpRequest> {
        self.1.lock().expect("the log").clone()
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

/// A session on `api.example.com` whose blobs are on another host, as
/// providers with a separate domain for user content have them. The
/// download template is the one of RFC 8620 section 2.1.
fn session() -> Value {
    json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": {"maxSizeUpload": 1000},
            "urn:ietf:params:jmap:mail": {}
        },
        "accounts": {"a 1": {"name": "ada@example.com"}},
        "primaryAccounts": {"urn:ietf:params:jmap:mail": "a 1"},
        "apiUrl": "https://api.example.com/jmap/api",
        "uploadUrl": "https://content.example.net/upload/{accountId}/",
        "downloadUrl":
            "https://content.example.net/download/{accountId}/{blobId}/{name}?accept={type}",
        "state": "s1"
    })
}

fn client<T: HttpTransport>(builder: ClientBuilder, transport: T) -> Client<T> {
    builder
        .credentials(Credentials::bearer("a-token"))
        .with_session(
            transport,
            "https://api.example.com/jmap/session"
                .parse()
                .expect("a URL"),
            serde_json::from_value(session()).expect("a session"),
        )
        .expect("a client")
}

fn answer(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> HttpResponse {
    HttpResponse::new(
        status,
        vec![("Content-Type".to_owned(), content_type.to_owned())],
        body.into(),
    )
}

fn redirect(status: u16, location: &str) -> HttpResponse {
    HttpResponse::new(
        status,
        vec![("Location".to_owned(), location.to_owned())],
        Vec::new(),
    )
}

fn header<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(header, _)| header == name)
        .map(|(_, value)| value.as_str())
}

/// RFC 8620 section 6.1: an authenticated POST of the bytes to the
/// upload URL of the account, with the `Content-Type` given; the answer
/// has the blob id.
#[test]
fn an_upload_posts_the_bytes_with_their_type_and_reads_the_blob_id() {
    let transport = Programmed::new(|request| {
        Ok(answer(
            201,
            "application/json",
            json!({"accountId": "a 1", "blobId": "G4c6751", "type": "message/rfc822",
                   "size": request.body.len()})
            .to_string(),
        ))
    });
    let client = client(Client::builder(), &transport);
    let blob = now(client.upload("a 1", "message/rfc822", b"Subject: Hi\r\n\r\nHi".to_vec()))
        .expect("uploaded");
    assert_eq!(blob.blob_id, "G4c6751");
    assert_eq!(blob.content_type.as_deref(), Some("message/rfc822"));
    assert_eq!(blob.size, Some(17));
    assert_eq!(blob.account_id.as_deref(), Some("a 1"));

    let sent = transport.sent();
    assert_eq!(sent.len(), 1);
    let request = &sent[0];
    assert_eq!(request.method, HttpMethod::Post);
    assert_eq!(
        request.url.as_str(),
        "https://content.example.net/upload/a%201/",
        "the account id fills the template, percent-encoded"
    );
    assert_eq!(request.body, b"Subject: Hi\r\n\r\nHi");
    assert_eq!(header(request, "content-type"), Some("message/rfc822"));
    assert_eq!(header(request, "accept"), Some("application/json"));
    assert_eq!(
        header(request, "authorization"),
        Some("Bearer a-token"),
        "the upload URL is the session's: an authenticated POST, whatever its host"
    );
    assert_eq!(request.timeout, DEFAULT_BLOB_TIMEOUT, "not the API timeout");
    assert!(request.max_response_bytes <= 1024 * 1024);

    // No type: the type for bytes of no known type.
    now(client.upload("a 1", "", vec![1, 2, 3])).expect("uploaded");
    assert_eq!(
        header(&transport.sent()[1], "content-type"),
        Some("application/octet-stream")
    );
}

/// The session's `maxSizeUpload` is checked before anything is sent.
#[test]
fn an_upload_at_the_limit_is_sent_and_one_over_it_is_refused_unsent() {
    let transport = Programmed::new(|_| Ok(answer(200, "application/json", r#"{"blobId":"b"}"#)));
    let client = client(Client::builder(), &transport);
    now(client.upload("a 1", "text/plain", vec![0; 1000])).expect("at the limit");
    match now(client.upload("a 1", "text/plain", vec![0; 1001])) {
        Err(Error::UploadTooLarge { size, limit }) => assert_eq!((size, limit), (1001, 1000)),
        other => panic!("expected the limit, got {other:?}"),
    }
    assert_eq!(transport.sent().len(), 1);
}

/// RFC 8620 section 6.1: an HTTP error should carry problem details;
/// and the answers that are not what the RFC describes.
#[test]
fn a_refused_upload_says_why() {
    let upload = |response: HttpResponse| {
        let transport = Programmed::new(move |_| Ok(response.clone()));
        now(client(Client::builder(), &transport).upload("a 1", "text/plain", vec![1]))
            .expect_err("refused")
    };

    let error = upload(answer(
        413,
        "application/problem+json",
        r#"{"type":"urn:ietf:params:jmap:error:limit","status":413,"limit":"maxSizeUpload"}"#,
    ));
    match &error {
        Error::Problem(problem) => assert_eq!(problem.limit.as_deref(), Some("maxSizeUpload")),
        other => panic!("expected problem details, got {other:?}"),
    }
    assert_eq!(error.http_status(), Some(413));

    let error = upload(answer(507, "text/plain", "full"));
    assert!(matches!(error, Error::Http { status: 507 }), "{error:?}");
    assert_eq!(error.http_status(), Some(507));

    let error = upload(answer(200, "text/html", "<html>sign in</html>"));
    assert!(
        matches!(&error, Error::Json { what, .. } if what == "upload answer"),
        "{error:?}"
    );
    // An answer without a blob id is of no use.
    let error = upload(answer(200, "application/json", r#"{"accountId":"a 1"}"#));
    assert!(matches!(error, Error::Json { .. }), "{error:?}");
}

/// RFC 8620 section 6.2: an authenticated GET of the download URL with
/// its four variables filled in.
#[test]
fn a_download_fills_the_template_and_returns_the_bytes() {
    let transport = Programmed::new(|_| Ok(answer(200, "application/pdf", b"%PDF-1.7".to_vec())));
    let client = client(Client::builder(), &transport);
    let download = BlobDownload::new("a 1", "G4c6751/x")
        .name("my report/1.pdf")
        .content_type("application/pdf");
    assert_eq!(
        client.download_url(&download).expect("a URL").as_str(),
        "https://content.example.net/download/a%201/G4c6751%2Fx/my%20report%2F1.pdf\
         ?accept=application%2Fpdf"
    );
    let blob = now(client.download(&download)).expect("downloaded");
    assert_eq!(blob.bytes, b"%PDF-1.7");
    assert_eq!(blob.content_type.as_deref(), Some("application/pdf"));

    let sent = transport.sent();
    let request = &sent[0];
    assert_eq!(request.method, HttpMethod::Get);
    assert_eq!(request.url, client.download_url(&download).expect("a URL"));
    assert!(request.body.is_empty());
    assert_eq!(header(request, "authorization"), Some("Bearer a-token"));
    assert_eq!(header(request, "accept"), Some("application/pdf"));
    assert_eq!(header(request, "content-type"), None);
    assert_eq!(
        header(request, "accept-encoding"),
        None,
        "blobs are not asked for compressed"
    );
    assert_eq!(request.timeout, DEFAULT_BLOB_TIMEOUT);
    assert_eq!(request.max_response_bytes, DEFAULT_MAX_DOWNLOAD_BYTES);

    // The defaults: any type, and a name, as the template needs both.
    now(client.download(&BlobDownload::new("a 1", "b2"))).expect("downloaded");
    let sent = transport.sent();
    assert_eq!(
        sent[1].url.as_str(),
        "https://content.example.net/download/a%201/b2/blob?accept=application%2Foctet-stream"
    );
    assert_eq!(header(&sent[1], "accept"), Some("application/octet-stream"));
}

/// The limits of a download are its own: neither the API's response
/// limit nor its timeout.
#[test]
fn a_download_has_its_own_size_limit_and_timeout() {
    let transport = Programmed::new(|request| {
        // What a transport does with a body over the request's limit.
        if request.max_response_bytes < 4096 {
            Err(TransportError::too_large(request.max_response_bytes))
        } else {
            Ok(answer(200, "application/octet-stream", vec![7; 4096]))
        }
    });
    let client = client(
        Client::builder()
            .timeout(Duration::from_secs(3))
            .max_response_bytes(16)
            .blob_timeout(Duration::from_secs(90))
            .max_download_bytes(8192),
        &transport,
    );
    let blob = now(client.download(&BlobDownload::new("a 1", "b"))).expect("under its own limit");
    assert_eq!(blob.bytes.len(), 4096);
    let sent = transport.sent();
    assert_eq!(sent[0].max_response_bytes, 8192);
    assert_eq!(sent[0].timeout, Duration::from_secs(90));

    // One download can set both for itself.
    let error = now(client.download(
        &BlobDownload::new("a 1", "b")
            .max_bytes(100)
            .timeout(Duration::from_secs(5)),
    ))
    .expect_err("over its limit");
    assert!(error.is_response_too_large(), "{error:?}");
    let sent = transport.sent();
    assert_eq!(sent[1].max_response_bytes, 100);
    assert_eq!(sent[1].timeout, Duration::from_secs(5));
}

/// RFC 8620 section 6.2: an HTTP error should carry problem details.
#[test]
fn a_blob_the_server_does_not_have_or_will_not_give_is_an_http_error() {
    let download = |response: HttpResponse| {
        let transport = Programmed::new(move |_| Ok(response.clone()));
        now(client(Client::builder(), &transport).download(&BlobDownload::new("a 1", "gone")))
            .expect_err("refused")
    };
    let error = download(answer(
        404,
        "application/problem+json",
        r#"{"type":"about:blank","status":404,"detail":"The blob does not exist"}"#,
    ));
    assert!(matches!(error, Error::Problem(_)), "{error:?}");
    assert_eq!(error.http_status(), Some(404));
    let error = download(answer(404, "text/plain", "not found"));
    assert!(matches!(error, Error::Http { status: 404 }), "{error:?}");
    let error = download(answer(403, "text/plain", "no"));
    assert_eq!(error.http_status(), Some(403));
    // An error page is never returned as the blob.
    let error = download(answer(500, "application/octet-stream", vec![1, 2, 3]));
    assert_eq!(error.http_status(), Some(500));
}

/// The download URL is on another host than the session: it gets the
/// credentials, because the session named it. Where it redirects to is
/// for the redirect policy, judged from the download URL's host.
#[test]
fn a_download_redirect_takes_the_credentials_only_where_the_policy_allows() {
    let serve = |request: &HttpRequest| {
        Ok(match (request.url.host_str(), request.url.path()) {
            (Some("content.example.net"), path) if path.starts_with("/download/") => {
                match request.url.path().rsplit('/').nth(1) {
                    Some("same-host") => redirect(302, "/stored/1"),
                    Some("insecure") => redirect(302, "http://content.example.net/stored/1"),
                    _ => redirect(302, "https://storage.example.org/signed?sig=s3cret"),
                }
            }
            (Some("content.example.net"), "/stored/1") => {
                answer(200, "application/octet-stream", b"same host".to_vec())
            }
            (Some("storage.example.org"), "/signed") => {
                answer(200, "application/octet-stream", b"stored".to_vec())
            }
            _ => answer(404, "text/plain", "not found"),
        })
    };

    // The default policy: the same host is followed with credentials.
    let transport = Programmed::new(serve);
    let client_default = client(Client::builder(), &transport);
    let blob = now(client_default.download(&BlobDownload::new("a 1", "same-host")))
        .expect("the same host");
    assert_eq!(blob.bytes, b"same host");
    assert!(
        transport
            .sent()
            .iter()
            .all(|request| header(request, "authorization").is_some())
    );

    // Another host is refused, and never contacted.
    let transport = Programmed::new(serve);
    let client_default = client(Client::builder(), &transport);
    let error = now(client_default.download(&BlobDownload::new("a 1", "elsewhere")))
        .expect_err("another host");
    match &error {
        Error::Redirect { to, reason } => {
            assert_eq!(*reason, RedirectRefusal::Policy);
            assert_eq!(to, "https://storage.example.org/signed", "no query string");
        }
        other => panic!("expected a refused redirect, got {other:?}"),
    }
    assert_eq!(transport.sent().len(), 1);

    // The download opts into following it without credentials.
    let transport = Programmed::new(serve);
    let client_default = client(Client::builder(), &transport);
    let blob = now(client_default
        .download(&BlobDownload::new("a 1", "elsewhere").follow_redirects_without_credentials()))
    .expect("followed");
    assert_eq!(blob.bytes, b"stored");
    let sent = transport.sent();
    assert_eq!(sent.len(), 2);
    assert!(header(&sent[0], "authorization").is_some());
    assert_eq!(sent[1].url.host_str(), Some("storage.example.org"));
    assert_eq!(
        header(&sent[1], "authorization"),
        None,
        "the storage host never sees the credentials"
    );

    // The caller names the host in the policy: the credentials follow.
    let transport = Programmed::new(serve);
    let client_named = client(
        Client::builder().redirect_policy(RedirectPolicy::hosts(["storage.example.org"])),
        &transport,
    );
    now(client_named.download(&BlobDownload::new("a 1", "elsewhere"))).expect("followed");
    assert!(header(&transport.sent()[1], "authorization").is_some());

    // HTTPS to HTTP is refused whatever was opted into.
    let transport = Programmed::new(serve);
    let client_default = client(Client::builder(), &transport);
    let error = now(client_default
        .download(&BlobDownload::new("a 1", "insecure").follow_redirects_without_credentials()))
    .expect_err("insecure");
    assert!(
        matches!(
            error,
            Error::Redirect {
                reason: RedirectRefusal::Insecure,
                ..
            }
        ),
        "{error:?}"
    );
}

/// Once the credentials are left behind they stay behind, even when a
/// later redirect leads back to a host that may have them: whoever
/// controls the storage host must not be able to send them anywhere.
#[test]
fn credentials_left_behind_do_not_come_back_on_a_later_hop() {
    let transport = Programmed::new(|request| {
        Ok(match request.url.host_str() {
            Some("content.example.net") if request.url.path().starts_with("/download/") => {
                redirect(307, "https://storage.example.org/hop")
            }
            Some("storage.example.org") => redirect(307, "https://content.example.net/final"),
            _ => answer(200, "application/octet-stream", b"back".to_vec()),
        })
    });
    let client = client(Client::builder(), &transport);
    now(client.download(&BlobDownload::new("a 1", "b").follow_redirects_without_credentials()))
        .expect("followed");
    let sent = transport.sent();
    assert_eq!(sent.len(), 3);
    assert!(header(&sent[0], "authorization").is_some());
    assert_eq!(header(&sent[1], "authorization"), None);
    assert_eq!(sent[2].url.host_str(), Some("content.example.net"));
    assert_eq!(header(&sent[2], "authorization"), None);
}

/// An upload is a POST: a redirect that keeps the method is followed
/// where the policy allows, with the body again; any other is refused.
#[test]
fn an_upload_redirect_keeps_the_post_or_is_refused() {
    let transport = Programmed::new(|request| {
        Ok(match request.url.path() {
            "/upload/a%201/" => redirect(307, "/upload-v2/"),
            "/upload-v2/" => answer(200, "application/json", r#"{"blobId":"b"}"#),
            _ => answer(404, "text/plain", ""),
        })
    });
    let client = client(Client::builder(), &transport);
    now(client.upload("a 1", "text/plain", b"abc".to_vec())).expect("followed");
    let sent = transport.sent();
    assert_eq!(
        (sent[1].method, sent[1].body.as_slice()),
        (HttpMethod::Post, b"abc".as_slice())
    );
    assert_eq!(header(&sent[1], "content-type"), Some("text/plain"));

    for (status, location, reason) in [
        (302, "/upload-v2/", RedirectRefusal::MethodChange),
        (
            307,
            "https://storage.example.org/upload",
            RedirectRefusal::Policy,
        ),
    ] {
        let transport = Programmed::new(move |_| Ok(redirect(status, location)));
        let refused = self::client(Client::builder(), &transport);
        let error = now(refused.upload("a 1", "text/plain", b"abc".to_vec())).expect_err("refused");
        assert!(
            matches!(error, Error::Redirect { reason: why, .. } if why == reason),
            "{error:?}"
        );
        assert_eq!(transport.sent().len(), 1, "the bytes go nowhere else");
    }
}

/// A transport failure of a blob is the transport's error, with its
/// kind.
#[test]
fn a_blob_exchange_that_fails_says_how() {
    let transport = Programmed::new(|_| {
        Err(TransportError::new(
            TransportErrorKind::Timeout,
            "timed out",
        ))
    });
    let client = client(Client::builder(), &transport);
    for error in [
        now(client.download(&BlobDownload::new("a 1", "b"))).expect_err("fails"),
        now(client.upload("a 1", "text/plain", vec![1])).expect_err("fails"),
    ] {
        assert!(
            matches!(&error, Error::Transport(error) if error.kind() == TransportErrorKind::Timeout),
            "{error:?}"
        );
    }
}

/// An answer in a content encoding the client cannot undo is not the
/// blob.
#[test]
fn a_download_in_an_encoding_the_client_cannot_undo_is_refused() {
    let transport = Programmed::new(|_| {
        Ok(HttpResponse::new(
            200,
            vec![("Content-Encoding".to_owned(), "br".to_owned())],
            vec![1, 2, 3],
        ))
    });
    let client = client(Client::builder(), &transport);
    let error = now(client.download(&BlobDownload::new("a 1", "b"))).expect_err("refused");
    assert!(matches!(error, Error::Transport(_)), "{error:?}");

    // `identity` is no encoding.
    let transport = Programmed::new(|_| {
        Ok(HttpResponse::new(
            200,
            vec![("Content-Encoding".to_owned(), "identity".to_owned())],
            vec![1, 2, 3],
        ))
    });
    let client = self::client(Client::builder(), &transport);
    assert_eq!(
        now(client.download(&BlobDownload::new("a 1", "b")))
            .expect("as it is")
            .bytes,
        [1, 2, 3]
    );
}

#[cfg(not(feature = "gzip"))]
#[test]
fn without_the_gzip_feature_a_gzipped_download_is_refused() {
    let transport = Programmed::new(|_| {
        Ok(HttpResponse::new(
            200,
            vec![("Content-Encoding".to_owned(), "gzip".to_owned())],
            vec![0x1f, 0x8b, 8, 0],
        ))
    });
    let client = client(Client::builder(), &transport);
    now(client.download(&BlobDownload::new("a 1", "b"))).expect_err("not the blob");
}

#[cfg(feature = "gzip")]
mod gzip {
    use std::io::Write as _;

    use super::*;

    fn gzipped(body: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(body).expect("compresses");
        encoder.finish().expect("compresses")
    }

    fn compressed(body: &[u8]) -> HttpResponse {
        HttpResponse::new(
            200,
            vec![
                ("Content-Type".to_owned(), "text/plain".to_owned()),
                ("Content-Encoding".to_owned(), "gzip".to_owned()),
            ],
            gzipped(body),
        )
    }

    /// A server that compresses a download unasked: the blob is what it
    /// expands to, and the limit counts that.
    #[test]
    fn a_gzipped_download_is_expanded_within_the_downloads_limit() {
        let megabyte = vec![b'x'; 1024 * 1024];
        let small = gzipped(&megabyte).len();
        assert!(small < 4096, "{small} bytes");

        let body = megabyte.clone();
        let transport = Programmed::new(move |_| Ok(compressed(&body)));
        let client = client(Client::builder(), &transport);
        let blob = now(client.download(&BlobDownload::new("a 1", "b"))).expect("expanded");
        assert_eq!(blob.bytes.len(), megabyte.len());

        // About a kilobyte on the wire, a megabyte expanded: refused at
        // the limit, however small it arrived.
        let error = now(client.download(&BlobDownload::new("a 1", "b").max_bytes(64 * 1024)))
            .expect_err("over the limit once expanded");
        assert!(error.is_response_too_large(), "{error:?}");
        assert_eq!(transport.sent()[1].max_response_bytes, 64 * 1024);
    }

    /// The same for the answer to an upload, which is asked for
    /// compressed like any JSON.
    #[test]
    fn a_gzipped_upload_answer_is_read() {
        let transport = Programmed::new(|_| {
            let mut response = compressed(br#"{"blobId":"b","size":3}"#);
            response.status = 201;
            Ok(response)
        });
        let client = client(Client::builder(), &transport);
        let blob = now(client.upload("a 1", "text/plain", b"abc".to_vec())).expect("uploaded");
        assert_eq!(blob.blob_id, "b");
        assert_eq!(
            header(&transport.sent()[0], "accept-encoding"),
            Some("gzip")
        );
    }
}
