//! Search over synthetic mailboxes.
//!
//! `operators_match_a_brute_force_oracle` checks every operator against an
//! independent, in-memory evaluation of the generated data. The ignored
//! `large_mailbox_search_timings` run measures first-page latency and the
//! indexing hot paths over about 100k messages (the #137 perf budget is a
//! first search page under 100 ms):
//!
//! ```sh
//! cargo test --release -p zmail-test-support --test search_scale -- --ignored --nocapture
//! ```
//!
//! `ZMAIL_SEARCH_SCALE_THREADS` overrides the threads per mailbox (four
//! mailboxes of three-message threads).

use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use zmail_core::{
    AccountSyncSnapshot, HydratedBody, ProtocolKind, ThreadSnapshot,
    search::{SearchQuery, SearchTerm, SearchText, day_start_unix, mailbox_matches},
};
use zmail_store::LocalStore;
use zmail_test_support::SyntheticMailboxSpec;

/// "Now" for every query: dates in the dataset run back two years from it.
fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0)
        .single()
        .expect("valid time")
}

fn seeded(
    spec: SyntheticMailboxSpec,
    hydrated_every: usize,
) -> Result<(LocalStore, i64, AccountSyncSnapshot)> {
    let store = LocalStore::open_ephemeral()?;
    let account_id = store.create_demo_account(ProtocolKind::Imap)?;
    let snapshot = spec.snapshot(7, hydrated_every);
    store.replace_account_snapshot(account_id, &snapshot)?;
    Ok((store, account_id, snapshot))
}

/// One thread as the oracle sees it: the indexed columns as token lists,
/// and the metadata the filters read.
struct OracleThread {
    subject: String,
    mailbox: String,
    role: Option<String>,
    subject_tokens: Vec<String>,
    participants: Vec<String>,
    preview: Vec<String>,
    body: Vec<String>,
    senders: Vec<String>,
    recipients: Vec<String>,
    unread: bool,
    flagged: bool,
    attachment: bool,
    times: Vec<i64>,
}

/// ASCII tokenization equivalent to FTS5's `unicode61` for this data:
/// runs of letters and digits, lowercased.
fn tokens(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn joined<'a>(values: impl Iterator<Item = &'a str>) -> Vec<String> {
    tokens(&values.collect::<Vec<_>>().join(" "))
}

fn oracle_threads(snapshot: &AccountSyncSnapshot) -> Vec<OracleThread> {
    let mut threads = Vec::new();
    for mailbox in &snapshot.mailboxes {
        for thread in &mailbox.threads {
            threads.push(oracle_thread(&mailbox.name, mailbox.role.clone(), thread));
        }
    }
    threads
}

fn oracle_thread(mailbox: &str, role: Option<String>, thread: &ThreadSnapshot) -> OracleThread {
    let messages = &thread.messages;
    OracleThread {
        subject: thread.subject.clone(),
        mailbox: mailbox.to_owned(),
        role,
        subject_tokens: tokens(&thread.subject),
        participants: tokens(&thread.participants),
        preview: tokens(&thread.preview),
        // The index stores the body, or the preview of a headers-only
        // message, in message order.
        body: joined(messages.iter().map(|message| {
            message
                .body_text
                .as_deref()
                .unwrap_or(message.body_preview.as_str())
        })),
        senders: joined(messages.iter().map(|message| message.sender.as_str())),
        recipients: joined(messages.iter().map(|message| message.recipients.as_str())),
        unread: thread.unread_count > 0,
        flagged: thread.is_flagged,
        attachment: thread.has_attachments,
        times: messages
            .iter()
            .filter_map(|message| DateTime::parse_from_rfc3339(&message.received_at).ok())
            .map(|time| time.timestamp())
            .collect(),
    }
}

/// Whether `column` contains the text's tokens consecutively; an unquoted
/// word's last token may be a prefix.
fn column_matches(column: &[String], text: &SearchText) -> bool {
    let wanted = tokens(&text.value);
    if wanted.is_empty() || wanted.len() > column.len() {
        return false;
    }
    column.windows(wanted.len()).any(|window| {
        window
            .iter()
            .zip(&wanted)
            .enumerate()
            .all(|(index, (have, want))| {
                if !text.phrase && index == wanted.len() - 1 {
                    have.starts_with(want.as_str())
                } else {
                    have == want
                }
            })
    })
}

