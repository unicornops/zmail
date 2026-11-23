//! The Session object (RFC 8620 section 2): what a server offers and
//! where its endpoints are.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use url::Url;

use crate::{capability, error::Error, method::null_as_default, record::lenient};

/// The JMAP Session object (RFC 8620 section 2).
///
/// Capabilities are kept as the JSON the server sent, whether this crate
/// knows them or not: read one with [`capability`](Self::capability)
/// into a type of your own, or Core's with [`core`](Self::core).
/// Properties the RFC does not define are kept in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Session {
    /// The server's capabilities by URI, each with its own object.
    #[serde(default, deserialize_with = "null_as_default")]
    pub capabilities: BTreeMap<String, Value>,
    /// The accounts the user has access to, by id.
    #[serde(default, deserialize_with = "null_as_default")]
    pub accounts: BTreeMap<String, Account>,
    /// The id of the user's own account for each capability, where they
    /// have one.
    #[serde(default, deserialize_with = "null_as_default")]
    pub primary_accounts: BTreeMap<String, String>,
    /// The username the credentials belong to.
    #[serde(default)]
    pub username: String,
    /// The URL of the API endpoint, which requests are POSTed to.
    pub api_url: String,
    /// The URL template for downloading blobs (section 6.2).
    #[serde(default)]
    pub download_url: String,
    /// The URL template for uploading blobs (section 6.1).
    #[serde(default)]
    pub upload_url: String,
    /// The URL template of the event source (section 7.3).
    #[serde(default)]
    pub event_source_url: String,
    /// A string that changes whenever anything else in the session does.
    #[serde(default)]
    pub state: String,
    /// Properties this crate has no field for.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// An account in the session (RFC 8620 section 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Account {
    /// A name for the account, such as its email address.
    #[serde(default)]
    pub name: String,
    /// Whether the account is the user's own rather than shared.
    #[serde(default)]
    pub is_personal: bool,
    /// Whether the whole account is read-only.
    #[serde(default)]
    pub is_read_only: bool,
    /// The capabilities usable with this account, each with its limits
    /// for the account.
    #[serde(default, deserialize_with = "null_as_default")]
    pub account_capabilities: BTreeMap<String, Value>,
    /// Properties this crate has no field for.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// The limits of `urn:ietf:params:jmap:core` (RFC 8620 section 2). A
/// limit the server did not state, or stated as something other than a
/// whole number, is `None`.
///
/// The ones a reader of mail needs: `max_objects_in_get` (how many ids
/// one `/get` may ask for; [`IdBatches`](crate::IdBatches) splits a
/// longer list), `max_calls_in_request` (how many method calls one
/// request may chain) and `max_size_request` (the largest request body,
/// in octets).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct CoreCapabilities {
    /// The largest upload, in octets.
    #[serde(default, deserialize_with = "lenient")]
    pub max_size_upload: Option<u64>,
    /// How many uploads may run at once.
    #[serde(default, deserialize_with = "lenient")]
    pub max_concurrent_upload: Option<u64>,
    /// The largest API request, in octets.
    #[serde(default, deserialize_with = "lenient")]
    pub max_size_request: Option<u64>,
    /// How many API requests may run at once.
    #[serde(default, deserialize_with = "lenient")]
    pub max_concurrent_requests: Option<u64>,
    /// How many method calls one request may have.
    #[serde(default, deserialize_with = "lenient")]
    pub max_calls_in_request: Option<u64>,
    /// How many records one `/get` may ask for.
    #[serde(default, deserialize_with = "lenient")]
    pub max_objects_in_get: Option<u64>,
    /// How many records one `/set` may create, update and destroy in
    /// all.
    #[serde(default, deserialize_with = "lenient")]
    pub max_objects_in_set: Option<u64>,
    /// The collation algorithms the server can sort with.
    #[serde(default, deserialize_with = "null_as_default")]
    pub collation_algorithms: Vec<String>,
}

impl Session {
    /// Whether the server advertises the capability.
    pub fn has_capability(&self, uri: &str) -> bool {
        self.capabilities.contains_key(uri)
    }

