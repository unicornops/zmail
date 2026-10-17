//! Compiles parsed search queries ([`zmail_core::search`]) into SQL over the
//! local store.
//!
//! Text clauses (`word`, `"phrase"`, `from:`, `to:`, `subject:`) become one
//! FTS5 `MATCH` expression against `search_documents`, one document per
//! thread. User text only ever reaches FTS5 inside a double-quoted string
//! with embedded quotes doubled, so FTS5 operators typed by the user (`OR`,
//! `NEAR`, `*`, `^`, column filters, parentheses) are matched as plain text
//! and never change the expression. State, mailbox and date clauses become
//! SQL conditions with bound parameters. Nothing typed by the user is ever
//! spliced into SQL text.
//!
//! Matching is per thread: `from:alice after:2026-01-01` finds threads that
//! have a message from Alice and a message on or after that day, not
//! necessarily the same message.

use chrono::{DateTime, NaiveDate, NaiveTime, TimeDelta, TimeZone};
use rusqlite::types::Value;
use zmail_core::search::{SearchQuery, SearchTerm, SearchText};

/// A query compiled to SQL. `sql` selects the [`zmail_core::SearchHit`]
/// columns in order; `params` binds its anonymous `?` placeholders.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompiledSearch {
    pub(crate) sql: String,
    pub(crate) params: Vec<Value>,
}

/// Compiles `query` for one account. `now` supplies today's date and the
/// time zone that `before:`/`after:` day boundaries are taken in.
///
/// Returns `None` when no clause is searchable (for example a query of
/// only punctuation), which callers treat as no results.
pub(crate) fn compile_search<Tz: TimeZone>(
    query: &SearchQuery,
    account_id: i64,
    limit: usize,
    now: &DateTime<Tz>,
) -> Option<CompiledSearch> {
    let today = now.date_naive();
    let zone = now.timezone();

    let mut include = Vec::new();
    let mut exclude = Vec::new();
    let mut conditions: Vec<String> = Vec::new();
    let mut condition_params: Vec<Value> = Vec::new();

    for clause in &query.clauses {
        let condition = match &clause.term {
            SearchTerm::Text(text) => fts_phrase(text),
            SearchTerm::From(text) => fts_phrase(text).map(|phrase| format!("senders : {phrase}")),
            SearchTerm::To(text) => fts_phrase(text).map(|phrase| format!("recipients : {phrase}")),
            SearchTerm::Subject(text) => {
                fts_phrase(text).map(|phrase| format!("subject : {phrase}"))
            }
            SearchTerm::HasAttachment => {
                push_condition(&mut conditions, clause.negated, "t.has_attachments = 1");
                None
            }
            SearchTerm::Unread => {
                push_condition(&mut conditions, clause.negated, "t.unread_count > 0");
                None
            }
            SearchTerm::Read => {
                push_condition(&mut conditions, clause.negated, "t.unread_count = 0");
                None
            }
            SearchTerm::Flagged => {
                push_condition(&mut conditions, clause.negated, "t.is_flagged = 1");
                None
            }
            SearchTerm::Mailbox(name) => {
                push_condition(
                    &mut conditions,
                    clause.negated,
                    "t.mailbox_id IN (SELECT id FROM mailboxes WHERE lower(name) = lower(?) \
                     OR lower(COALESCE(role, '')) = lower(?))",
                );
                condition_params.push(Value::Text(name.clone()));
                condition_params.push(Value::Text(mailbox_role_alias(name).to_owned()));
                None
            }
            SearchTerm::Before(spec) | SearchTerm::After(spec) => {
                let operator = if matches!(clause.term, SearchTerm::Before(_)) {
                    "<"
                } else {
                    ">="
                };
                // A boundary outside chrono's range cannot narrow anything.
                if let Some(boundary) = spec
                    .resolve(today)
                    .and_then(|day| day_start_unix(day, &zone))
                {
                    push_condition(
                        &mut conditions,
                        clause.negated,
                        &format!(
                            "EXISTS (SELECT 1 FROM messages dm WHERE dm.thread_id = t.id \
                             AND unixepoch(dm.received_at) {operator} ?)"
                        ),
                    );
                    condition_params.push(Value::Integer(boundary));
                }
                None
            }
        };
        if let Some(expression) = condition {
            if clause.negated {
                exclude.push(expression);
            } else {
                include.push(expression);
            }
        }
    }

    if include.is_empty() && exclude.is_empty() && conditions.is_empty() {
        return None;
    }

    let mut params = Vec::new();
    let mut sql = String::from(
        "SELECT t.mailbox_id, b.name, t.id, t.subject, m.sender, t.participants, t.preview, \
         t.unread_count, t.is_flagged, t.has_attachments, t.latest_message_at\n",
    );
    sql.push_str(
        "FROM threads t\n\
         CROSS JOIN mailboxes b ON b.id = t.mailbox_id\n\
         JOIN messages m ON m.id = t.latest_message_id\n\
         WHERE b.account_id = ?",
    );
    params.push(Value::Integer(account_id));
    if !include.is_empty() {
        sql.push_str(
            "\n  AND t.id IN (SELECT rowid FROM search_documents WHERE search_documents MATCH ?)",
        );
        params.push(Value::Text(include.join(" AND ")));
    }
    if !exclude.is_empty() {
        sql.push_str(
            "\n  AND t.id NOT IN (SELECT rowid FROM search_documents WHERE search_documents MATCH ?)",
        );
        params.push(Value::Text(exclude.join(" OR ")));
    }
    for condition in &conditions {
        sql.push_str("\n  AND ");
        sql.push_str(condition);
    }
    params.extend(condition_params);
    sql.push_str("\nORDER BY t.latest_message_at DESC, t.id DESC");
    sql.push_str("\nLIMIT ?");
    params.push(Value::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));

    Some(CompiledSearch { sql, params })
}

