//! Settings → About & Diagnostics: the version and upstream pins, the
//! store and its queues, changes the server never accepted, and "Copy
//! Diagnostics".
//!
//! A mailbox change (read, flag, move, delete) is applied locally at once
//! and queued for the server. One that keeps failing is set aside
//! ("dead-lettered") after its retry budget; it used to be visible only
//! through `zmail-app dead-ops`. Here each one has Retry, which queues it
//! again, and Discard, which gives up on it so the next sync shows the
//! server's version.
//!
//! [`Diagnostics::report`] is the text "Copy Diagnostics" puts on the
//! clipboard, for a bug report. It holds versions, counts, sync phases and
//! error messages, and never message content, names, addresses, server
//! names, folder names or secrets: errors are passed through [`redact`]
//! first. The `diagnostics_never_include_mail_or_secrets` test checks it.

use gpui::{
    ClipboardItem, Context, IntoElement, Render, SharedString, Window, div, prelude::*, px, rgb,
};
use zmail_core::{
    AccountSummary, OutboxJobState, PendingOp, PendingOpKind, StoreStats, text::count_noun,
};
use zmail_store::LocalStore;

use super::{SettingsEvent, SettingsHost, group_heading, section_heading, setting_row};
use crate::components::{Button, ButtonSize, ComponentTheme};
use crate::styles::{SPACE_2, SPACE_3, TYPE_CAPTION, TypeStyleExt, UiColors};
use crate::widgets::{account_dot, short_sha};

/// The longest error message the report carries.
const MAX_REPORT_ERROR_CHARS: usize = 300;

/// What About shows and "Copy Diagnostics" reports, read from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Diagnostics {
    pub(crate) app_version: String,
    pub(crate) gpui_version: String,
    pub(crate) zed_commit: String,
    pub(crate) worker_threads: usize,
    pub(crate) stats: StoreStats,
    pub(crate) accounts: Vec<AccountDiagnostics>,
    /// Outbox jobs waiting to send (queued, sending or retrying).
    pub(crate) outbox_waiting: usize,
    /// Outbox jobs that gave up and wait for Retry.
    pub(crate) outbox_failed: usize,
}

/// One account's sync and queue state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountDiagnostics {
    pub(crate) account: AccountSummary,
    /// Mailbox changes waiting for the server.
    pub(crate) pending_ops: usize,
    /// Mailbox changes the server never accepted.
    pub(crate) dead_ops: Vec<PendingOp>,
    /// Strings that identify the account or its mail (its name, sign-in,
    /// server and folders), which the report must not contain.
    private_terms: Vec<String>,
}

impl Diagnostics {
    /// Read the diagnostics from the store.
    pub(crate) fn collect(host: &SettingsHost) -> Self {
        let store = host.store.as_ref();
        let accounts = store.list_accounts().unwrap_or_default();
        let jobs = store.list_outbox_jobs(None).unwrap_or_default();
        Self {
            app_version: host.app_version.clone(),
            gpui_version: host.upstream.gpui_version.clone(),
            zed_commit: host.upstream.zed_commit.clone(),
            worker_threads: host.worker_threads,
            stats: store.stats().unwrap_or(StoreStats {
                account_count: 0,
                mailbox_count: 0,
                thread_count: 0,
                message_count: 0,
            }),
            accounts: accounts
                .into_iter()
                .map(|account| AccountDiagnostics::collect(store, account))
                .collect(),
            outbox_waiting: jobs
                .iter()
                .filter(|job| {
                    matches!(
                        job.state,
                        OutboxJobState::Queued | OutboxJobState::Sending | OutboxJobState::Retrying
                    )
                })
                .count(),
            outbox_failed: jobs
                .iter()
                .filter(|job| job.state == OutboxJobState::Failed)
                .count(),
        }
    }

    /// Every account's undelivered changes.
    pub(crate) fn dead_ops(&self) -> impl Iterator<Item = (&AccountSummary, &PendingOp)> {
        self.accounts.iter().flat_map(|diagnostics| {
            diagnostics
                .dead_ops
                .iter()
                .map(|op| (&diagnostics.account, op))
        })
    }

