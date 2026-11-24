//! Binary data (RFC 8620 section 6): uploading a blob and downloading
//! one, over the client's transport.
//!
//! Blobs are what messages and attachments travel as. A message to
//! import is uploaded and then named by its blob id in an
//! [`EmailImport`](crate::mail::EmailImport); an attachment of an email
//! is downloaded by the `blobId` of its body part.
//!
//! # Whole bodies, with limits of their own
//!
//! Both directions hold the blob in memory whole: an upload is given as
//! bytes and a download returns bytes. That keeps the transport trait
//! to one exchange with one body ([`HttpTransport`]), and it is what a
//! mail client needs for attachments it then encrypts, parses or shows.
//! What makes it safe is that blobs have limits apart from the API's:
//!
//! - a download reads at most
//!   [`max_download_bytes`](crate::ClientBuilder::max_download_bytes),
//!   or the download's own [`max_bytes`](BlobDownload::max_bytes),
//!   counted after any decompression, and fails over that instead of
//!   holding more;
//! - an upload larger than the session's `maxSizeUpload` is refused
//!   before anything is sent ([`Error::UploadTooLarge`]);
//! - both run under the
//!   [`blob_timeout`](crate::ClientBuilder::blob_timeout), not the API
//!   timeout.
//!
//! A caller that must not hold a large download in memory (saving a
//! video to disk, say) reads it piece by piece with
//! [`Client::download_stream`], over a transport that can stream
//! ([`StreamingTransport`]), or builds the URL with
//! [`Client::download_url`] and fetches it with an HTTP client of its
//! own.
//!
//! # Where the credentials go
//!
//! The upload and download URLs are the session's, and a session may
//! put them on another host than its own (a provider's separate domain
//! for user content, say). RFC 8620 sections 6.1 and 6.2 make both
//! requests authenticated ones, so the credentials go to those URLs as
//! they go to the API URL: the session is the server's own statement of
//! where its endpoints are.
//!
//! A *redirect* from either is another matter, and is judged by the
//! client's [`RedirectPolicy`](crate::RedirectPolicy) as everywhere
//! else, against the host of the upload or download URL: with the
//! default policy the credentials never leave that host, and a redirect
//! elsewhere is refused. A download can opt into following such a
//! redirect without credentials
//! ([`BlobDownload::follow_redirects_without_credentials`]), for
//! servers that hand out a signed link to where the data is kept.

use std::{collections::BTreeMap, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::{
    client::{Client, Exchange, MAX_UPLOAD_ANSWER_BYTES, exchange, open, stream_success, success},
    error::{Error, TransportError},
    transport::{BodyStream, HttpMethod, HttpTransport, StreamingTransport},
};

/// The media type a blob is asked for, or uploaded as, when the caller
/// names none.
const OCTET_STREAM: &str = "application/octet-stream";

/// What the server says of a blob it took (RFC 8620 section 6.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct UploadedBlob {
    /// The account it was uploaded to.
    #[serde(default)]
    pub account_id: Option<String>,
    /// The blob's id, to name it by in a later call. A server may give
    /// the id of a blob with the same content that it had already.
    pub blob_id: String,
    /// The media type, as the upload's `Content-Type` gave it.
    #[serde(default, rename = "type")]
    pub content_type: Option<String>,
    /// The size in octets.
    #[serde(default, deserialize_with = "crate::record::lenient")]
    pub size: Option<u64>,
    /// Properties this crate has no field for.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// A blob to download (RFC 8620 section 6.2), for [`Client::download`].
///
/// The `name` and `content_type` fill the `{name}` and `{type}`
/// variables of the session's `downloadUrl`: they are what the server
/// is asked to call the file and to label it as, since a blob id stands
/// for bytes alone. They change the response's headers, never its
/// bytes.
///
/// ```
/// use jmap_wire::BlobDownload;
///
/// let download = BlobDownload::new("u1", "G4c6751")
///     .name("report.pdf")
///     .content_type("application/pdf")
///     // An attachment viewer that shows nothing over 25 MiB anyway.
///     .max_bytes(25 * 1024 * 1024);
/// # let _ = download;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct BlobDownload {
    account_id: String,
    blob_id: String,
    name: String,
    content_type: String,
    max_bytes: Option<usize>,
    timeout: Option<Duration>,
    redirects_without_credentials: bool,
}