    /// The server-wide object of a capability, read as `T`; `None` when
    /// the server does not advertise it.
    pub fn capability<T: DeserializeOwned>(&self, uri: &str) -> Option<Result<T, Error>> {
        let value = self.capabilities.get(uri)?;
        Some(T::deserialize(value).map_err(|source| Error::json(uri, source)))
    }

    /// The limits of JMAP Core, when the server states them in a form
    /// that can be read.
    pub fn core(&self) -> Option<CoreCapabilities> {
        self.capability(capability::CORE)?.ok()
    }

    /// The id of the user's primary account for a capability.
    pub fn primary_account(&self, capability: &str) -> Option<&str> {
        self.primary_accounts.get(capability).map(String::as_str)
    }

    /// The API endpoint, resolved against the session's own URL.
    pub fn api_endpoint(&self, session_url: &Url) -> Result<Url, Error> {
        resolve(session_url, &self.api_url)
    }

    /// Where to download a blob from (RFC 8620 section 6.2): the
    /// `downloadUrl` template with its variables filled in. `name` is the
    /// file name the server should give the download and `content_type`
    /// the media type it should say it has.
    pub fn download_endpoint(
        &self,
        session_url: &Url,
        account_id: &str,
        blob_id: &str,
        name: &str,
        content_type: &str,
    ) -> Result<Url, Error> {
        let url = expand(
            &self.download_url,
            &[
                ("accountId", account_id),
                ("blobId", blob_id),
                ("name", name),
                ("type", content_type),
            ],
        );
        resolve(session_url, &url)
    }

    /// Where to upload a blob to (RFC 8620 section 6.1): the `uploadUrl`
    /// template with the account filled in.
    pub fn upload_endpoint(&self, session_url: &Url, account_id: &str) -> Result<Url, Error> {
        resolve(
            session_url,
            &expand(&self.upload_url, &[("accountId", account_id)]),
        )
    }

    /// Where the event source is (RFC 8620 section 7.3): the
    /// `eventSourceUrl` template with the data types to hear about (`*`
    /// for all), whether the server should close the connection after
    /// each state event (`state`) or not (`no`), and the seconds between
    /// pings (0 for none).
    pub fn event_source_endpoint(
        &self,
        session_url: &Url,
        types: &str,
        close_after: &str,
        ping: u32,
    ) -> Result<Url, Error> {
        let url = expand(
            &self.event_source_url,
            &[
                ("types", types),
                ("closeafter", close_after),
                ("ping", &ping.to_string()),
            ],
        );
        resolve(session_url, &url)
    }
}

fn resolve(base: &Url, reference: &str) -> Result<Url, Error> {
    if reference.is_empty() {
        return Err(Error::Session("URL for this endpoint".to_owned()));
    }
    let url = base.join(reference).map_err(|error| Error::InvalidUrl {
        url: reference.to_owned(),
        reason: error.to_string(),
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::InvalidUrl {
            url: reference.to_owned(),
            reason: "it is neither HTTP nor HTTPS".to_owned(),
        });
    }
    Ok(url)
}