    /// "GPUI v1.24.2 • Zed 1a2b3c4d".
    fn upstream_line(&self) -> String {
        format!(
            "GPUI {} \u{2022} Zed {}",
            or_unknown(&self.gpui_version),
            or_unknown(short_sha(&self.zed_commit))
        )
    }

    fn store_line(&self) -> String {
        [
            count_noun(self.stats.account_count, "account", "accounts"),
            count_noun(self.stats.mailbox_count, "mailbox", "mailboxes"),
            count_noun(self.stats.thread_count, "conversation", "conversations"),
            count_noun(self.stats.message_count, "message", "messages"),
        ]
        .join(" \u{2022} ")
    }

    fn outbox_line(&self) -> String {
        format!(
            "{} waiting \u{2022} {} failed",
            self.outbox_waiting, self.outbox_failed
        )
    }

    /// The plain-text report "Copy Diagnostics" copies. Accounts are
    /// numbered, not named, and every free-text error goes through
    /// [`redact`].
    pub(crate) fn report(&self) -> String {
        let mut lines = vec![
            format!(
                "Zmail {} ({} {})",
                self.app_version,
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
            self.upstream_line(),
            format!(
                "Background runtime: {}",
                count_noun(self.worker_threads, "thread", "threads")
            ),
            format!("Local store: {}", self.store_line()),
            format!("Outbox: {}", self.outbox_line()),
        ];
        for (index, diagnostics) in self.accounts.iter().enumerate() {
            let account = &diagnostics.account;
            let status = &account.sync_status;
            let redact = |text: &str| redact(text, &diagnostics.private_terms);
            lines.push(format!(
                "Account {}: {} \u{2022} {} \u{2022} {} \u{2022} last success {} \u{2022} {} \u{2022} {} \u{2022} cache {}",
                index + 1,
                account.protocol.label(),
                status.phase.label(),
                count_noun(status.failure_count, "failure", "failures"),
                status.last_success_at.as_deref().unwrap_or("never"),
                count_noun(diagnostics.pending_ops, "queued change", "queued changes"),
                count_noun(diagnostics.dead_ops.len(), "undelivered change", "undelivered changes"),
                redact(&account.cache_policy.name),
            ));
            lines.push(format!("  Status: {}", redact(&status.detail)));
            if let Some(error) = &status.last_error_message {
                lines.push(format!("  Last error: {}", redact(error)));
            }
            for op in &diagnostics.dead_ops {
                lines.push(format!(
                    "  Undelivered {} after {}: {}",
                    op.kind.as_str(),
                    count_noun(op.attempts.max(0) as usize, "attempt", "attempts"),
                    redact(op.last_error.as_deref().unwrap_or("no error recorded")),
                ));
            }
        }
        lines.join("\n") + "\n"
    }
}

impl AccountDiagnostics {
    fn collect(store: &LocalStore, account: AccountSummary) -> Self {
        let mut private_terms = vec![
            account.display_name.clone(),
            account.username.clone(),
            account.endpoint.clone(),
        ];
        if let Ok(url) = url::Url::parse(&account.endpoint)
            && let Some(host) = url.host_str()
        {
            private_terms.push(host.to_owned());
        }
        for identity in store.list_identities(account.id).unwrap_or_default() {
            private_terms.push(identity.display_name);
            private_terms.push(identity.email_address);
        }
        for mailbox in store.list_mailboxes(account.id).unwrap_or_default() {
            private_terms.push(mailbox.name);
        }
        let dead_ops = store.list_dead_ops(account.id).unwrap_or_default();
        for op in &dead_ops {
            private_terms.push(op.source_mailbox.clone());
            private_terms.extend(op.target_mailbox.clone());
            private_terms.push(op.remote_id.clone());
        }
        // Longest first, so a name is not left half-replaced by a shorter
        // term inside it.
        private_terms.retain(|term| term.trim().chars().count() >= 3);
        private_terms.sort_by_key(|term| std::cmp::Reverse(term.len()));
        private_terms.dedup();
        Self {
            pending_ops: store.pending_op_count(account.id).unwrap_or(0),
            dead_ops,
            account,
            private_terms,
        }
    }
}

/// `text` with `private_terms` and anything that looks like an email
/// address or a URL replaced, cut to [`MAX_REPORT_ERROR_CHARS`].
pub(crate) fn redact(text: &str, private_terms: &[String]) -> String {
    let mut text = text.to_owned();
    for term in private_terms {
        text = replace_ignoring_case(&text, term.trim(), "[redacted]");
    }
    let words: Vec<String> = text
        .split(' ')
        .map(|word| {
            if word.contains('@') {
                "[address]".to_owned()
            } else if word.contains("://") {
                "[url]".to_owned()
            } else {
                word.to_owned()
            }
        })
        .collect();
    let text = words.join(" ");
    if text.chars().count() > MAX_REPORT_ERROR_CHARS {
        text.chars()
            .take(MAX_REPORT_ERROR_CHARS)
            .collect::<String>()
            + "…"
    } else {
        text
    }
}

/// `text` with every case-insensitive occurrence of `term` replaced.
fn replace_ignoring_case(text: &str, term: &str, with: &str) -> String {
    if term.is_empty() {
        return text.to_owned();
    }
    let lower_text = text.to_lowercase();
    let lower_term = term.to_lowercase();
    // Lowercasing can change byte lengths outside ASCII; fall back to an
    // exact match there rather than slicing at the wrong offsets.
    if lower_text.len() != text.len() || lower_term.len() != term.len() {
        return text.replace(term, with);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = 0;
    for (start, _) in lower_text.match_indices(&lower_term) {
        if start < rest {
            continue;
        }
        out.push_str(&text[rest..start]);
        out.push_str(with);
        rest = start + term.len();
    }
    out.push_str(&text[rest..]);
    out
}

fn or_unknown(value: &str) -> &str {
    if value.is_empty() { "unknown" } else { value }
}

/// What an undelivered change was, in words.
pub(crate) fn op_description(op: &PendingOp) -> String {
    let what = match op.kind {
        PendingOpKind::MarkRead => "Mark as read".to_owned(),
        PendingOpKind::MarkUnread => "Mark as unread".to_owned(),
        PendingOpKind::SetFlagged => "Flag".to_owned(),
        PendingOpKind::ClearFlagged => "Unflag".to_owned(),
        PendingOpKind::Move => format!(
            "Move to {}",
            op.target_mailbox.as_deref().unwrap_or("another folder")
        ),
        PendingOpKind::Destroy => "Delete".to_owned(),
    };
    format!("{what}, in {}", op.source_mailbox)
}

/// The About & Diagnostics section.
pub(crate) struct AboutSettings {
    diagnostics: Diagnostics,
}

impl AboutSettings {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            diagnostics: Diagnostics::collect(SettingsHost::get(cx)),
        }
    }

