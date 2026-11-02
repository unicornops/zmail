//! JMAP identities (RFC 8621 section 6): reading the server's for the
//! sync (`zmail_core::identity_sync`) and for submission, and updating,
//! creating or destroying one when the user chooses to.
//!
//! Requests and answers go as plain JSON: `jmap-client`'s typed
//! `Identity/get` sends no method call at all and refuses what some
//! servers answer (Apache James's, for one), and neither of its helpers
//! adds the submission capability identities need.

use anyhow::{Context, Result, anyhow, bail};
use jmap_client::{client::Client, email::EmailAddress};
use serde_json::Value;
use zmail_core::{
    SyncTarget,
    identity_sync::{IdentityValues, ServerIdentity},
    participants::{Participant, format_address_list, parse_address_list},
};

use crate::connect_client;

/// An `Identity/get` of every identity, with the submission capability it
/// needs, which `jmap-client` does not add.
pub(crate) fn identity_get_request(client: &Client) -> jmap_client::core::request::Request<'_> {
    let mut request = client.build();
    request.get_identity();
    request.add_capability(jmap_client::URI::Submission);
    request
}

/// The arguments of the `method` answer in `method_response` (`[name,
/// arguments, call id]`), or why the server gave none.
pub(crate) fn method_arguments<'a>(
    method_response: Option<&'a Value>,
    method: &str,
) -> Result<&'a Value> {
    let [name, arguments, _] = method_response
        .and_then(Value::as_array)
        .and_then(|call| <&[Value; 3]>::try_from(call.as_slice()).ok())
        .with_context(|| format!("the JMAP server answered {method} with no method response"))?;
    if name.as_str() != Some(method) {
        bail!(
            "the JMAP server refused {method}: {}",
            refusal_reason(arguments)
        );
    }
    Ok(arguments)
}