/// Fills in a level 1 URI Template (RFC 6570 section 3.2.2), the form
/// the session's URL templates take: `{name}` becomes the variable's
/// value with everything but unreserved characters percent-encoded, and
/// a variable that is not given becomes nothing.
fn expand(template: &str, variables: &[(&str, &str)]) -> String {
    let mut expanded = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        expanded.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            // Not a template expression: kept as it is.
            expanded.push_str(&rest[open..]);
            return expanded;
        };
        let name = &after[..close];
        if let Some((_, value)) = variables.iter().find(|(variable, _)| *variable == name) {
            for byte in value.bytes() {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                    expanded.push(char::from(byte));
                } else {
                    expanded.push_str(&format!("%{byte:02X}"));
                }
            }
        }
        rest = &after[close + 1..];
    }
    expanded.push_str(rest);
    expanded
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The example Session object of RFC 8620 section 2.1, with the
    /// parts the RFC elides (`...`) left out and its two slips put
    /// right: the comma missing after the mail capability, and
    /// `maxConcurrentRequest` for `maxConcurrentRequests`.
    pub(crate) fn rfc_session() -> Value {
        json!({
            "capabilities": {
                "urn:ietf:params:jmap:core": {
                    "maxSizeUpload": 50000000,
                    "maxConcurrentUpload": 8,
                    "maxSizeRequest": 10000000,
                    "maxConcurrentRequests": 8,
                    "maxCallsInRequest": 32,
                    "maxObjectsInGet": 256,
                    "maxObjectsInSet": 128,
                    "collationAlgorithms": [
                        "i;ascii-numeric", "i;ascii-casemap", "i;unicode-casemap"
                    ]
                },
                "urn:ietf:params:jmap:mail": {},
                "urn:ietf:params:jmap:contacts": {},
                "https://example.com/apis/foobar": {"maxFoosFinangled": 42}
            },
            "accounts": {
                "A13824": {
                    "name": "john@example.com",
                    "isPersonal": true,
                    "isReadOnly": false,
                    "accountCapabilities": {
                        "urn:ietf:params:jmap:mail": {
                            "maxMailboxesPerEmail": null, "maxMailboxDepth": 10
                        },
                        "urn:ietf:params:jmap:contacts": {}
                    }
                },
                "A97813": {
                    "name": "jane@example.com",
                    "isPersonal": false,
                    "isReadOnly": true,
                    "accountCapabilities": {
                        "urn:ietf:params:jmap:mail": {
                            "maxMailboxesPerEmail": 1, "maxMailboxDepth": 10
                        }
                    }
                }
            },
            "primaryAccounts": {
                "urn:ietf:params:jmap:mail": "A13824",
                "urn:ietf:params:jmap:contacts": "A13824"
            },
            "username": "john@example.com",
            "apiUrl": "https://jmap.example.com/api/",
            "downloadUrl":
                "https://jmap.example.com/download/{accountId}/{blobId}/{name}?accept={type}",
            "uploadUrl": "https://jmap.example.com/upload/{accountId}/",
            "eventSourceUrl":
                "https://jmap.example.com/eventsource/?types={types}&closeafter={closeafter}&ping={ping}",
            "state": "75128aab4b1b"
        })
    }

    fn session_url() -> Url {
        Url::parse("https://jmap.example.com/.well-known/jmap").expect("a URL")
    }

    #[test]
    fn the_session_of_rfc_8620_section_2_1_is_read() {
        let session: Session = serde_json::from_value(rfc_session()).expect("decodes");
        assert_eq!(session.username, "john@example.com");
        assert_eq!(session.state, "75128aab4b1b");
        assert_eq!(session.primary_account(capability::MAIL), Some("A13824"));
        assert_eq!(session.primary_account(capability::SUBMISSION), None);
        assert!(session.accounts["A97813"].is_read_only);
        assert!(!session.accounts["A97813"].is_personal);
        assert_eq!(
            session.accounts["A13824"].account_capabilities[capability::MAIL]["maxMailboxDepth"],
            10
        );

        let core = session.core().expect("core limits");
        assert_eq!(core.max_size_upload, Some(50_000_000));
        assert_eq!(core.max_concurrent_requests, Some(8));
        assert_eq!(core.max_objects_in_get, Some(256));
        assert_eq!(core.collation_algorithms.len(), 3);
    }

    #[test]
    fn a_limit_that_is_no_whole_number_is_not_known_and_the_others_still_are() {
        let session: Session = serde_json::from_value(json!({
            "capabilities": {"urn:ietf:params:jmap:core": {
                "maxSizeUpload": "50000000",
                "maxSizeRequest": 1e7,
                "maxCallsInRequest": -1,
                "maxObjectsInGet": 500,
                "maxObjectsInSet": null
            }},
            "apiUrl": "/api"
        }))
        .expect("decodes");
        let core = session.core().expect("the limits that can be read");
        assert_eq!(core.max_objects_in_get, Some(500));
        assert_eq!(core.max_size_upload, None);
        assert_eq!(core.max_calls_in_request, None);
        assert_eq!(core.max_objects_in_set, None);
        assert!(core.collation_algorithms.is_empty());
    }

    #[test]
    fn capabilities_this_crate_does_not_know_are_kept() {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Foobar {
            max_foos_finangled: u32,
        }

        let session: Session = serde_json::from_value(rfc_session()).expect("decodes");
        assert!(session.has_capability("urn:ietf:params:jmap:contacts"));
        assert!(!session.has_capability(capability::SUBMISSION));
        let foobar: Foobar = session
            .capability("https://example.com/apis/foobar")
            .expect("advertised")
            .expect("decodes");
        assert_eq!(foobar.max_foos_finangled, 42);
        assert!(session.capability::<Foobar>("urn:example:absent").is_none());
        assert!(
            session
                .capability::<Foobar>(capability::MAIL)
                .expect("advertised")
                .is_err(),
            "a capability that is not of the type asked for is an error, not a panic"
        );

        // Writing the session out again loses nothing.
        assert_eq!(
            serde_json::to_value(&session).expect("encodes"),
            rfc_session()
        );
    }

    /// The RFC's own example writes `maxConcurrentRequest`; a limit under
    /// a name this crate does not expect is simply not known, and
    /// properties from later specifications are kept.
    #[test]
    fn a_session_with_slips_and_additions_still_reads() {
        let mut value = rfc_session();
        let core = &mut value["capabilities"][capability::CORE];
        core["maxConcurrentRequest"] = core["maxConcurrentRequests"].take();
        value["futureProperty"] = json!({"a": 1});
        value["accounts"]["A13824"]["futureAccountProperty"] = json!(true);
        value["primaryAccounts"] = Value::Null;
        let session: Session = serde_json::from_value(value).expect("decodes");
        let core = session.core().expect("core limits");
        assert_eq!(core.max_concurrent_requests, None);
        assert_eq!(core.max_calls_in_request, Some(32));
        assert_eq!(session.extra["futureProperty"], json!({"a": 1}));
        assert_eq!(
            session.accounts["A13824"].extra["futureAccountProperty"],
            true
        );
        assert!(session.primary_accounts.is_empty());

        assert!(
            serde_json::from_value::<Session>(json!({"capabilities": {}})).is_err(),
            "a session without an API URL is no session"
        );
    }

    /// RFC 8620 sections 6.1, 6.2 and 7.3, with the templates of the
    /// section 2.1 example.
    #[test]
    fn url_templates_are_filled_in_and_encoded() {
        let session: Session = serde_json::from_value(rfc_session()).expect("decodes");
        let base = session_url();
        assert_eq!(
            session.api_endpoint(&base).expect("a URL").as_str(),
            "https://jmap.example.com/api/"
        );
        assert_eq!(
            session
                .download_endpoint(
                    &base,
                    "A13824",
                    "G4c6751",
                    "my report/1.pdf",
                    "application/pdf"
                )
                .expect("a URL")
                .as_str(),
            "https://jmap.example.com/download/A13824/G4c6751/my%20report%2F1.pdf\
             ?accept=application%2Fpdf"
        );
        assert_eq!(
            session
                .upload_endpoint(&base, "A13824")
                .expect("a URL")
                .as_str(),
            "https://jmap.example.com/upload/A13824/"
        );
        assert_eq!(
            session
                .event_source_endpoint(&base, "*", "no", 30)
                .expect("a URL")
                .as_str(),
            "https://jmap.example.com/eventsource/?types=%2A&closeafter=no&ping=30"
        );
    }

    #[test]
    fn relative_and_unusable_endpoints() {
        let mut value = rfc_session();
        value["apiUrl"] = json!("/jmap/api");
        value["uploadUrl"] = json!("upload/{accountId}/{unknown}");
        value["downloadUrl"] = json!("ftp://example.com/{blobId}");
        value["eventSourceUrl"] = json!("");
        let session: Session = serde_json::from_value(value).expect("decodes");
        let base = Url::parse("https://example.com/jmap/session").expect("a URL");
        assert_eq!(
            session.api_endpoint(&base).expect("a URL").as_str(),
            "https://example.com/jmap/api"
        );
        assert_eq!(
            session
                .upload_endpoint(&base, "a b")
                .expect("a URL")
                .as_str(),
            "https://example.com/jmap/upload/a%20b/"
        );
        assert!(matches!(
            session.download_endpoint(&base, "a", "b", "n", "t"),
            Err(Error::InvalidUrl { .. })
        ));
        assert!(matches!(
            session.event_source_endpoint(&base, "*", "no", 0),
            Err(Error::Session(_))
        ));
    }
}
