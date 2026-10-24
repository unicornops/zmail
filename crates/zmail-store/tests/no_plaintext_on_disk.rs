//! End to end through the store's public API (ADR 0005, #139): every way
//! mail reaches the disk (a sync snapshot, a hydrated body, received and
//! draft attachments, a draft in the outbox, the search index, a backup)
//! leaves no readable trace of it in any file under the data directory,
//! open or closed, and no attachment or mailbox name in any file name.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use zmail_core::{
    AccountSyncSnapshot, AuthKind, CachePolicySummary, HydratedBody, MailboxSnapshot,
    MessageAttachment, MessageSnapshot, NewAccount, NewDraft, NewIdentity, OutgoingConfig,
    ProtocolKind, ThreadSnapshot,
};
use zmail_security::InMemorySecretStore;
use zmail_store::LocalStore;

/// Text that appears only in the mail, each phrase in one place.
const SUBJECT: &str = "Subject-ocelot-umbrella";
const BODY: &str = "Body-marmalade-thunderclap";
const HYDRATED_BODY: &str = "Hydrated-lantern-pumpernickel";
const SENDER: &str = "sender.kestrel@example.com";
const MAILBOX: &str = "Folder-zeppelin";
const ATTACHMENT_NAME: &str = "Attachment-quokka.pdf";
const ATTACHMENT_BYTES: &str = "Attachment-bytes-gazpacho-nebula";
const HYDRATED_ATTACHMENT_BYTES: &str = "Hydrated-bytes-tamarind-orbit";
const DRAFT_BODY: &str = "Draft-body-cardamom-satellite";
const DRAFT_ATTACHMENT_NAME: &str = "Draft-attachment-wombat.txt";
const DRAFT_ATTACHMENT_BYTES: &str = "Draft-bytes-saffron-glacier";

const CONTENTS: &[&str] = &[
    SUBJECT,
    BODY,
    HYDRATED_BODY,
    SENDER,
    MAILBOX,
    ATTACHMENT_NAME,
    ATTACHMENT_BYTES,
    HYDRATED_ATTACHMENT_BYTES,
    DRAFT_BODY,
    DRAFT_ATTACHMENT_NAME,
    DRAFT_ATTACHMENT_BYTES,
];

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "zmail-no-plaintext-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("temp dir");
    directory
}

