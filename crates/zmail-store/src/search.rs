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

use chrono::{DateTime, TimeZone};
use rusqlite::types::Value;
use zmail_core::search::{SearchQuery, SearchTerm, SearchText, day_start_unix, mailbox_role_alias};

/// A query compiled to SQL. `sql` selects the [`zmail_core::SearchHit`]
/// columns in order; `params` binds its anonymous `?` placeholders.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompiledSearch {
    pub(crate) sql: String,
    pub(crate) params: Vec<Value>,
}

/// Which threads a compiled query looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchScope<'a> {
    /// The local index: every clause is matched locally.
    Local,
    /// The threads the server search recorded for this query text. Free
    /// text is the server's verdict (the local index may lack the bodies),
    /// so only the other clauses are re-checked locally, exactly.
    ServerHits(&'a str),
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
    scope: SearchScope<'_>,
) -> Option<CompiledSearch> {
    let today = now.date_naive();
    let zone = now.timezone();

    let mut include = Vec::new();
    let mut exclude = Vec::new();
    let mut conditions: Vec<String> = Vec::new();
    let mut condition_params: Vec<Value> = Vec::new();

    if let SearchScope::ServerHits(query_text) = scope {
        conditions.push(
            "t.id IN (SELECT thread_id FROM server_search_hits WHERE account_id = ? AND query = ?)"
                .to_owned(),
        );
        condition_params.push(Value::Integer(account_id));
        condition_params.push(Value::Text(query_text.to_owned()));
    }

    for clause in &query.clauses {
        let condition = match &clause.term {
            SearchTerm::Text(_) if matches!(scope, SearchScope::ServerHits(_)) => None,
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

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, Utc};

    use super::*;

    fn compile(query: &str) -> Option<CompiledSearch> {
        let now = Utc
            .with_ymd_and_hms(2026, 3, 31, 12, 0, 0)
            .single()
            .expect("valid time");
        compile_search(&SearchQuery::parse(query), 7, 50, &now, SearchScope::Local)
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
            SearchScope::Local,
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
                    ..Default::default()
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

    fn hydrate(store: &LocalStore, message_id: i64, bytes: usize) {
        store
            .hydrate_message_body(
                message_id,
                &zmail_core::HydratedBody {
                    body_text: Some("x".repeat(bytes)),
                    sanitized_html: None,
                    has_remote_content: false,
                    body_preview: "x".to_owned(),
                    attachments: Vec::new(),
                },
            )
            .expect("hydration should persist");
    }

    fn lazy_mail() -> Vec<Mail> {
        [
            ("m1", "2026-03-01T10:00:00Z"),
            ("m2", "2026-03-02T10:00:00Z"),
            ("m3", "2026-03-03T10:00:00Z"),
            ("m4", "2026-03-04T10:00:00Z"),
        ]
        .into_iter()
        .map(|(id, at)| Mail {
            mailbox: "Inbox",
            id,
            from: "a@example.com",
            to: "ops@zmail.dev",
            subject: id,
            body: None,
            at,
            unread: true,
            flagged: false,
            attachment: false,
        })
        .collect()
    }

    fn queue(store: &LocalStore, account: i64, budget: Option<u64>) -> Vec<String> {
        store
            .index_candidates(account, 10, budget)
            .expect("queue should load")
            .into_iter()
            .map(|candidate| candidate.remote_id)
            .collect()
    }

    #[test]
    fn indexing_queue_is_newest_first_and_skips_failures() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        store
            .replace_account_snapshot(account, &snapshot(&lazy_mail()))
            .expect("snapshot should apply");
        assert_eq!(queue(&store, account, None), vec!["m4", "m3", "m2", "m1"]);

        let candidates = store
            .index_candidates(account, 10, None)
            .expect("queue should load");
        hydrate(&store, candidates[0].message_id, 10);
        // Transient failures retry until the limit; permanent ones skip at once.
        for _ in 0..crate::MAX_BODY_FETCH_FAILURES {
            assert!(queue(&store, account, None).contains(&"m3".to_owned()));
            store
                .record_body_fetch_failure(candidates[1].message_id, false)
                .expect("failure should record");
        }
        store
            .record_body_fetch_failure(candidates[2].message_id, true)
            .expect("failure should record");
        assert_eq!(queue(&store, account, None), vec!["m1"]);
        assert_eq!(
            store
                .index_candidates(account, 0, None)
                .expect("queue")
                .len(),
            0
        );

        let progress = store.index_progress(account).expect("progress should load");
        assert_eq!(
            progress,
            zmail_core::IndexProgress {
                messages: 4,
                indexed: 1,
                skipped: 2,
            }
        );
        assert_eq!(progress.pending(), 1);

        // Hydrated bodies make their threads searchable.
        assert_eq!(search(&store, account, "xxxxx"), vec!["m4"]);
    }

    #[test]
    fn a_full_budget_only_admits_mail_newer_than_the_oldest_body() {
        let store = LocalStore::open_ephemeral().expect("store should open");
        let account = store
            .create_demo_account(ProtocolKind::Imap)
            .expect("account should be created");
        store
            .replace_account_snapshot(account, &snapshot(&lazy_mail()))
            .expect("snapshot should apply");
        let by_id = store
            .index_candidates(account, 10, None)
            .expect("queue should load")
            .into_iter()
            .map(|candidate| (candidate.remote_id, candidate.message_id))
            .collect::<std::collections::HashMap<_, _>>();

        // m2 is stored and fills the budget: only m3 and m4 may displace it.
        hydrate(&store, by_id["m2"], 100);
        assert_eq!(queue(&store, account, Some(100)), vec!["m4", "m3"]);
        // Below the budget everything is admitted.
        assert_eq!(queue(&store, account, Some(1_000)), vec!["m4", "m3", "m1"]);

        // Fetching m4 and evicting back to the budget drops m2, the oldest.
        hydrate(&store, by_id["m4"], 100);
        store
            .evict_account_blobs(account, 100)
            .expect("eviction should run");
        assert!(!store.message_has_body(by_id["m2"]).expect("check"));
        assert!(store.message_has_body(by_id["m4"]).expect("check"));
        // m4 is now the oldest (and only) body, so nothing older is fetched
        // again and the queue settles instead of churning.
        assert!(queue(&store, account, Some(100)).is_empty());
    }

    fn server_hit(mailbox: Option<&str>, remote_id: &str) -> zmail_core::ServerHitRef {
        zmail_core::ServerHitRef {
            mailbox_name: mailbox.map(str::to_owned),
            remote_id: remote_id.to_owned(),
        }
    }

    #[test]
    fn server_hits_join_local_results_marked_and_filtered() {
        let (store, account) = seeded();
        assert!(store.search_needs_server(account, "rocket").expect("check"));
        assert!(
            !store
                .search_needs_server(account, "from:alice is:unread")
                .expect("check")
        );
        assert!(search(&store, account, "rocket").is_empty());

        // The server found `rocket` in the unhydrated launch body; `nope`
        // is mail the store has not synced yet.
        let unknown = store
            .record_server_search_hits(
                account,
                " rocket ",
                &[
                    server_hit(Some("Project X"), "launch"),
                    server_hit(Some("Inbox"), "launch"),
                    server_hit(None, "nope"),
                ],
            )
            .expect("hits should record");
        assert_eq!(
            unknown,
            vec![
                server_hit(Some("Inbox"), "launch"),
                server_hit(None, "nope")
            ]
        );
        let hits = store
            .search_threads(account, "rocket", 10)
            .expect("search should run");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subject, "Launch plan");
        assert!(hits[0].from_server);

        // Server hits only belong to the query text that produced them.
        assert!(search(&store, account, "rockets").is_empty());

        // Non-text clauses are re-checked locally.
        store
            .record_server_search_hits(
                account,
                "rocket in:inbox",
                &[server_hit(Some("Project X"), "launch")],
            )
            .expect("hits should record");
        assert!(search(&store, account, "rocket in:inbox").is_empty());
        // ...and recording replaced the earlier query's hits.
        assert!(search(&store, account, "rocket").is_empty());

        // A thread the local index also finds is listed once, as local.
        store
            .record_server_search_hits(account, "budget", &[server_hit(None, "budget")])
            .expect("hits should record");
        let hits = store
            .search_threads(account, "budget", 10)
            .expect("search should run");
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| !hit.from_server));
    }
}
