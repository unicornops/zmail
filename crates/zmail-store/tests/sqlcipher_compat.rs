//! A store written by an earlier SQLCipher still opens (ADR 0005).
//!
//! `tests/fixtures/store-sqlcipher-4.14` is a store Zmail wrote with
//! SQLCipher 4.14.0 (SQLite 3.51.3, rusqlite 0.40) on Linux, so through
//! OpenSSL: an account, a synced message with a body and an attachment, the
//! search index and a draft with an attachment, under a fixed key. Every
//! build opens a copy with the SQLCipher it links (OpenSSL on Linux and
//! Windows, CommonCrypto on macOS), applies the current migrations, reads
//! everything back, writes, and opens it again. When the bundled SQLCipher
//! changes, this is what shows existing stores survive the upgrade.
//!
//! The fixture is regenerated only when the store's format deliberately
//! changes, never to make this test pass:
//! `ZMAIL_WRITE_STORE_FIXTURE=1 cargo test -p zmail-store --test sqlcipher_compat -- --ignored`

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use zmail_core::{
    AccountSyncSnapshot, AuthKind, CachePolicySummary, MailboxSnapshot, MessageAttachment,
    MessageSnapshot, NewAccount, NewDraft, NewIdentity, OutgoingConfig, ProtocolKind,
    ThreadSnapshot,
};
use zmail_security::vault::{KeyId, StoreKey};
use zmail_store::LocalStore;

const FIXTURE: &str = "store-sqlcipher-4.14";
const DATABASE: &str = "store.sqlite3";

const USERNAME: &str = "fixture@example.com";
const SUBJECT: &str = "Fixture subject lighthouse";
const BODY: &str = "Fixture body marmalade porcupine";
const SEARCH_TERM: &str = "porcupine";
const ATTACHMENT_NAME: &str = "fixture-report.pdf";
const ATTACHMENT_BYTES: &str = "Fixture attachment bytes quasar";
const DRAFT_SUBJECT: &str = "Fixture draft";
const DRAFT_BODY: &str = "Fixture draft body tamarind";
const DRAFT_ATTACHMENT_NAME: &str = "fixture-notes.txt";
const DRAFT_ATTACHMENT_BYTES: &str = "Fixture draft attachment saffron";

/// The fixture's key. Not a secret: it only ever encrypts this fixture.
fn fixture_key() -> StoreKey {
    StoreKey::from_fixture(KeyId::from_bytes([0x5a; 16]), [0x42; 32])
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(FIXTURE)
}

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "zmail-sqlcipher-compat-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("temp dir");
    directory
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("created");
    for entry in fs::read_dir(from).expect("listed").flatten() {
        let source = entry.path();
        let destination = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir(&source, &destination);
        } else {
            fs::copy(&source, &destination).expect("copied");
        }
    }
}

/// The fixture's one account, its inbox thread and its message.
fn read_back(store: &LocalStore) -> (i64, i64) {
    let accounts = store.list_accounts().expect("accounts");
    assert_eq!(accounts.len(), 1);
    let account = accounts[0].id;
    assert_eq!(accounts[0].username, USERNAME);

    let inbox = store
        .mailbox_id_by_role(account, "inbox")
        .expect("lookup")
        .expect("an inbox");
    let threads = store.list_threads(inbox, 0..10).expect("threads");
    assert_eq!(threads.len(), 1);
    let view = store
        .thread_view(threads[0].id)
        .expect("reads")
        .expect("the thread");
    assert_eq!(view.subject, SUBJECT);
    let message = &view.messages[0];
    assert_eq!(message.body_text.as_deref(), Some(BODY));
    assert_eq!(message.attachments.len(), 1);
    assert_eq!(message.attachments[0].filename, ATTACHMENT_NAME);
    let bytes = store
        .attachment_blob(message.attachments[0].id)
        .expect("the attachment")
        .read()
        .expect("decrypts");
    assert_eq!(bytes, ATTACHMENT_BYTES.as_bytes());

    let hits = store
        .search_threads(account, SEARCH_TERM, 10)
        .expect("searches");
    assert_eq!(hits.len(), 1, "the search index survived");
    assert_eq!(hits[0].thread_id, view.id);

    let drafts = store.list_drafts(Some(account)).expect("drafts");
    let fixture_draft = drafts
        .iter()
        .find(|draft| draft.subject == DRAFT_SUBJECT)
        .expect("the fixture draft");
    let detail = store
        .draft_detail(fixture_draft.id)
        .expect("reads")
        .expect("the draft");
    assert_eq!(detail.body, DRAFT_BODY);
    assert_eq!(detail.attachments.len(), 1);
    assert_eq!(detail.attachments[0].filename, DRAFT_ATTACHMENT_NAME);

    (account, view.id)
}