/// Evaluates a query against the oracle threads, returning the matching
/// subjects (each thread's subject is unique through its `T<n>` tag).
fn oracle(threads: &[OracleThread], query: &str) -> BTreeSet<String> {
    let parsed = SearchQuery::parse(query);
    let today = now().date_naive();
    threads
        .iter()
        .filter(|thread| {
            parsed.clauses.iter().all(|clause| {
                let matched = match &clause.term {
                    SearchTerm::Text(text) if !text.is_searchable() => return true,
                    SearchTerm::From(text) | SearchTerm::To(text) | SearchTerm::Subject(text)
                        if !text.is_searchable() =>
                    {
                        return true;
                    }
                    SearchTerm::Text(text) => [
                        &thread.subject_tokens,
                        &thread.participants,
                        &thread.preview,
                        &thread.body,
                        &thread.senders,
                        &thread.recipients,
                    ]
                    .iter()
                    .any(|column| column_matches(column, text)),
                    SearchTerm::From(text) => column_matches(&thread.senders, text),
                    SearchTerm::To(text) => column_matches(&thread.recipients, text),
                    SearchTerm::Subject(text) => column_matches(&thread.subject_tokens, text),
                    SearchTerm::HasAttachment => thread.attachment,
                    SearchTerm::Unread => thread.unread,
                    SearchTerm::Read => !thread.unread,
                    SearchTerm::Flagged => thread.flagged,
                    SearchTerm::Mailbox(value) => {
                        mailbox_matches(value, &thread.mailbox, thread.role.as_deref())
                    }
                    SearchTerm::Before(spec) | SearchTerm::After(spec) => {
                        let Some(boundary) = spec
                            .resolve(today)
                            .and_then(|day| day_start_unix(day, &Utc))
                        else {
                            return true;
                        };
                        let before = matches!(clause.term, SearchTerm::Before(_));
                        thread.times.iter().any(|time| {
                            if before {
                                *time < boundary
                            } else {
                                *time >= boundary
                            }
                        })
                    }
                };
                matched != clause.negated
            })
        })
        .map(|thread| thread.subject.clone())
        .collect()
}

fn store_results(store: &LocalStore, account_id: i64, query: &str) -> Result<BTreeSet<String>> {
    Ok(store
        .search_threads_at(account_id, query, 1_000_000, &now())?
        .into_iter()
        .map(|hit| hit.subject)
        .collect())
}

const QUERIES: &[&str] = &[
    "budget",
    "kittiwake",
    "budget review",
    "\"please attached\"",
    "mig",
    "T12",
    "\"T12\"",
    "from:alice",
    "from:\"alice smith\"",
    "to:partner",
    "subject:launch",
    "subject:launch -subject:review",
    "is:unread",
    "is:read",
    "is:flagged has:attachment",
    "in:inbox budget",
    "-in:inbox is:flagged",
    "in:Synthetic/002 -kittiwake",
    "after:2026-01-01",
    "before:2025-06-01 subject:deploy",
    "after:30d",
    "after:1y before:6m from:kai",
    "budget -from:bob -is:read",
    "-subject:budget -subject:review -is:read",
    "subject:report -kittiwake",
];

/// What the live-search timing types, one character at a time.
const LIVE_TYPING: &[&str] = &["budget review", "from:alice", "kittiwake"];

/// The page a live search reads (the UI's first page of results).
const LIVE_PAGE: usize = 100;

#[test]
fn operators_match_a_brute_force_oracle() -> Result<()> {
    let spec = SyntheticMailboxSpec {
        mailbox_count: 3,
        threads_per_mailbox: 300,
        messages_per_thread: 3,
        average_body_bytes: 400,
    };
    // Every third message hydrated: the rest are indexed by preview only.
    let (store, account_id, snapshot) = seeded(spec, 3)?;
    let threads = oracle_threads(&snapshot);
    for query in QUERIES {
        let expected = oracle(&threads, query);
        let actual = store_results(&store, account_id, query)?;
        assert!(
            !expected.is_empty() && expected.len() < threads.len() || query.starts_with("is:"),
            "{query}: the dataset should give a selective query, got {} of {}",
            expected.len(),
            threads.len()
        );
        assert_eq!(
            actual,
            expected,
            "{query}: {} from the store, {} from the oracle",
            actual.len(),
            expected.len()
        );
    }
    Ok(())
}