    /// Re-read the store; redraws only if something changed.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let diagnostics = Diagnostics::collect(SettingsHost::get(cx));
        if diagnostics != self.diagnostics {
            self.diagnostics = diagnostics;
            cx.notify();
        }
    }

    /// What the section shows.
    #[cfg(test)]
    pub(crate) fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    /// Queue an undelivered change again; the account's next sync sends
    /// it.
    pub(crate) fn retry_op(&mut self, op_id: i64, cx: &mut Context<Self>) {
        match SettingsHost::get(cx).store.retry_dead_op(op_id) {
            Ok(()) => {
                cx.emit(SettingsEvent::Status(
                    "The change is queued again and goes with the next sync.".to_owned(),
                ));
            }
            Err(error) => cx.emit(SettingsEvent::Error(format!(
                "Could not retry the change: {error:#}"
            ))),
        }
        self.refresh(cx);
    }

    /// Give up on an undelivered change.
    pub(crate) fn discard_op(&mut self, op_id: i64, cx: &mut Context<Self>) {
        match SettingsHost::get(cx).store.discard_dead_op(op_id) {
            Ok(()) => {
                cx.emit(SettingsEvent::Status(
                    "The change was discarded. The next sync shows the server's version."
                        .to_owned(),
                ));
            }
            Err(error) => cx.emit(SettingsEvent::Error(format!(
                "Could not discard the change: {error:#}"
            ))),
        }
        self.refresh(cx);
    }

    /// Put the report on the clipboard.
    pub(crate) fn copy_diagnostics(&mut self, cx: &mut Context<Self>) {
        self.refresh(cx);
        cx.write_to_clipboard(ClipboardItem::new_string(self.diagnostics.report()));
        cx.emit(SettingsEvent::Status(
            "Diagnostics copied to the clipboard.".to_owned(),
        ));
    }

    fn render_dead_ops(&self, colors: &UiColors, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let rows: Vec<_> = self
            .diagnostics
            .dead_ops()
            .map(|(account, op)| (account.display_name.clone(), account.color(), op.clone()))
            .collect();
        if rows.is_empty() {
            return vec![
                setting_row(
                    "None",
                    Some("Every change you made has reached the server.".into()),
                    div(),
                    colors,
                )
                .into_any_element(),
            ];
        }
        rows.into_iter()
            .map(|(account_name, color, op)| {
                let op_id = op.id;
                let detail: SharedString = format!(
                    "{account_name} \u{2022} {} \u{2022} {}",
                    count_noun(op.attempts.max(0) as usize, "attempt", "attempts"),
                    op.last_error.as_deref().unwrap_or("no error recorded"),
                )
                .into();
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_3))
                    .py(px(SPACE_3))
                    .border_b_1()
                    .border_color(rgb(colors.border))
                    .child(account_dot(color))
                    .child(super::setting_text(
                        op_description(&op),
                        Some(detail),
                        colors,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .gap(px(SPACE_2))
                            .child(
                                div()
                                    .debug_selector(move || format!("retry-op-{op_id}"))
                                    .child(
                                        Button::new(("retry-op", op_id as u64), "Retry")
                                            .size(ButtonSize::Small)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.retry_op(op_id, cx)
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .debug_selector(move || format!("discard-op-{op_id}"))
                                    .child(
                                        Button::new(("discard-op", op_id as u64), "Discard")
                                            .size(ButtonSize::Small)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.discard_op(op_id, cx)
                                            })),
                                    ),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

impl Render for AboutSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let diagnostics = &self.diagnostics;
        let line = |label: &'static str, value: String| {
            setting_row(
                label,
                None,
                div()
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(value),
                &colors,
            )
        };
        let version = line(
            "Version",
            format!("Zmail {}", or_unknown(&diagnostics.app_version)),
        );
        let upstream = line("Built with", diagnostics.upstream_line());
        let store = line("Local store", diagnostics.store_line());
        let outbox = line("Outbox", diagnostics.outbox_line());
        let accounts: Vec<_> = diagnostics
            .accounts
            .iter()
            .map(|account| {
                let status = &account.account.sync_status;
                setting_row(
                    account.account.display_name.clone(),
                    Some(
                        format!(
                            "{} \u{2022} {}",
                            status.phase.label(),
                            count_noun(
                                account.pending_ops,
                                "change waiting to sync",
                                "changes waiting to sync"
                            )
                        )
                        .into(),
                    ),
                    account_dot(account.account.color()),
                    &colors,
                )
            })
            .collect();
        let dead_ops = self.render_dead_ops(&colors, cx);
        div()
            .flex()
            .flex_col()
            .child(section_heading("About & Diagnostics"))
            .child(version)
            .child(upstream)
            .child(group_heading("Queues", &colors))
            .child(store)
            .child(outbox)
            .children(accounts)
            .child(group_heading("Undelivered changes", &colors))
            .children(dead_ops)
            .child(group_heading("Bug reports", &colors))
            .child(setting_row(
                "Copy Diagnostics",
                Some(
                    "Copies versions, counts and sync errors as plain text. It never includes messages, names, addresses, server or folder names, or passwords."
                        .into(),
                ),
                div().debug_selector(|| "copy-diagnostics".into()).child(
                    Button::new("copy-diagnostics", "Copy Diagnostics")
                        .icon("copy")
                        .on_click(cx.listener(|this, _, _, cx| this.copy_diagnostics(cx))),
                ),
                &colors,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn redact_hides_terms_addresses_and_urls() {
        let terms = vec!["Work Mail".to_owned(), "mail.example.test".to_owned()];
        assert_eq!(
            redact(
                "LOGIN for kai@example.test failed at imaps://mail.example.test:993 (work mail)",
                &terms
            ),
            "LOGIN for [address] failed at [url] ([redacted])"
        );
        assert_eq!(redact("connection refused", &terms), "connection refused");
        assert!(redact(&"x".repeat(400), &[]).chars().count() <= 301);
    }
}