impl BlobDownload {
    /// The blob `blob_id` of the account, asked for as
    /// `application/octet-stream` under the name `blob`.
    pub fn new(account_id: impl Into<String>, blob_id: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            blob_id: blob_id.into(),
            name: "blob".to_owned(),
            content_type: OCTET_STREAM.to_owned(),
            max_bytes: None,
            timeout: None,
            redirects_without_credentials: false,
        }
    }

    /// The file name the server should give the download (`{name}`).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// The media type the server should label the download as
    /// (`{type}`), which is also what the request's `Accept` asks for.
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = content_type.into();
        self
    }

    /// The most bytes to read for this download, instead of the
    /// client's
    /// [`max_download_bytes`](crate::ClientBuilder::max_download_bytes).
    /// When the size is known beforehand (a body part's `size`), a
    /// limit a little over it costs nothing and bounds what a server
    /// that sends something else can make the client hold.
    pub fn max_bytes(mut self, bytes: usize) -> Self {
        self.max_bytes = Some(bytes);
        self
    }

    /// How long this download may take, instead of the client's
    /// [`blob_timeout`](crate::ClientBuilder::blob_timeout).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Follows a redirect the client's
    /// [`RedirectPolicy`](crate::RedirectPolicy) does not allow all the
    /// same, **without the credentials** from that hop on, instead of
    /// refusing it. For servers whose download URL redirects to a
    /// signed link on a storage host. Off unless set; a redirect from
    /// HTTPS to HTTP is refused either way, and so are more redirects
    /// than the policy's limit.
    pub fn follow_redirects_without_credentials(mut self) -> Self {
        self.redirects_without_credentials = true;
        self
    }
}

/// A downloaded blob.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Blob {
    /// The blob's bytes.
    pub bytes: Vec<u8>,
    /// The `Content-Type` the server answered with: what the download
    /// asked for, when the server honours `{type}`. Do not trust it to
    /// say what the bytes are.
    pub content_type: Option<String>,
}

