//! Internal diagnostics that used to sit in the window header.
//!
//! Store counts, the GPUI/Zed pin, the background runtime size, queued
//! write-back operations and the dead-letter count are useful when
//! debugging, but they are not something a person reading mail needs to
//! see. They are collected here so the planned Settings → About &
//! Diagnostics page (UI plan, Phase 6) can render them without the shell
//! having to rediscover where each value lives.

// Nothing renders the diagnostics until the About page exists.
#![allow(dead_code)]

use zmail_core::text::count_noun;

use super::WorkspaceView;
use crate::widgets::short_sha;

/// One labelled diagnostics line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticsLine {
    /// Short label, for example "Local store".
    pub(crate) label: &'static str,
    /// Human-readable value.
    pub(crate) value: String,
}

impl WorkspaceView {
    /// Collect the diagnostics lines for the selected account and the
    /// whole store.
    pub(crate) fn diagnostics(&self) -> Vec<DiagnosticsLine> {
        let mut lines = vec![
            DiagnosticsLine {
                label: "Local store",
                value: [
                    count_noun(self.stats.account_count, "account", "accounts"),
                    count_noun(self.stats.mailbox_count, "mailbox", "mailboxes"),
                    count_noun(self.stats.thread_count, "thread", "threads"),
                    count_noun(self.stats.message_count, "message", "messages"),
                ]
                .join(" • "),
            },
            DiagnosticsLine {
                label: "Upstream",
                value: format!(
                    "GPUI {} • Zed {}",
                    self.bootstrap.upstream.gpui_version,
                    short_sha(&self.bootstrap.upstream.zed_commit),
                ),
            },
            DiagnosticsLine {
                label: "Background runtime",
                value: count_noun(
                    self.bootstrap.runtime.worker_threads,
                    "Tokio worker",
                    "Tokio workers",
                ),
            },
            DiagnosticsLine {
                label: "Queues",
                value: format!(
                    "{} • {}",
                    count_noun(
                        self.pending_op_count,
                        "queued mailbox operation",
                        "queued mailbox operations"
                    ),
                    count_noun(self.outbox_jobs.len(), "outbox job", "outbox jobs"),
                ),
            },
        ];

        if self.dead_op_count > 0 {
            lines.push(DiagnosticsLine {
                label: "Dead-lettered operations",
                value: format!(
                    "{} (inspect with `zmail-app dead-ops`)",
                    count_noun(self.dead_op_count, "operation", "operations")
                ),
            });
        }

        if let Some(account) = self.selected_account() {
            lines.push(DiagnosticsLine {
                label: "Account",
                value: format!(
                    "{} • {} • {} • {}",
                    account.protocol,
                    account.endpoint,
                    count_noun(account.identity_count, "identity", "identities"),
                    account.cache_policy.name,
                ),
            });
            lines.push(DiagnosticsLine {
                label: "Sync",
                value: format!(
                    "{} • {}",
                    account.sync_status.phase, account.sync_status.detail
                ),
            });
        }

        lines
    }
}