fn push_condition(conditions: &mut Vec<String>, negated: bool, condition: &str) {
    if negated {
        conditions.push(format!("NOT ({condition})"));
    } else {
        conditions.push(condition.to_owned());
    }
}

/// Quotes user text as an FTS5 string, doubling embedded quotes. Words
/// (unquoted text) match as prefixes; quoted text matches as a phrase.
///
/// Returns `None` for text with no letters or digits: it tokenizes to
/// nothing, so it cannot narrow a search.
pub(crate) fn fts_phrase(text: &SearchText) -> Option<String> {
    if !text.is_searchable() {
        return None;
    }
    let escaped = text.value.replace('"', "\"\"");
    Some(if text.phrase {
        format!("\"{escaped}\"")
    } else {
        format!("\"{escaped}\"*")
    })
}

/// The mailbox role an `in:` value also matches, for the common names
/// that differ from the stored role.
fn mailbox_role_alias(name: &str) -> &str {
    if name.eq_ignore_ascii_case("spam") {
        "junk"
    } else if name.eq_ignore_ascii_case("bin") {
        "trash"
    } else {
        name
    }
}

/// The Unix time at which `day` starts in `zone`. Days that skip midnight
/// (a daylight-saving jump) start at the first valid local time after it.
fn day_start_unix<Tz: TimeZone>(day: NaiveDate, zone: &Tz) -> Option<i64> {
    let midnight = day.and_time(NaiveTime::MIN);
    (0..=2)
        .find_map(|hours| {
            zone.from_local_datetime(&(midnight + TimeDelta::hours(hours)))
                .earliest()
        })
        .map(|start| start.timestamp())
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, Utc};

    use super::*;

    fn compile(query: &str) -> Option<CompiledSearch> {
        let now = Utc
            .with_ymd_and_hms(2026, 3, 31, 12, 0, 0)
            .single()
            .expect("valid time");
        compile_search(&SearchQuery::parse(query), 7, 50, &now)
    }

    fn text_params(compiled: &CompiledSearch) -> Vec<String> {
        compiled
            .params
            .iter()
            .filter_map(|value| match value {
                Value::Text(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn words_become_prefix_phrases_and_quotes_are_doubled() {
        let compiled = compile("report \"say \"\"hi\"\"\" o\"neil").expect("compiles");
        // The quoted value ends at the first closing quote, so `""hi""`
        // splits into separate clauses; every quote that reaches FTS5 is
        // doubled inside a string.
        let fts = text_params(&compiled)[0].clone();
        assert!(fts.starts_with("\"report\"* AND \"say\""));
        assert!(fts.ends_with("\"o\"\"neil\"*"));
        assert!(compiled.sql.contains("search_documents MATCH ?"));
    }

    #[test]
    fn fts_operators_in_user_text_stay_literal() {
        for hostile in [
            "OR",
            "NEAR(a b)",
            "subject:x",
            "a*",
            "^start",
            "(x",
            "{a b}:c",
            "\"",
            "x\" OR \"y",
        ] {
            let text = SearchText {
                value: hostile.to_owned(),
                phrase: false,
            };
            let Some(phrase) = fts_phrase(&text) else {
                continue;
            };
            // One string token: opens and closes with a quote, and every
            // quote inside is doubled.
            let inner = phrase
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix("\"*"))
                .expect("a quoted prefix string");
            assert_eq!(inner.replace("\"\"", ""), hostile.replace('"', ""));
        }
    }

    #[test]
    fn field_operators_use_fts_column_filters() {
        let compiled = compile("from:alice to:\"Bob Jones\" subject:invoice").expect("compiles");
        assert_eq!(
            text_params(&compiled),
            vec![
                "senders : \"alice\"* AND recipients : \"Bob Jones\" AND subject : \"invoice\"*"
                    .to_owned()
            ]
        );
    }

    #[test]
    fn negated_text_is_excluded_through_a_subquery() {
        let compiled = compile("-from:bob -spam").expect("compiles");
        assert!(!compiled.sql.contains("FROM search_documents JOIN"));
        assert!(
            compiled
                .sql
                .contains("NOT IN (SELECT rowid FROM search_documents")
        );
        assert_eq!(
            text_params(&compiled),
            vec!["senders : \"bob\"* OR \"spam\"*".to_owned()]
        );
        assert!(compiled.sql.contains("ORDER BY t.latest_message_at DESC"));
    }

    #[test]
    fn state_operators_become_conditions() {
        let compiled = compile("has:attachment is:unread -is:flagged is:read").expect("compiles");
        assert!(compiled.sql.contains("AND t.has_attachments = 1"));
        assert!(compiled.sql.contains("AND t.unread_count > 0"));
        assert!(compiled.sql.contains("AND NOT (t.is_flagged = 1)"));
        assert!(compiled.sql.contains("AND t.unread_count = 0"));
        // Only the account id and limit are bound.
        assert_eq!(compiled.params.len(), 2);
    }

    #[test]
    fn mailbox_values_are_bound_never_spliced() {
        let compiled = compile("in:\"x') OR 1=1 --\" in:spam").expect("compiles");
        assert!(!compiled.sql.contains("1=1"));
        assert_eq!(
            text_params(&compiled),
            vec![
                "x') OR 1=1 --".to_owned(),
                "x') OR 1=1 --".to_owned(),
                "spam".to_owned(),
                "junk".to_owned(),
            ]
        );
    }

    #[test]
    fn dates_bind_day_boundaries_in_the_viewer_zone() {
        let dublin_summer = FixedOffset::east_opt(3600).expect("valid offset");
        let now = dublin_summer
            .with_ymd_and_hms(2026, 6, 15, 9, 0, 0)
            .single()
            .expect("valid time");
        let compiled = compile_search(
            &SearchQuery::parse("after:2026-06-01 before:7d"),
            1,
            10,
            &now,
        )
        .expect("compiles");
        let integers = compiled
            .params
            .iter()
            .filter_map(|value| match value {
                Value::Integer(value) => Some(*value),
                _ => None,
            })
            .collect::<Vec<_>>();
        // account, after (2026-06-01T00:00+01:00), before (2026-06-08T00:00+01:00), limit.
        assert_eq!(integers, vec![1, 1_780_268_400, 1_780_873_200, 10]);
        assert!(compiled.sql.contains("unixepoch(dm.received_at) >= ?"));
        assert!(compiled.sql.contains("unixepoch(dm.received_at) < ?"));
    }

    #[test]
    fn placeholders_match_bound_parameters() {
        for query in [
            "word",
            "-word",
            "is:unread",
            "in:inbox from:a -to:b after:1d -before:2026-01-01 \"x y\" has:attachment",
        ] {
            let compiled = compile(query).expect("compiles");
            assert_eq!(
                compiled.sql.matches('?').count(),
                compiled.params.len(),
                "{query}"
            );
        }
    }

    #[test]
    fn unsearchable_queries_compile_to_nothing() {
        assert_eq!(compile(""), None);
        assert_eq!(compile("!!! \"...\" -??"), None);
        // An out-of-range relative date narrows nothing either.
        assert_eq!(compile("after:999999999y"), None);
    }
}

#[cfg(test)]
mod store_tests {
    use chrono::{FixedOffset, TimeZone, Utc};
    use zmail_core::{
        AccountSyncSnapshot, MailboxSnapshot, MessageAttachment, MessageSnapshot, ProtocolKind,
        ThreadSnapshot,
    };

    use crate::LocalStore;

    #[derive(Clone, Copy)]
    struct Mail {
        mailbox: &'static str,
        id: &'static str,
        from: &'static str,
        to: &'static str,
        subject: &'static str,
        body: Option<&'static str>,
        at: &'static str,
        unread: bool,
        flagged: bool,
        attachment: bool,
    }

    const MAIL: &[Mail] = &[
        Mail {
            mailbox: "Inbox",
            id: "budget",
            from: "Alice Smith <alice@example.com>",
            to: "ops@zmail.dev",
            subject: "Quarterly budget review",
            body: Some("Please review the budget spreadsheet before Friday."),
            at: "2026-03-30T09:00:00Z",
            unread: true,
            flagged: false,
            attachment: true,
        },
        Mail {
            mailbox: "Inbox",
            id: "standup",
            from: "Bob Jones <bob@example.com>",
            to: "team@example.com",
            subject: "Standup notes",
            body: Some("Notes: the review of the outage is done. OR NEAR stays literal."),
            at: "2026-03-10T09:00:00Z",
            unread: false,
            flagged: true,
            attachment: false,
        },
        Mail {
            mailbox: "Project X",
            id: "launch",
            from: "Carol <carol@partner.test>",
            to: "Alice Smith <alice@example.com>",
            subject: "Launch plan",
            // Not hydrated: only headers and the preview are indexed.
            body: None,
            at: "2025-12-24T18:30:00+01:00",
            unread: false,
            flagged: false,
            attachment: false,
        },
        Mail {
            mailbox: "Junk",
            id: "prize",
            from: "Prize Desk <win@spam.test>",
            to: "ops@zmail.dev",
            subject: "You won a budget prize",
            body: Some("Click here"),
            at: "2026-03-31T08:00:00Z",
            unread: true,
            flagged: false,
            attachment: false,
        },
    ];

    fn snapshot(mail: &[Mail]) -> AccountSyncSnapshot {
        let mut mailboxes: Vec<MailboxSnapshot> = Vec::new();
        for item in mail {
            let message = MessageSnapshot {
                remote_id: item.id.to_owned(),
                identity_email: "ops@zmail.dev".to_owned(),
                sender: item.from.to_owned(),
                reply_to: None,
                recipients: item.to.to_owned(),
                subject: item.subject.to_owned(),
                body_preview: "Body not downloaded.".to_owned(),
                body_text: item.body.map(str::to_owned),
                sanitized_html: None,
                has_remote_content: false,
                is_read: !item.unread,
                is_flagged: item.flagged,
                received_at: item.at.to_owned(),
                attachments: if item.attachment {
                    vec![MessageAttachment {
                        filename: "budget.xlsx".to_owned(),
                        content_type: "application/octet-stream".to_owned(),
                        size_bytes: 10,
                        storage_path: format!("{}.bin", item.id),
                    }]
                } else {
                    Vec::new()
                },
            };
            let thread = ThreadSnapshot {
                remote_thread_id: item.id.to_owned(),
                subject: item.subject.to_owned(),
                participants: item.from.to_owned(),
                preview: "Body not downloaded.".to_owned(),
                is_flagged: item.flagged,
                has_attachments: item.attachment,
                unread_count: usize::from(item.unread),
                latest_message_at: item.at.to_owned(),
                messages: vec![message],
            };
            match mailboxes
                .iter_mut()
                .find(|mailbox| mailbox.name == item.mailbox)
            {
                Some(mailbox) => mailbox.threads.push(thread),
                None => mailboxes.push(MailboxSnapshot {
                    name: item.mailbox.to_owned(),
                    role: match item.mailbox {
                        "Inbox" => Some("inbox".to_owned()),
                        "Junk" => Some("junk".to_owned()),
                        _ => None,
                    },
                    threads: vec![thread],
                }),
            }
        }
        AccountSyncSnapshot {
            checkpoint: "search".to_owned(),
            detail: "search fixture".to_owned(),
            identities: None,
            mailboxes,
        }
    }

    fn seeded() -> (LocalStore, i64) {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account_id = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        store
            .replace_account_snapshot(account_id, &snapshot(MAIL))
            .expect("snapshot should apply");
        (store, account_id)
    }

    /// Runs a search "now" (2026-03-31 12:00 UTC) and returns the matching
    /// subjects in result order.
    fn search(store: &LocalStore, account_id: i64, query: &str) -> Vec<String> {
        let now = Utc
            .with_ymd_and_hms(2026, 3, 31, 12, 0, 0)
            .single()
            .expect("valid time");
        let mut subjects = store
            .search_threads_at(account_id, query, 50, &now)
            .unwrap_or_else(|error| panic!("`{query}` should run: {error:#}"))
            .into_iter()
            .map(|hit| hit.subject)
            .collect::<Vec<_>>();
        subjects.sort();
        subjects
    }

    fn index_size(store: &LocalStore) -> (i64, i64) {
        let conn = store.connection().expect("connection should open");
        let documents = conn
            .query_row("SELECT COUNT(*) FROM search_documents", [], |row| {
                row.get(0)
            })
            .expect("documents should count");
        let threads = conn
            .query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))
            .expect("threads should count");
        (documents, threads)
    }

    #[test]
    fn words_match_prefixes_across_every_indexed_field() {
        let (store, account) = seeded();
        assert_eq!(
            search(&store, account, "budg"),
            vec!["Quarterly budget review", "You won a budget prize"]
        );
        // Body text of hydrated messages.
        assert_eq!(
            search(&store, account, "spreadsheet"),
            vec!["Quarterly budget review"]
        );
        // Sender addresses.
        assert_eq!(search(&store, account, "partner.test"), vec!["Launch plan"]);
        assert!(search(&store, account, "nothing-matches-this").is_empty());
    }

    #[test]
    fn phrases_match_words_in_order() {
        let (store, account) = seeded();
        assert_eq!(
            search(&store, account, "\"review the budget\""),
            vec!["Quarterly budget review"]
        );
        assert!(search(&store, account, "\"budget the review\"").is_empty());
        // A phrase does not prefix-match its last word.
        assert!(search(&store, account, "\"review the budg\"").is_empty());
    }

    #[test]
    fn field_operators_match_their_own_columns() {
        let (store, account) = seeded();
        assert_eq!(
            search(&store, account, "from:alice"),
            vec!["Quarterly budget review"]
        );
        assert_eq!(search(&store, account, "to:alice"), vec!["Launch plan"]);
        assert_eq!(
            search(&store, account, "from:\"Alice Smith\""),
            vec!["Quarterly budget review"]
        );
        assert_eq!(
            search(&store, account, "subject:budget"),
            vec!["Quarterly budget review", "You won a budget prize"]
        );
        // `review` is in the standup body but not its subject.
        assert_eq!(
            search(&store, account, "subject:review"),
            vec!["Quarterly budget review"]
        );
    }

    #[test]
    fn state_mailbox_and_date_operators_filter_threads() {
        let (store, account) = seeded();
        assert_eq!(
            search(&store, account, "is:unread"),
            vec!["Quarterly budget review", "You won a budget prize"]
        );
        assert_eq!(
            search(&store, account, "is:read"),
            vec!["Launch plan", "Standup notes"]
        );
        assert_eq!(search(&store, account, "is:flagged"), vec!["Standup notes"]);
        assert_eq!(
            search(&store, account, "has:attachment"),
            vec!["Quarterly budget review"]
        );
        assert_eq!(
            search(&store, account, "in:\"project x\""),
            vec!["Launch plan"]
        );
        assert_eq!(
            search(&store, account, "in:INBOX"),
            vec!["Quarterly budget review", "Standup notes"]
        );
        assert_eq!(
            search(&store, account, "in:spam"),
            vec!["You won a budget prize"]
        );
        assert_eq!(
            search(&store, account, "after:2026-03-30"),
            vec!["Quarterly budget review", "You won a budget prize"]
        );
        // 2025-12-24T18:30+01:00 is before 2026-01-01 in any zone.
        assert_eq!(
            search(&store, account, "before:2026-01-01"),
            vec!["Launch plan"]
        );
        // Relative: within the last week of 2026-03-31.
        assert_eq!(
            search(&store, account, "after:7d"),
            vec!["Quarterly budget review", "You won a budget prize"]
        );
        assert_eq!(
            search(&store, account, "before:2w"),
            vec!["Launch plan", "Standup notes"]
        );
    }

    #[test]
    fn day_boundaries_follow_the_viewer_zone() {
        let (store, account) = seeded();
        // 2026-03-30T09:00Z is still 29 March in UTC-10.
        let hawaii = FixedOffset::west_opt(10 * 3600).expect("valid offset");
        let now = hawaii
            .with_ymd_and_hms(2026, 3, 31, 0, 0, 0)
            .single()
            .expect("valid time");
        let hits = store
            .search_threads_at(account, "before:2026-03-30 after:2026-03-29", 10, &now)
            .expect("search should run");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subject, "Quarterly budget review");
    }

    #[test]
    fn negation_and_combinations() {
        let (store, account) = seeded();
        assert_eq!(
            search(&store, account, "budget -in:junk"),
            vec!["Quarterly budget review"]
        );
        assert_eq!(
            search(&store, account, "-from:alice -from:carol -in:spam"),
            vec!["Standup notes"]
        );
        assert_eq!(
            search(&store, account, "review -\"budget spreadsheet\""),
            vec!["Standup notes"]
        );
        assert_eq!(
            search(&store, account, "-is:unread -is:flagged"),
            vec!["Launch plan"]
        );
        assert_eq!(
            search(
                &store,
                account,
                "budget is:unread has:attachment in:inbox after:1w"
            ),
            vec!["Quarterly budget review"]
        );
    }

    #[test]
    fn fts_syntax_in_queries_is_matched_literally() {
        let (store, account) = seeded();
        // Every query must run without an FTS5 syntax error.
        for hostile in [
            "OR",
            "AND",
            "NOT",
            "NEAR(a b)",
            "\"",
            "*",
            "^",
            "(",
            ")",
            "a:b",
            "{subject}:x",
            "subject:\"",
            "x\"y",
            "-",
            "--",
            "\"OR\"",
            "from:)",
            "in:'",
            "-(\"",
        ] {
            search(&store, account, hostile);
        }
        // `OR` is a word, not an operator: only the standup body has it.
        assert_eq!(search(&store, account, "OR NEAR"), vec!["Standup notes"]);
        assert!(search(&store, account, "budget OR prize").is_empty());
    }

    #[test]
    fn searches_never_cross_accounts() {
        let (store, account) = seeded();
        let other = store
            .create_demo_account(ProtocolKind::Jmap)
            .expect("account should be created");
        assert!(search(&store, other, "budget").is_empty());
        assert!(search(&store, other, "is:unread").is_empty());
        assert_eq!(search(&store, account, "budget").len(), 2);
    }

    #[test]
    fn index_follows_moves_deletes_and_resyncs() {
        let (store, account) = seeded();
        assert_eq!(index_size(&store), (4, 4));

        let standup = store
            .search_threads(account, "standup", 1)
            .expect("search should run")[0]
            .thread_id;
        let project = store
            .list_mailboxes(account)
            .expect("mailboxes should load")
            .into_iter()
            .find(|mailbox| mailbox.name == "Project X")
            .expect("project mailbox should exist")
            .id;
        store
            .move_thread(standup, project)
            .expect("move should work");
        assert_eq!(
            search(&store, account, "in:\"Project X\""),
            vec!["Launch plan", "Standup notes"]
        );
        assert_eq!(search(&store, account, "outage"), vec!["Standup notes"]);
        assert_eq!(index_size(&store), (4, 4));

        // A resync without the junk mailbox drops its threads and documents.
        let kept = MAIL
            .iter()
            .filter(|mail| mail.mailbox != "Junk")
            .copied()
            .collect::<Vec<_>>();
        store
            .replace_account_snapshot(account, &snapshot(&kept))
            .expect("resync should apply");
        assert_eq!(index_size(&store), (3, 3));
        assert_eq!(
            search(&store, account, "budget"),
            vec!["Quarterly budget review"]
        );
        assert!(search(&store, account, "prize").is_empty());
    }

    #[test]
    fn index_survives_reopening_and_rebuilds_dirty_threads() {
        let (store, account) = seeded();
        {
            // Simulate an interrupted flush: a dirty thread with no document.
            let conn = store.connection().expect("connection should open");
            conn.execute_batch(
                "DELETE FROM search_documents; \
                 INSERT INTO search_dirty_threads(thread_id) SELECT id FROM threads;",
            )
            .expect("index should reset");
        }
        assert!(search(&store, account, "budget").is_empty());
        let reopened = LocalStore::open(store.path()).expect("store should reopen");
        assert_eq!(index_size(&reopened), (4, 4));
        assert_eq!(search(&reopened, account, "budget").len(), 2);
    }

    #[test]
    fn migration_rebuilds_a_v1_index() {
        let (store, account) = seeded();
        {
            // Roll the schema back to the per-account v1 index.
            let conn = store.connection().expect("connection should open");
            conn.execute_batch(
                r#"
                DROP TRIGGER search_dirty_thread_insert;
                DROP TRIGGER search_dirty_thread_update;
                DROP TRIGGER search_dirty_thread_delete;
                DROP TRIGGER search_dirty_message_insert;
                DROP TRIGGER search_dirty_message_update;
                DROP TRIGGER search_dirty_message_delete;
                DROP TABLE search_dirty_threads;
                DROP TABLE search_documents;
                CREATE VIRTUAL TABLE search_documents USING fts5(
                    account_id UNINDEXED, mailbox_id UNINDEXED, thread_id UNINDEXED,
                    mailbox_name UNINDEXED, subject, participants, preview, body
                );
                DELETE FROM schema_migrations WHERE version = '0014_search_index_v2';
                "#,
            )
            .expect("schema should roll back");
        }
        let reopened = LocalStore::open(store.path()).expect("store should migrate");
        assert_eq!(index_size(&reopened), (4, 4));
        assert_eq!(
            search(&reopened, account, "from:alice"),
            vec!["Quarterly budget review"]
        );
    }
}