#[test]
#[ignore = "large-mailbox timing run; use --release"]
fn large_mailbox_search_timings() -> Result<()> {
    let threads_per_mailbox = std::env::var("ZMAIL_SEARCH_SCALE_THREADS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8_334);
    let spec = SyntheticMailboxSpec {
        mailbox_count: 4,
        threads_per_mailbox,
        messages_per_thread: 3,
        average_body_bytes: 600,
    };
    let started = Instant::now();
    // A quarter of the bodies hydrated, as after some reading and indexing.
    let (store, account_id, _) = seeded(spec, 4)?;
    println!(
        "seeded {} messages in {} threads in {:.1?}",
        spec.total_messages(),
        spec.mailbox_count * spec.threads_per_mailbox,
        started.elapsed()
    );

    println!("\n| first page (50) | hits | cold | warm (median of 5) |");
    println!("|---|---|---|---|");
    let mut slowest = Duration::ZERO;
    for query in QUERIES.iter().chain(&["nothing-matches-this"]) {
        let timed = || -> Result<(usize, Duration)> {
            let started = Instant::now();
            let hits = store.search_threads_at(account_id, query, 50, &now())?;
            Ok((hits.len(), started.elapsed()))
        };
        let (hits, cold) = timed()?;
        let mut warm = (0..5)
            .map(|_| timed().map(|(_, elapsed)| elapsed))
            .collect::<Result<Vec<_>>>()?;
        warm.sort();
        slowest = slowest.max(warm[2]);
        println!("| `{query}` | {hits} | {cold:.1?} | {:.1?} |", warm[2]);
    }

    // Live search runs on every pause in typing, so every prefix of a
    // query is searched in turn: short prefixes match the most threads.
    println!("\n| live typing (first {LIVE_PAGE}) | hits | warm (median of 5) |");
    println!("|---|---|---|");
    for typed in LIVE_TYPING {
        for end in typed
            .char_indices()
            .map(|(index, _)| index)
            .skip(1)
            .chain([typed.len()])
        {
            let query = &typed[..end];
            let timed = || -> Result<(usize, Duration)> {
                let started = Instant::now();
                let hits = store.search_threads_at(account_id, query, LIVE_PAGE, &now())?;
                Ok((hits.len(), started.elapsed()))
            };
            let mut runs = (0..5).map(|_| timed()).collect::<Result<Vec<_>>>()?;
            runs.sort_by_key(|(_, elapsed)| *elapsed);
            let (hits, warm) = runs[2];
            slowest = slowest.max(warm);
            println!("| `{query}` | {hits} | {warm:.1?} |");
        }
    }

    // Scrolling the results reads one more page each time, re-reading
    // the pages before it (the read is one ordered query with a growing
    // limit).
    println!("\n| paging (pages of {LIVE_PAGE}) | results | warm (median of 5) |");
    println!("|---|---|---|");
    for query in ["b", "budget", "from:alice", "is:unread"] {
        for pages in [1, 2, 5, 10] {
            let limit = pages * LIVE_PAGE;
            let timed = || -> Result<(usize, Duration)> {
                let started = Instant::now();
                let hits = store.search_threads_at(account_id, query, limit, &now())?;
                Ok((hits.len(), started.elapsed()))
            };
            let mut runs = (0..5).map(|_| timed()).collect::<Result<Vec<_>>>()?;
            runs.sort_by_key(|(_, elapsed)| *elapsed);
            let (hits, warm) = runs[2];
            slowest = slowest.max(warm);
            println!("| `{query}` × {pages} | {hits} | {warm:.1?} |");
        }
    }

    // A page's body excerpts: the stored bodies of the results whose own
    // snippet does not show the match (at most a page of them).
    let page = store
        .search_threads_at(account_id, "kittiwake", LIVE_PAGE, &now())?
        .into_iter()
        .map(|hit| hit.thread_id)
        .collect::<Vec<_>>();
    let mut runs = (0..5)
        .map(|_| -> Result<(usize, Duration)> {
            let started = Instant::now();
            let texts = store.thread_body_texts(&page)?;
            Ok((texts.len(), started.elapsed()))
        })
        .collect::<Result<Vec<_>>>()?;
    runs.sort_by_key(|(_, elapsed)| *elapsed);
    let (bodies, body_read) = runs[2];
    slowest = slowest.max(body_read);

    // Indexing hot paths: queue lookups and per-message hydration, which
    // rebuild one thread document each.
    let started = Instant::now();
    let candidates = store.index_candidates(account_id, 25, None)?;
    let queue = started.elapsed();
    let started = Instant::now();
    let needs_server = store.search_needs_server(account_id, "budget")?;
    let needs = started.elapsed();
    let started = Instant::now();
    for candidate in &candidates {
        store.hydrate_message_body(
            candidate.message_id,
            &HydratedBody {
                body_text: Some("hydrated body with a fresh skua".to_owned()),
                sanitized_html: None,
                has_remote_content: false,
                body_preview: "hydrated body".to_owned(),
                attachments: Vec::new(),
            },
        )?;
    }
    let hydrate = started.elapsed() / candidates.len().max(1) as u32;
    println!("\n| operation | time |");
    println!("|---|---|");
    println!(
        "| `thread_body_texts` ({} threads, {bodies} bodies) | {body_read:.1?} |",
        page.len()
    );
    println!("| `index_candidates` (25) | {queue:.1?} |");
    println!("| `search_needs_server` ({needs_server}) | {needs:.1?} |");
    println!(
        "| `hydrate_message_body` (mean of {}) | {hydrate:.1?} |",
        candidates.len()
    );
    // Several candidates can share a thread; every hydrated one is found.
    let threads = store
        .search_threads_at(account_id, "skua", 50, &now())?
        .len();
    assert!(threads > 0 && threads <= candidates.len());

    // A loose ceiling that only trips on pathological regressions (such as
    // a full index scan per write); the budget itself is tracked in #137.
    assert!(
        slowest < Duration::from_secs(1),
        "slowest warm query took {slowest:?}"
    );
    assert!(
        hydrate < Duration::from_millis(250),
        "hydration took {hydrate:?}"
    );
    Ok(())
}