/// A method error's or set error's description, else its type.
fn refusal_reason(error: &Value) -> &str {
    error
        .get("description")
        .or_else(|| error.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("no reason given")
}

/// The identities an `Identity/get` answer lists.
pub(crate) fn identity_list(method_response: Option<&Value>) -> Result<&[Value]> {
    Ok(method_arguments(method_response, "Identity/get")?
        .get("list")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default())
}

/// The server's identities, for the sync.
pub(crate) async fn fetch_server_identities(client: &Client) -> Result<Vec<ServerIdentity>> {
    let response = client
        .send::<Value>(&identity_get_request(client))
        .await
        .map_err(|error| anyhow!("failed to fetch the JMAP identities: {error}"))?;
    server_identities(response.method_responses().first())
}

/// The identities in an `Identity/get` answer; ones without an id or an
/// address are left out.
fn server_identities(method_response: Option<&Value>) -> Result<Vec<ServerIdentity>> {
    let text = |identity: &Value, key: &str| {
        identity
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Ok(identity_list(method_response)?
        .iter()
        .filter_map(|identity| {
            Some(ServerIdentity {
                id: identity.get("id")?.as_str()?.to_owned(),
                email: identity.get("email")?.as_str()?.to_owned(),
                values: IdentityValues {
                    name: text(identity, "name"),
                    reply_to: reply_to_summary(identity.get("replyTo")),
                    signature: text(identity, "textSignature"),
                },
                may_delete: identity
                    .get("mayDelete")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect())
}

/// A JMAP `EmailAddress[]` (or `null`) as an address list.
fn reply_to_summary(value: Option<&Value>) -> String {
    let participants = value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|address| {
            let email = address.get("email")?.as_str()?.trim();
            (!email.is_empty()).then(|| Participant {
                name: address
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned),
                email: Some(email.to_owned()),
            })
        })
        .collect::<Vec<_>>();
    format_address_list(&participants)
}

/// An address list as a JMAP `replyTo`: `None` (null) when it names no
/// address.
fn reply_to_addresses(value: &str) -> Option<Vec<EmailAddress>> {
    let addresses = parse_address_list(value)
        .into_iter()
        .filter_map(|participant| match (participant.name, participant.email) {
            (Some(name), Some(email)) => Some(EmailAddress::from((name, email))),
            (None, Some(email)) => Some(EmailAddress::from(email)),
            (_, None) => None,
        })
        .collect::<Vec<_>>();
    (!addresses.is_empty()).then_some(addresses)
}

/// Sets `values` on an identity being created or updated. An empty
/// reply-to is sent as `null`, which clears the server's.
fn set_values(
    identity: &mut jmap_client::identity::Identity<jmap_client::Set>,
    values: &IdentityValues,
) {
    identity
        .name(values.name.clone())
        .text_signature(values.signature.clone())
        .reply_to(reply_to_addresses(&values.reply_to).map(Vec::into_iter));
}

/// "Update the server": sets server identity `server_id`'s name, reply-to
/// and text signature to `values` (`Identity/set` update). A refusal
/// (`notUpdated`, say `forbidden`) is the error, with the server's reason.
pub async fn update_server_identity(
    target: &SyncTarget,
    secret: &str,
    server_id: &str,
    values: &IdentityValues,
) -> Result<()> {
    let client = connect_client(&target.endpoint, &target.username, secret, target.auth).await?;
    update_identity(&client, server_id, values).await
}

async fn update_identity(client: &Client, server_id: &str, values: &IdentityValues) -> Result<()> {
    let mut request = client.build();
    set_values(request.set_identity().update(server_id), values);
    request.add_capability(jmap_client::URI::Submission);
    let response = client
        .send::<Value>(&request)
        .await
        .map_err(|error| anyhow!("failed to update the JMAP identity: {error}"))?;
    updated(response.method_responses().first(), server_id)
}

/// Whether an `Identity/set` answer updated `server_id`, or why not.
fn updated(method_response: Option<&Value>, server_id: &str) -> Result<()> {
    let arguments = method_arguments(method_response, "Identity/set")?;
    if arguments
        .get("updated")
        .and_then(Value::as_object)
        .is_some_and(|updated| updated.contains_key(server_id))
    {
        return Ok(());
    }
    match arguments
        .get("notUpdated")
        .and_then(|refused| refused.get(server_id))
    {
        Some(error) => bail!(
            "the server refused to change the identity: {}",
            refusal_reason(error)
        ),
        None => bail!("the server did not say whether it changed the identity"),
    }
}

/// Creates an identity for `email` with `values` on the server
/// (`Identity/set` create), for one the server no longer lists; returns
/// the server's id for it. Servers only allow addresses the account may
/// send from.
pub async fn create_server_identity(
    target: &SyncTarget,
    secret: &str,
    email: &str,
    values: &IdentityValues,
) -> Result<String> {
    let client = connect_client(&target.endpoint, &target.username, secret, target.auth).await?;
    create_identity(&client, email, values).await
}

async fn create_identity(client: &Client, email: &str, values: &IdentityValues) -> Result<String> {
    let mut request = client.build();
    let identity = request.set_identity().create();
    identity.email(email);
    set_values(identity, values);
    request.add_capability(jmap_client::URI::Submission);
    let response = client
        .send::<Value>(&request)
        .await
        .map_err(|error| anyhow!("failed to create the JMAP identity: {error}"))?;
    created(response.method_responses().first())
}

/// The id of the identity an `Identity/set` answer created, or why it
/// created none.
fn created(method_response: Option<&Value>) -> Result<String> {
    let arguments = method_arguments(method_response, "Identity/set")?;
    if let Some(id) = arguments
        .get("created")
        .and_then(Value::as_object)
        .and_then(|created| created.values().next())
        .and_then(|identity| identity.get("id"))
        .and_then(Value::as_str)
    {
        return Ok(id.to_owned());
    }
    match arguments
        .get("notCreated")
        .and_then(Value::as_object)
        .and_then(|refused| refused.values().next())
    {
        Some(error) => bail!(
            "the server refused to create the identity: {}",
            refusal_reason(error)
        ),
        None => bail!("the server did not say whether it created the identity"),
    }
}

/// Destroys server identity `server_id` (`Identity/set` destroy), when
/// the user removes an identity from Zmail and the server too. A refusal
/// (`notDestroyed`, say `forbidden` for one the server made) is the error,
/// with the server's reason.
pub async fn destroy_server_identity(
    target: &SyncTarget,
    secret: &str,
    server_id: &str,
) -> Result<()> {
    let client = connect_client(&target.endpoint, &target.username, secret, target.auth).await?;
    destroy_identity(&client, server_id).await
}

async fn destroy_identity(client: &Client, server_id: &str) -> Result<()> {
    let mut request = client.build();
    request.set_identity().destroy([server_id]);
    request.add_capability(jmap_client::URI::Submission);
    let response = client
        .send::<Value>(&request)
        .await
        .map_err(|error| anyhow!("failed to delete the JMAP identity: {error}"))?;
    destroyed(response.method_responses().first(), server_id)
}

/// Whether an `Identity/set` answer destroyed `server_id`, or why not.
fn destroyed(method_response: Option<&Value>, server_id: &str) -> Result<()> {
    let arguments = method_arguments(method_response, "Identity/set")?;
    if arguments
        .get("destroyed")
        .and_then(Value::as_array)
        .is_some_and(|destroyed| destroyed.iter().any(|id| id.as_str() == Some(server_id)))
    {
        return Ok(());
    }
    match arguments
        .get("notDestroyed")
        .and_then(|refused| refused.get(server_id))
    {
        Some(error) => bail!(
            "the server refused to delete the identity: {}",
            refusal_reason(error)
        ),
        None => bail!("the server did not say whether it deleted the identity"),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::{TcpListener, TcpStream},
        sync::{Arc, Mutex},
    };

    use serde_json::{Value, json};
    use zmail_core::{AuthKind, identity_sync::IdentityValues};

    use super::{created, destroyed, reply_to_addresses, server_identities, updated};

    #[test]
    fn server_identities_are_read_from_a_james_shaped_answer() {
        let response = json!(["Identity/get", {
            "accountId": "a1",
            "state": null,
            "notFound": null,
            "list": [
                {"id": "i1", "name": "", "email": "jmap@localhost", "replyTo": null,
                 "bcc": null, "textSignature": "", "htmlSignature": "", "mayDelete": false,
                 "sortOrder": 100},
                {"id": "i2", "name": "Help desk", "email": "help@localhost",
                 "replyTo": [{"name": "Tickets, Inc", "email": "tickets@localhost"},
                             {"name": null, "email": "ops@localhost"}],
                 "textSignature": "-- Help", "mayDelete": true},
                {"id": "i3"}
            ]
        }, "s0"]);
        let identities = server_identities(Some(&response)).expect("identities");
        assert_eq!(identities.len(), 2);
        assert_eq!(identities[0].id, "i1");
        assert_eq!(identities[0].values, IdentityValues::default());
        assert!(!identities[0].may_delete);
        assert_eq!(identities[1].values.name, "Help desk");
        assert_eq!(
            identities[1].values.reply_to,
            "\"Tickets, Inc\" <tickets@localhost>, ops@localhost"
        );
        assert_eq!(identities[1].values.signature, "-- Help");
        assert!(identities[1].may_delete);

        let refused = json!(["error", {"type": "unknownMethod"}, "s0"]);
        let error = server_identities(Some(&refused)).expect_err("refused");
        assert!(error.to_string().contains("unknownMethod"), "{error}");
    }

    #[test]
    fn reply_to_round_trips_and_empty_is_null() {
        let addresses = reply_to_addresses("\"Tickets, Inc\" <tickets@localhost>, ops@localhost")
            .expect("addresses");
        let json = serde_json::to_value(&addresses).expect("serialises");
        assert_eq!(
            super::reply_to_summary(Some(&json)),
            "\"Tickets, Inc\" <tickets@localhost>, ops@localhost"
        );
        assert!(reply_to_addresses("  ").is_none());
    }

    /// A JMAP server on loopback, just enough of one for identities:
    /// the session, `Identity/get` and `Identity/set`. Identity `locked`
    /// refuses changes, as servers do for identities the user may not
    /// edit. It records whether each request asked for the submission
    /// capability.
    struct FakeServer {
        endpoint: String,
        identities: Arc<Mutex<Vec<Value>>>,
        submission_asked: Arc<Mutex<Vec<bool>>>,
    }

    impl FakeServer {
        fn start(identities: Vec<Value>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
            let base = format!("http://{}", listener.local_addr().expect("an address"));
            let identities = Arc::new(Mutex::new(identities));
            let submission_asked = Arc::new(Mutex::new(Vec::new()));
            let (state, asked, url) = (identities.clone(), submission_asked.clone(), base.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    serve(stream, &url, &state, &asked);
                }
            });
            Self {
                endpoint: base,
                identities,
                submission_asked,
            }
        }
    }

    fn serve(
        mut stream: TcpStream,
        base: &str,
        identities: &Mutex<Vec<Value>>,
        submission_asked: &Mutex<Vec<bool>>,
    ) {
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
            })
        } else {
            let request: Value = serde_json::from_slice(&body).expect("a JSON request");
            submission_asked.lock().expect("the lock").push(
                request["using"].as_array().is_some_and(|using| {
                    using
                        .iter()
                        .any(|uri| uri == "urn:ietf:params:jmap:submission")
                }),
            );
            let call = &request["methodCalls"][0];
            let mut identities = identities.lock().expect("the lock");
            let response = match call[0].as_str() {
                Some("Identity/get") => json!(["Identity/get",
                    {"accountId": "a1", "state": "s1", "list": *identities}, call[2]]),
                Some("Identity/set") => {
                    let mut answer = json!({"accountId": "a1"});
                    if let Some(updates) = call[1]["update"].as_object() {
                        for (id, patch) in updates {
                            let found =
                                identities.iter_mut().find(|identity| identity["id"] == *id);
                            match found {
                                Some(identity) if id != "locked" => {
                                    for (key, value) in patch.as_object().expect("a patch") {
                                        identity[key] = value.clone();
                                    }
                                    answer["updated"][id] = Value::Null;
                                }
                                _ => {
                                    answer["notUpdated"][id] = json!({
                                        "type": "forbidden",
                                        "description": "this identity can't be changed"
                                    })
                                }
                            }
                        }
                    }
                    if let Some(destroys) = call[1]["destroy"].as_array() {
                        let mut gone = Vec::new();
                        for id in destroys.iter().filter_map(Value::as_str) {
                            let index = identities.iter().position(|identity| identity["id"] == id);
                            match index {
                                Some(index) if identities[index]["mayDelete"] == true => {
                                    identities.remove(index);
                                    gone.push(id);
                                }
                                _ => {
                                    answer["notDestroyed"][id] = json!({
                                        "type": "forbidden",
                                        "description": "this identity can't be deleted"
                                    })
                                }
                            }
                        }
                        answer["destroyed"] = json!(gone);
                    }
                    if let Some(creates) = call[1]["create"].as_object() {
                        for (create_id, identity) in creates {
                            let mut identity = identity.clone();
                            let id = format!("new-{}", identities.len());
                            identity["id"] = json!(id);
                            identities.push(identity);
                            answer["created"][create_id] = json!({"id": id});
                        }
                    }
                    json!(["Identity/set", answer, call[2]])
                }
                _ => json!(["error", {"type": "unknownMethod"}, call[2]]),
            };
            json!({"methodResponses": [response], "sessionState": "s1"})
        };
        let body = answer.to_string();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
    }

    #[tokio::test]
    async fn identities_are_read_updated_and_created_on_a_jmap_server() {
        let server = FakeServer::start(vec![
            json!({"id": "i1", "name": "Rob", "email": "rob@example.com", "replyTo": null,
                   "textSignature": "-- Rob", "mayDelete": false}),
            json!({"id": "locked", "name": "Admin", "email": "admin@example.com",
                   "textSignature": "", "mayDelete": false}),
        ]);
        let client = crate::connect_client(
            &server.endpoint,
            "rob@example.com",
            "pw",
            AuthKind::Password,
        )
        .await
        .expect("the session loads");
        let listed = super::fetch_server_identities(&client)
            .await
            .expect("the identities load");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].values.signature, "-- Rob");

        let edited = IdentityValues {
            name: "Rob Lazzurs".to_owned(),
            reply_to: "Team <team@example.com>".to_owned(),
            signature: "-- Rob, from Zmail".to_owned(),
        };
        super::update_identity(&client, "i1", &edited)
            .await
            .expect("the server takes the edit");
        let listed = super::fetch_server_identities(&client)
            .await
            .expect("the identities load");
        assert_eq!(listed[0].values, edited, "read back as sent");

        // Clearing the reply-to sends null.
        let cleared = IdentityValues {
            reply_to: String::new(),
            ..edited.clone()
        };
        super::update_identity(&client, "i1", &cleared)
            .await
            .expect("the server takes the edit");
        assert_eq!(
            server.identities.lock().expect("the lock")[0]["replyTo"],
            Value::Null
        );

        let error = super::update_identity(&client, "locked", &edited)
            .await
            .expect_err("refused");
        assert!(
            error.to_string().contains("this identity can't be changed"),
            "{error}"
        );

        let id = super::create_identity(&client, "help@example.com", &edited)
            .await
            .expect("created");
        let listed = super::fetch_server_identities(&client)
            .await
            .expect("the identities load");
        let made = listed
            .iter()
            .find(|identity| identity.id == id)
            .expect("listed");
        assert_eq!(made.email, "help@example.com");
        assert_eq!(made.values, edited);

        // Destroying: the one the user made goes; one the server made is
        // refused, with the server's reason.
        server
            .identities
            .lock()
            .expect("the lock")
            .last_mut()
            .expect("made")["mayDelete"] = json!(true);
        super::destroy_identity(&client, &id)
            .await
            .expect("destroyed");
        let listed = super::fetch_server_identities(&client)
            .await
            .expect("the identities load");
        assert!(
            listed.iter().all(|identity| identity.id != id),
            "{listed:?}"
        );
        let error = super::destroy_identity(&client, "i1")
            .await
            .expect_err("refused");
        assert!(
            error.to_string().contains("this identity can't be deleted"),
            "{error}"
        );

        let asked = server.submission_asked.lock().expect("the lock");
        assert!(
            !asked.is_empty() && asked.iter().all(|asked| *asked),
            "{asked:?}"
        );
    }

    #[test]
    fn set_answers_say_what_happened_or_why_not() {
        let done = json!(["Identity/set", {"updated": {"i1": null}}, "s0"]);
        updated(Some(&done), "i1").expect("updated");
        let refused = json!(["Identity/set", {"notUpdated": {"i1": {
            "type": "forbidden", "description": "server-set identities can't change"}}}, "s0"]);
        let error = updated(Some(&refused), "i1").expect_err("refused");
        assert!(
            error
                .to_string()
                .contains("server-set identities can't change"),
            "{error}"
        );
        assert!(updated(Some(&json!(["Identity/set", {}, "s0"])), "i1").is_err());

        let made = json!(["Identity/set", {"created": {"c0": {"id": "i9"}}}, "s0"]);
        assert_eq!(created(Some(&made)).expect("created"), "i9");
        let refused =
            json!(["Identity/set", {"notCreated": {"c0": {"type": "forbiddenFrom"}}}, "s0"]);
        let error = created(Some(&refused)).expect_err("refused");
        assert!(error.to_string().contains("forbiddenFrom"), "{error}");

        let gone = json!(["Identity/set", {"destroyed": ["i1"]}, "s0"]);
        destroyed(Some(&gone), "i1").expect("destroyed");
        let refused =
            json!(["Identity/set", {"notDestroyed": {"i1": {"type": "forbidden"}}}, "s0"]);
        let error = destroyed(Some(&refused), "i1").expect_err("refused");
        assert!(error.to_string().contains("forbidden"), "{error}");
        assert!(destroyed(Some(&json!(["Identity/set", {}, "s0"])), "i1").is_err());
    }
}