/// Every file under `directory`.
fn files(directory: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(directory).expect("listed").flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// Fails if any file under `directory` holds a phrase, or names one.
fn assert_no_plaintext(directory: &Path, when: &str) {
    let files = files(directory);
    assert!(!files.is_empty());
    for file in files {
        let bytes = fs::read(&file).expect("readable");
        let name = file
            .strip_prefix(directory)
            .expect("inside")
            .to_string_lossy()
            .into_owned();
        for phrase in CONTENTS {
            assert!(
                !bytes
                    .windows(phrase.len())
                    .any(|window| window == phrase.as_bytes()),
                "{when}: {name} contains {phrase:?}"
            );
            // A UTF-16 copy would hide from the byte search above.
            let wide: Vec<u8> = phrase.encode_utf16().flat_map(u16::to_le_bytes).collect();
            assert!(
                !bytes.windows(wide.len()).any(|window| window == wide),
                "{when}: {name} contains {phrase:?} as UTF-16"
            );
            assert!(
                !name.contains(phrase),
                "{when}: the file name {name} reveals {phrase:?}"
            );
        }
        assert!(
            !name.contains("incoming") && !name.contains("draft-"),
            "{when}: {name} looks like a plaintext-era name"
        );
    }
}

fn message(
    remote_id: &str,
    body: Option<&str>,
    attachments: Vec<MessageAttachment>,
) -> MessageSnapshot {
    MessageSnapshot {
        remote_id: remote_id.to_owned(),
        identity_email: "me@example.com".to_owned(),
        sender: format!("Kestrel <{SENDER}>"),
        reply_to: None,
        recipients: "me@example.com".to_owned(),
        subject: SUBJECT.to_owned(),
        body_preview: body.unwrap_or("Body not downloaded.").to_owned(),
        body_text: body.map(str::to_owned),
        sanitized_html: None,
        raw_html: None,
        has_remote_content: false,
        is_read: false,
        is_flagged: false,
        received_at: format!("2026-10-01T09:0{}:00Z", remote_id.len() % 10),
        attachments,
    }
}

#[test]
fn mail_never_reaches_the_disk_in_plaintext() {
    let directory = temp_dir("data");
    let path = directory.join("zmail.sqlite3");
    let keychain = InMemorySecretStore::default();
    let (store, report) = LocalStore::unlock(&path, &keychain, &|_| {}).expect("unlocks");
    assert_eq!(report, None);

    let account = store
        .insert_account(&NewAccount {
            display_name: "Work".to_owned(),
            protocol: ProtocolKind::Imap,
            auth: AuthKind::Password,
            endpoint: "imaps://mail.example.com:993".to_owned(),
            username: "me@example.com".to_owned(),
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: Some(OutgoingConfig::smtp_default("mail.example.com")),
            identities: vec![NewIdentity {
                display_name: "Me".to_owned(),
                email_address: "me@example.com".to_owned(),
                signature: String::new(),
                is_primary: true,
            }],
        })
        .expect("account");

    // A synced message with a body and an attachment, written through the
    // sync target's blob store as the IMAP and JMAP adapters do (with the
    // mailbox and filename in the name they ask for).
    let target = store.sync_target(account).expect("target").expect("exists");
    let storage_path = target
        .blobs
        .write(
            &format!("incoming-{MAILBOX}-1-0-{ATTACHMENT_NAME}"),
            ATTACHMENT_BYTES.as_bytes(),
        )
        .expect("blob");
    let attachment = MessageAttachment {
        filename: ATTACHMENT_NAME.to_owned(),
        content_type: "application/pdf".to_owned(),
        size_bytes: ATTACHMENT_BYTES.len() as u64,
        storage_path,
        content_id: None,
        inline_only: false,
    };
    let snapshot = AccountSyncSnapshot {
        checkpoint: "seed".to_owned(),
        detail: "seed".to_owned(),
        identities: None,
        mailboxes: vec![MailboxSnapshot {
            name: MAILBOX.to_owned(),
            role: Some("inbox".to_owned()),
            threads: vec![
                ThreadSnapshot {
                    remote_thread_id: "t1".to_owned(),
                    subject: SUBJECT.to_owned(),
                    participants: format!("Kestrel <{SENDER}>"),
                    preview: BODY.to_owned(),
                    is_flagged: false,
                    has_attachments: true,
                    unread_count: 1,
                    latest_message_at: "2026-10-01T09:02:00Z".to_owned(),
                    messages: vec![message("m1", Some(BODY), vec![attachment])],
                },
                // Headers only, as the lazy IMAP sync stores it; hydrated below.
                ThreadSnapshot {
                    remote_thread_id: "t2".to_owned(),
                    subject: SUBJECT.to_owned(),
                    participants: format!("Kestrel <{SENDER}>"),
                    preview: "Body not downloaded.".to_owned(),
                    is_flagged: false,
                    has_attachments: false,
                    unread_count: 1,
                    latest_message_at: "2026-10-01T09:03:00Z".to_owned(),
                    messages: vec![message("lazy-m2", None, Vec::new())],
                },
            ],
            ..Default::default()
        }],
    };
    store
        .replace_account_snapshot(account, &snapshot)
        .expect("snapshot");

    // Hydrating the header-only message, with an attachment of its own.
    let inbox = store
        .mailbox_id_by_role(account, "inbox")
        .expect("lookup")
        .expect("inbox");
    let lazy = store
        .list_threads(inbox, 0..10)
        .expect("threads")
        .into_iter()
        .find_map(|thread| {
            store
                .messages_missing_bodies(thread.id)
                .expect("reads")
                .and_then(|missing| missing.first().map(|(id, _)| *id))
        })
        .expect("one message has no body");
    let hydrated_path = target
        .blobs
        .write(
            &format!("incoming-{MAILBOX}-2-0-{ATTACHMENT_NAME}"),
            HYDRATED_ATTACHMENT_BYTES.as_bytes(),
        )
        .expect("blob");
    store
        .hydrate_message_body(
            lazy,
            &HydratedBody {
                body_text: Some(HYDRATED_BODY.to_owned()),
                sanitized_html: None,
                raw_html: None,
                has_remote_content: false,
                body_preview: HYDRATED_BODY.to_owned(),
                attachments: vec![MessageAttachment {
                    filename: ATTACHMENT_NAME.to_owned(),
                    content_type: "application/pdf".to_owned(),
                    size_bytes: HYDRATED_ATTACHMENT_BYTES.len() as u64,
                    storage_path: hydrated_path,
                    content_id: None,
                    inline_only: false,
                }],
            },
        )
        .expect("hydrates");

    // A draft with an attachment, queued in the outbox.
    let outside = temp_dir("outside");
    let source = outside.join(DRAFT_ATTACHMENT_NAME);
    fs::write(&source, DRAFT_ATTACHMENT_BYTES).expect("written");
    let identity = store.list_identities(account).expect("identities")[0].id;
    let draft = store
        .create_draft(&NewDraft {
            account_id: account,
            identity_id: identity,
            to: vec!["bob@example.com".to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: SUBJECT.to_owned(),
            body: DRAFT_BODY.to_owned(),
        })
        .expect("draft");
    store
        .add_draft_attachment(draft, &source)
        .expect("attached");
    store.queue_draft(draft).expect("queued");

    // The search index has every word, and finds them.
    for phrase in [BODY, HYDRATED_BODY] {
        assert_eq!(
            store
                .search_threads(account, phrase, 10)
                .expect("search")
                .len(),
            1,
            "{phrase}"
        );
    }
    store
        .create_backup(directory.join("backups"), 2)
        .expect("backup");

    // While the store is open, with recent pages still in the WAL.
    assert_no_plaintext(&directory, "open");
    drop(store);
    // And once it is closed and checkpointed.
    assert_no_plaintext(&directory, "closed");

    // Nothing was lost along the way.
    let (store, _) = LocalStore::unlock(&path, &keychain, &|_| {}).expect("reopens");
    let thread = store
        .search_threads(account, HYDRATED_BODY, 10)
        .expect("search")[0]
        .thread_id;
    let view = store.thread_view(thread).expect("loads").expect("exists");
    let attachment = &view.messages[0].attachments[0];
    assert_eq!(attachment.filename, ATTACHMENT_NAME);
    assert_eq!(
        store
            .attachment_blob(attachment.id)
            .expect("blob")
            .read()
            .expect("decrypts"),
        HYDRATED_ATTACHMENT_BYTES.as_bytes()
    );
    drop(store);
    let _ = fs::remove_dir_all(directory);
    let _ = fs::remove_dir_all(outside);
}

/// The scan is not vacuous: it finds a phrase in a file, in UTF-16, and in
/// a file name.
#[test]
fn the_scan_finds_plaintext() {
    let planted = |write: &dyn Fn(&Path)| {
        let directory = temp_dir("planted");
        write(&directory);
        let found = std::panic::catch_unwind(|| assert_no_plaintext(&directory, "planted"));
        let _ = fs::remove_dir_all(&directory);
        found.is_err()
    };
    assert!(planted(&|directory| {
        fs::write(directory.join("a"), format!("xx{BODY}xx")).expect("written");
    }));
    assert!(planted(&|directory| {
        let wide: Vec<u8> = SENDER.encode_utf16().flat_map(u16::to_le_bytes).collect();
        fs::write(directory.join("a"), wide).expect("written");
    }));
    assert!(planted(&|directory| {
        fs::write(directory.join(ATTACHMENT_NAME), b"x").expect("written");
    }));
    assert!(!planted(&|directory| {
        fs::write(directory.join("a"), b"nothing to see").expect("written");
    }));
}