#[test]
fn a_store_from_sqlcipher_4_14_opens_upgrades_and_stays_writable() {
    let directory = temp_dir("open");
    copy_dir(&fixture_dir(), &directory);
    let path = directory.join(DATABASE);

    let (account, thread) = {
        let store = LocalStore::open(&path, fixture_key()).expect("the old store opens");
        let (account, thread) = read_back(&store);

        // Writes after the upgrade: a flag, and a second draft.
        store.set_thread_flagged(thread, true).expect("flags");
        let identity = store.list_identities(account).expect("identities")[0].id;
        store
            .create_draft(&NewDraft {
                account_id: account,
                identity_id: identity,
                to: vec!["later@example.com".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: "Written after the upgrade".to_owned(),
                body: "Still writable".to_owned(),
            })
            .expect("a new draft");
        (account, thread)
    };

    // And again, from a fresh open.
    let store = LocalStore::open(&path, fixture_key()).expect("reopens");
    assert_eq!(read_back(&store), (account, thread));
    let state = store
        .thread_state(thread)
        .expect("reads")
        .expect("the thread");
    assert!(state.is_flagged());
    assert_eq!(store.list_drafts(Some(account)).expect("drafts").len(), 2);

    drop(store);
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn the_fixture_does_not_open_with_another_key() {
    let directory = temp_dir("wrong-key");
    copy_dir(&fixture_dir(), &directory);
    let wrong = StoreKey::from_fixture(KeyId::from_bytes([0x5a; 16]), [0x43; 32]);
    assert!(LocalStore::open(directory.join(DATABASE), wrong).is_err());
    let _ = fs::remove_dir_all(&directory);
}

/// Writes the fixture. Run by hand (see the module docs), never in CI.
#[test]
#[ignore = "writes tests/fixtures; run by hand when the store format changes"]
fn write_fixture() {
    if std::env::var_os("ZMAIL_WRITE_STORE_FIXTURE").is_none() {
        return;
    }
    let destination = fixture_dir();
    let _ = fs::remove_dir_all(&destination);
    let staging = temp_dir("write");
    let path = staging.join(DATABASE);
    {
        let store = LocalStore::open(&path, fixture_key()).expect("creates");
        let account = store
            .insert_account(&NewAccount {
                display_name: "Fixture".to_owned(),
                protocol: ProtocolKind::Imap,
                auth: AuthKind::Password,
                endpoint: "imaps://mail.example.com:993".to_owned(),
                username: USERNAME.to_owned(),
                cache_policy: CachePolicySummary::performance_first(),
                outgoing: Some(OutgoingConfig::smtp_default("mail.example.com")),
                identities: vec![NewIdentity {
                    display_name: "Fixture".to_owned(),
                    email_address: USERNAME.to_owned(),
                    signature: String::new(),
                    is_primary: true,
                }],
            })
            .expect("account");

        let target = store.sync_target(account).expect("target").expect("exists");
        let storage_path = target
            .blobs
            .write(ATTACHMENT_NAME, ATTACHMENT_BYTES.as_bytes())
            .expect("blob");
        let message = MessageSnapshot {
            remote_id: "m1".to_owned(),
            identity_email: USERNAME.to_owned(),
            sender: "Sender <sender@example.com>".to_owned(),
            reply_to: None,
            recipients: USERNAME.to_owned(),
            subject: SUBJECT.to_owned(),
            body_preview: BODY.to_owned(),
            body_text: Some(BODY.to_owned()),
            sanitized_html: None,
            raw_html: None,
            has_remote_content: false,
            is_read: false,
            is_flagged: false,
            received_at: "2026-10-01T09:00:00Z".to_owned(),
            attachments: vec![MessageAttachment {
                filename: ATTACHMENT_NAME.to_owned(),
                content_type: "application/pdf".to_owned(),
                size_bytes: ATTACHMENT_BYTES.len() as u64,
                storage_path,
                content_id: None,
                inline_only: false,
            }],
        };
        store
            .replace_account_snapshot(
                account,
                &AccountSyncSnapshot {
                    checkpoint: "fixture".to_owned(),
                    detail: "fixture".to_owned(),
                    identities: None,
                    mailboxes: vec![MailboxSnapshot {
                        name: "INBOX".to_owned(),
                        role: Some("inbox".to_owned()),
                        threads: vec![ThreadSnapshot {
                            remote_thread_id: "t1".to_owned(),
                            subject: SUBJECT.to_owned(),
                            participants: "Sender <sender@example.com>".to_owned(),
                            preview: BODY.to_owned(),
                            is_flagged: false,
                            has_attachments: true,
                            unread_count: 1,
                            latest_message_at: "2026-10-01T09:00:00Z".to_owned(),
                            messages: vec![message],
                        }],
                        ..Default::default()
                    }],
                },
            )
            .expect("snapshot");

        let outside = temp_dir("write-outside");
        let source = outside.join(DRAFT_ATTACHMENT_NAME);
        fs::write(&source, DRAFT_ATTACHMENT_BYTES).expect("written");
        let identity = store.list_identities(account).expect("identities")[0].id;
        let draft = store
            .create_draft(&NewDraft {
                account_id: account,
                identity_id: identity,
                to: vec!["friend@example.com".to_owned()],
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: DRAFT_SUBJECT.to_owned(),
                body: DRAFT_BODY.to_owned(),
            })
            .expect("draft");
        store
            .add_draft_attachment(draft, &source)
            .expect("attached");
        let _ = fs::remove_dir_all(&outside);
    }
    // The store's pooled connections stay open until the test ends, so the
    // data is still in the WAL: fold it into the database with the only
    // connection on a copy, as a clean close would, then keep the database
    // (still in WAL mode, like a user's) and the attachments, not the WAL,
    // its index or the lock file.
    let folded = temp_dir("write-folded");
    copy_dir(&staging, &folded);
    {
        let conn = rusqlite::Connection::open(folded.join(DATABASE)).expect("opens");
        conn.execute_batch(&format!("PRAGMA key = {};", *fixture_key().sqlcipher_key()))
            .expect("keyed");
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("checkpointed");
    }
    fs::create_dir_all(&destination).expect("created");
    fs::copy(folded.join(DATABASE), destination.join(DATABASE)).expect("copied");
    copy_dir(
        &folded.join("attachments"),
        &destination.join("attachments"),
    );
    let _ = fs::remove_dir_all(&staging);
    let _ = fs::remove_dir_all(&folded);
}