impl<T: HttpTransport> Client<T> {
    /// Uploads `bytes` as a blob of the account (RFC 8620 section 6.1):
    /// an authenticated POST to the session's `uploadUrl` with
    /// `content_type` as its `Content-Type`. The blob id in the answer
    /// names the data in later calls, such as an
    /// [`EmailImport`](crate::mail::EmailImport) or a body part of an
    /// email to create; use it soon, as a server may drop a blob
    /// nothing refers to.
    ///
    /// The upload runs under the
    /// [`blob_timeout`](crate::ClientBuilder::blob_timeout). It is sent
    /// once: whether to try again after a failure is the caller's to
    /// decide (an upload is safe to repeat).
    ///
    /// ```no_run
    /// use jmap_wire::mail::{EmailImport, EmailToImport};
    ///
    /// # async fn example(
    /// #     client: &jmap_wire::Client<impl jmap_wire::HttpTransport>,
    /// #     message: Vec<u8>,
    /// # ) -> Result<(), jmap_wire::Error> {
    /// let account = client.primary_account(jmap_wire::capability::MAIL)?;
    /// let blob = client.upload(account, "message/rfc822", message).await?;
    /// let mut import = EmailImport::new(account);
    /// let creation_id = import.email(EmailToImport::new(blob.blob_id, ["mailbox-id"]));
    /// let answer = client.call(import).await?;
    /// println!("{:?}", answer.outcome_of(&creation_id));
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// [`Error::UploadTooLarge`], before anything is sent, when the
    /// blob is over the session's `maxSizeUpload`; [`Error::Problem`]
    /// or [`Error::Http`] when the server refuses it;
    /// [`Error::Json`] when the answer is not the object the RFC
    /// describes, a `blobId` above all; and the errors of any exchange.
    pub async fn upload(
        &self,
        account_id: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<UploadedBlob, Error> {
        if let Some(limit) = self.session().core().and_then(|core| core.max_size_upload)
            && bytes.len() as u64 > limit
        {
            return Err(Error::UploadTooLarge {
                size: bytes.len() as u64,
                limit,
            });
        }
        let url = self
            .session()
            .upload_endpoint(self.session_url(), account_id)?;
        let content_type = if content_type.trim().is_empty() {
            OCTET_STREAM
        } else {
            content_type
        };
        let (response, _) = exchange(
            &self.transport,
            &self.config,
            Exchange {
                method: HttpMethod::Post,
                url,
                body: bytes,
                content_type: Some(content_type),
                accept: "application/json",
                accept_gzip: true,
                timeout: self.config.blob_timeout,
                max_response_bytes: MAX_UPLOAD_ANSWER_BYTES,
                redirects_without_credentials: false,
                exact_content: false,
                extra_headers: Vec::new(),
            },
        )
        .await?;
        serde_json::from_slice(&success(response)?.body)
            .map_err(|source| Error::json("upload answer", source))
    }

    /// Downloads a blob (RFC 8620 section 6.2): an authenticated GET of
    /// the session's `downloadUrl` with the download's account, blob
    /// id, name and type filled in. The whole blob is returned in
    /// memory, so it is read only up to the download's
    /// [`max_bytes`](BlobDownload::max_bytes), or the client's
    /// [`max_download_bytes`](crate::ClientBuilder::max_download_bytes).
    ///
    /// The request asks for no compression. An answer that is
    /// gzip-compressed all the same is decompressed (feature `gzip`),
    /// with the limit counted on what it expands to; one in an encoding
    /// the client cannot undo is an error, never returned as if it were
    /// the blob.
    ///
    /// ```no_run
    /// use jmap_wire::BlobDownload;
    ///
    /// # async fn example(
    /// #     client: &jmap_wire::Client<impl jmap_wire::HttpTransport>,
    /// #     part: &jmap_wire::mail::EmailBodyPart,
    /// # ) -> Result<(), jmap_wire::Error> {
    /// let account = client.primary_account(jmap_wire::capability::MAIL)?;
    /// if let Some(blob_id) = &part.blob_id {
    ///     let download = BlobDownload::new(account, blob_id)
    ///         .name(part.name.as_deref().unwrap_or("attachment"));
    ///     let blob = client.download(&download).await?;
    ///     println!("{} bytes", blob.bytes.len());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// [`Error::Problem`] or [`Error::Http`] when the server refuses
    /// ([`Error::http_status`] is 404 for a blob it does not have);
    /// [`Error::Transport`] of kind
    /// [`TooLarge`](crate::TransportErrorKind::TooLarge) for a blob
    /// over the limit ([`Error::is_response_too_large`]) or of kind
    /// [`Timeout`](crate::TransportErrorKind::Timeout);
    /// [`Error::Redirect`] for a redirect that is not followed.
    pub async fn download(&self, download: &BlobDownload) -> Result<Blob, Error> {
        let (response, _) = exchange(
            &self.transport,
            &self.config,
            Exchange {
                method: HttpMethod::Get,
                url: self.download_url(download)?,
                body: Vec::new(),
                content_type: None,
                accept: &download.content_type,
                accept_gzip: false,
                timeout: download.timeout.unwrap_or(self.config.blob_timeout),
                max_response_bytes: download.max_bytes.unwrap_or(self.config.max_download_bytes),
                redirects_without_credentials: download.redirects_without_credentials,
                exact_content: true,
                extra_headers: Vec::new(),
            },
        )
        .await?;
        let response = success(response)?;
        Ok(Blob {
            content_type: response.header("content-type").map(str::to_owned),
            bytes: response.body,
        })
    }

    /// The URL [`download`](Self::download) fetches: the session's
    /// `downloadUrl` template with the download's variables filled in
    /// and percent-encoded (RFC 8620 section 6.2, RFC 6570 level 1).
    /// For fetching a blob by other means, such as streaming it to a
    /// file with an HTTP client of your own; the request must be
    /// authenticated as the client's are.
    pub fn download_url(&self, download: &BlobDownload) -> Result<Url, Error> {
        self.session().download_endpoint(
            self.session_url(),
            &download.account_id,
            &download.blob_id,
            &download.name,
            &download.content_type,
        )
    }
}

/// A blob being downloaded piece by piece
/// ([`Client::download_stream`]). Dropping it stops the download.
#[derive(Debug)]
pub struct BlobStream<B> {
    body: B,
    content_type: Option<String>,
    content_length: Option<u64>,
    idle: Duration,
    max_bytes: Option<usize>,
    read: usize,
}

impl<B: BodyStream> BlobStream<B> {
    /// The next piece of the blob, or `None` at its end. Write each to
    /// wherever the blob is going:
    ///
    /// ```no_run
    /// # async fn example(
    /// #     client: &jmap_wire::Client<impl jmap_wire::StreamingTransport>,
    /// #     download: &jmap_wire::BlobDownload,
    /// #     mut file: impl std::io::Write,
    /// # ) -> Result<(), Box<dyn std::error::Error>> {
    /// let mut blob = client.download_stream(download).await?;
    /// while let Some(piece) = blob.chunk().await? {
    ///     file.write_all(&piece)?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// [`Error::Transport`]: of kind
    /// [`Timeout`](crate::TransportErrorKind::Timeout) when the server
    /// sends nothing for the client's
    /// [`timeout`](crate::ClientBuilder::timeout), of kind
    /// [`TooLarge`](crate::TransportErrorKind::TooLarge) once the blob
    /// is over the download's [`max_bytes`](BlobDownload::max_bytes),
    /// and whatever the transport reports when the connection breaks
    /// (a body that ends before its length is an error, so a blob read
    /// to `None` is whole).
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let Some(chunk) = self.body.chunk(Some(self.idle)).await? else {
            return Ok(None);
        };
        self.read = self.read.saturating_add(chunk.len());
        if let Some(limit) = self.max_bytes.filter(|limit| self.read > *limit) {
            return Err(TransportError::too_large(limit).into());
        }
        Ok(Some(chunk))
    }

