//! Decodes every recorded response under `tests/fixtures`, whichever
//! server it is from (see `tests/fixtures/README.md`).

use std::{fs, path::Path};

use jmap_wire::{
    GetResponse, MethodError, MethodErrorType, ProblemDetails, ProblemKind, Response, Session,
    SetErrorType, SetResponse, capability,
    mail::{EmailSubmissionRecord, IdentityRecord},
};
use serde::de::DeserializeOwned;
use serde_json::Value;

fn read<T: DeserializeOwned>(path: &Path) -> T {
    let text = fs::read_to_string(path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path:?}: {error}"))
}

/// The first method response's arguments, as `T`; its name must be
/// `method`.
fn first<T: DeserializeOwned>(path: &Path, method: &str) -> T {
    let response: Response = read(path);
    let call_id = response
        .method_responses
        .first()
        .unwrap_or_else(|| panic!("{path:?} has no method response"))
        .call_id
        .clone();
    response
        .arguments_as(&call_id, method)
        .unwrap_or_else(|error| panic!("{path:?}: {error}"))
}

fn fixture(server: &str, name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(server)
        .join(name)
}

#[test]
fn every_fixture_of_every_server_decodes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut decoded = 0;
    for server in fs::read_dir(&root).expect("the fixtures directory") {
        let server = server.expect("an entry").path();
        if !server.is_dir() {
            continue;
        }
        for file in fs::read_dir(&server).expect("a server directory") {
            let path = file.expect("an entry").path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("a file name");
            let kind = name.split('.').next().unwrap_or_default();
            match kind {
                "session" => {
                    let session: Session = read(&path);
                    assert!(session.has_capability(capability::CORE), "{path:?}");
                    assert!(session.core().is_some(), "{path:?}");
                    // Nothing the server sent is dropped.
                    assert_eq!(
                        serde_json::to_value(&session).expect("encodes"),
                        read::<Value>(&path),
                        "{path:?}"
                    );
                }
                "problem" => {
                    read::<ProblemDetails>(&path);
                }
                "identity-get" => {
                    first::<GetResponse<IdentityRecord>>(&path, "Identity/get");
                }
                "identity-set" => {
                    first::<SetResponse<IdentityRecord>>(&path, "Identity/set");
                }
                "emailsubmission-set" => {
                    first::<SetResponse<EmailSubmissionRecord>>(&path, "EmailSubmission/set");
                }
                "email-set" => {
                    first::<SetResponse<Value>>(&path, "Email/set");
                }
                "method-error" => {
                    let response: Response = read(&path);
                    let error = &response.method_responses[0];
                    assert_eq!(error.name, "error", "{path:?}");
                    serde_json::from_value::<MethodError>(error.arguments.clone())
                        .unwrap_or_else(|error| panic!("{path:?}: {error}"));
                }
                // The reads: typed decoders come with the types.
                "mailbox-get" | "email-get" => {
                    read::<Response>(&path);
                }
                other => panic!("{path:?}: no decoder for fixtures named {other}"),
            }
            decoded += 1;
        }
    }
    assert!(decoded >= 12, "only {decoded} fixtures were found");
}

#[test]
fn the_rfc_examples_say_what_the_rfcs_say() {
    let session: Session = read(&fixture("rfc", "session.json"));
    assert_eq!(session.primary_account(capability::MAIL), Some("A13824"));
    assert_eq!(
        session.capabilities["https://example.com/apis/foobar"]["maxFoosFinangled"],
        42
    );

    let unknown: ProblemDetails = read(&fixture("rfc", "problem.unknown-capability.json"));
    assert_eq!(unknown.kind(), ProblemKind::UnknownCapability);
    let limit: ProblemDetails = read(&fixture("rfc", "problem.limit.json"));
    assert_eq!(limit.limit.as_deref(), Some("maxSizeRequest"));

    let identities: GetResponse<IdentityRecord> =
        first(&fixture("rfc", "identity-get.json"), "Identity/get");
    assert_eq!(identities.list.len(), 2);
    assert_eq!(identities.list[1].may_delete, Some(true));

    let refused: SetResponse<EmailSubmissionRecord> = first(
        &fixture("rfc", "emailsubmission-set.forbidden-to-send.json"),
        "EmailSubmission/set",
    );
    assert_eq!(
        refused.not_created["k1490"].error_type,
        SetErrorType::ForbiddenToSend
    );

    let response: Response = read(&fixture("rfc", "method-error.json"));
    let error: MethodError =
        serde_json::from_value(response.method_responses[0].arguments.clone()).expect("decodes");
    assert_eq!(error.error_type, MethodErrorType::UnknownMethod);
}

#[test]
fn james_shaped_answers_are_read_despite_their_nulls() {
    let session: Session = read(&fixture("james-3.9", "session.json"));
    assert!(session.has_capability("urn:apache:james:params:jmap:mail:quota"));
    assert!(!session.has_capability("urn:ietf:params:jmap:contacts"));
    assert_eq!(
        session.primary_account(capability::SUBMISSION),
        session.primary_account(capability::MAIL)
    );

    let identities: GetResponse<IdentityRecord> =
        first(&fixture("james-3.9", "identity-get.json"), "Identity/get");
    assert_eq!(identities.state, None);
    assert!(identities.not_found.is_empty());
    assert_eq!(identities.list[0].email.as_deref(), Some("jmap@localhost"));
    assert_eq!(identities.list[0].extra["sortOrder"], 100);

    let refused: SetResponse<EmailSubmissionRecord> = first(
        &fixture("james-3.9", "emailsubmission-set.invalid-arguments.json"),
        "EmailSubmission/set",
    );
    // A method error type used as a SetError type: kept as it came.
    assert_eq!(
        refused.not_created["k0"].error_type,
        SetErrorType::Other("invalidArguments".to_owned())
    );

    let destroyed: SetResponse<Value> = first(
        &fixture("james-3.9", "email-set.destroyed.json"),
        "Email/set",
    );
    assert_eq!(destroyed.destroyed.len(), 1);
    assert_eq!(
        destroyed.not_destroyed["not-an-id"].error_type,
        SetErrorType::NotFound
    );
}