    /// The `Content-Type` the server answered with. Do not trust it to
    /// say what the bytes are.
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    /// The size the server announced (`Content-Length`), when it did.
    pub fn content_length(&self) -> Option<u64> {
        self.content_length
    }
}

impl<T: StreamingTransport> Client<T> {
    /// Downloads a blob (RFC 8620 section 6.2) without holding it:
    /// the same request as [`download`](Self::download), with the body
    /// handed over piece by piece as it arrives, for a blob that goes
    /// to a file or is larger than memory should hold. It needs a
    /// transport that can stream ([`StreamingTransport`]), which the
    /// default one can.
    ///
    /// Its limits are a stream's, not a whole body's:
    ///
    /// - the server must begin its answer within the client's
    ///   [`timeout`](crate::ClientBuilder::timeout), and then never be
    ///   silent for longer than that; how long the whole download takes
    ///   is not limited, so
    ///   [`blob_timeout`](crate::ClientBuilder::blob_timeout) and the
    ///   download's own [`timeout`](BlobDownload::timeout) do not
    ///   apply;
    /// - there is no size limit unless the download sets
    ///   [`max_bytes`](BlobDownload::max_bytes); the client's
    ///   [`max_download_bytes`](crate::ClientBuilder::max_download_bytes)
    ///   bounds what is held in memory, which here is one piece;
    /// - the request asks for no compression, and an answer in a
    ///   content encoding all the same is an error (the whole-body
    ///   [`download`](Self::download) undoes gzip).
    ///
    /// Redirects and credentials are as for [`download`](Self::download).
    ///
    /// # Errors
    ///
    /// [`Error::Problem`] or [`Error::Http`] when the server refuses
    /// ([`Error::http_status`] is 404 for a blob it does not have),
    /// [`Error::Redirect`] for a redirect that is not followed, and
    /// [`Error::Transport`] when no answer begins in time or the
    /// download's limit is below the size the server announces.
    pub async fn download_stream(
        &self,
        download: &BlobDownload,
    ) -> Result<BlobStream<T::Body>, Error> {
        let idle = self.config.timeout;
        let (response, _) = open(
            &self.transport,
            &self.config,
            Exchange {
                method: HttpMethod::Get,
                url: self.download_url(download)?,
                body: Vec::new(),
                content_type: None,
                accept: &download.content_type,
                accept_gzip: false,
                timeout: idle,
                max_response_bytes: 0,
                redirects_without_credentials: download.redirects_without_credentials,
                exact_content: true,
                extra_headers: Vec::new(),
            },
        )
        .await?;
        let response = stream_success(response, idle).await?;
        let content_length = response
            .header("content-length")
            .and_then(|length| length.trim().parse::<u64>().ok());
        if let (Some(limit), Some(length)) = (download.max_bytes, content_length)
            && length > limit as u64
        {
            return Err(TransportError::too_large(limit).into());
        }
        Ok(BlobStream {
            content_type: response.header("content-type").map(str::to_owned),
            content_length,
            body: response.body,
            idle,
            max_bytes: download.max_bytes,
            read: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The answer of RFC 8620 section 6.1, and what servers leave out.
    #[test]
    fn an_upload_answer_needs_only_its_blob_id() {
        let blob: UploadedBlob = serde_json::from_value(json!({
            "accountId": "u1", "blobId": "G4c6751", "type": "image/png", "size": 3456,
            "expires": "2026-11-24T00:00:00Z"
        }))
        .expect("decodes");
        assert_eq!(blob.blob_id, "G4c6751");
        assert_eq!(blob.content_type.as_deref(), Some("image/png"));
        assert_eq!(blob.size, Some(3456));
        assert_eq!(blob.extra["expires"], "2026-11-24T00:00:00Z");

        let bare: UploadedBlob =
            serde_json::from_value(json!({"blobId": "b", "size": "big", "type": null}))
                .expect("decodes");
        assert_eq!((bare.size, bare.content_type), (None, None));
        assert!(serde_json::from_value::<UploadedBlob>(json!({"accountId": "u1"})).is_err());
    }
}
