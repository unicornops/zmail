//! What the CLI prints from the store. Every string that came from mail, a
//! server or an account (subjects, senders, bodies, file names, mailbox
//! names, server errors) goes through `zmail_security::terminal`'s filter
//! first, as ADR 0007 requires of the terminal UI: an escape sequence in a
//! subject must not rewrite the screen, set the title or write the
//! clipboard when `zmail-app read` prints it.

use std::io::{self, Write};

use zmail_core::{
    AccountSummary, DraftDetail, DraftSummary, OutboxJobSummary, PendingOp, SearchHit, ThreadView,
};
use zmail_security::terminal::{sanitize, sanitize_line};

/// A one-line field, safe for the terminal.
pub(crate) fn line(text: &str) -> String {
    sanitize_line(text)
}

/// Text of several lines (a body), safe for the terminal.
pub(crate) fn text(text: &str) -> String {
    sanitize(text)
}

pub(crate) fn write_accounts(out: &mut dyn Write, accounts: &[AccountSummary]) -> io::Result<()> {
    for account in accounts {
        writeln!(
            out,
            "#{} {} [{}] {}",
            account.id,
            line(&account.display_name),
            account.protocol,
            line(&account.endpoint)
        )?;
        writeln!(out, "  user: {}", line(&account.username))?;
        writeln!(out, "  identities: {}", account.identity_count)?;
        writeln!(
            out,
            "  sync: {} ({})",
            account.sync_status.phase,
            line(&account.sync_status.detail)
        )?;
    }
    Ok(())
}

pub(crate) fn write_search_hits(out: &mut dyn Write, hits: &[SearchHit]) -> io::Result<()> {
    for hit in hits {
        let origin = if hit.from_server { " (server)" } else { "" };
        writeln!(
            out,
            "#{} [{}] {}{origin}",
            hit.thread_id,
            line(&hit.mailbox_name),
            line(&hit.subject)
        )?;
        writeln!(out, "  {}", line(&hit.participants))?;
        writeln!(out, "  {}", line(&hit.preview))?;
        writeln!(
            out,
            "  {} unread • flagged={} • {}",
            hit.unread_count,
            hit.is_flagged,
            line(&hit.latest_message_at)
        )?;
    }
    Ok(())
}

/// `read`: the conversation, each message with its attachments and body.
pub(crate) fn write_thread(out: &mut dyn Write, thread: &ThreadView) -> io::Result<()> {
    write_thread_heading(out, thread)?;
    writeln!(
        out,
        "{} msgs • {} unread • flagged={} • remote_content={}",
        thread.message_count, thread.unread_count, thread.is_flagged, thread.has_remote_content
    )?;
    writeln!(out, "{}", line(&thread.participants))?;

    for message in &thread.messages {
        writeln!(out)?;
        writeln!(
            out,
            "{} -> {} [{}]",
            line(&message.sender),
            line(&message.recipients),
            message.render_mode
        )?;
        writeln!(
            out,
            "{} • read={} • flagged={} • remote_content={}",
            line(&message.received_at),
            message.is_read,
            message.is_flagged,
            message.has_remote_content
        )?;
        for attachment in &message.attachments {
            writeln!(
                out,
                "  attachment #{} {} [{}] {} bytes",
                attachment.id,
                line(&attachment.filename),
                line(&attachment.content_type),
                attachment.size_bytes
            )?;
        }
        writeln!(out, "{}", text(&message.rendered_body))?;
    }
    Ok(())
}

/// `attachments`: the conversation's attachments, one a line.
pub(crate) fn write_attachments(out: &mut dyn Write, thread: &ThreadView) -> io::Result<()> {
    write_thread_heading(out, thread)?;
    let mut count = 0;
    for message in &thread.messages {
        for attachment in &message.attachments {
            writeln!(
                out,
                "#{} {} [{}] {} bytes (message {})",
                attachment.id,
                line(&attachment.filename),
                line(&attachment.content_type),
                attachment.size_bytes,
                message.id
            )?;
            count += 1;
        }
    }
    if count == 0 {
        writeln!(out, "Thread {} has no attachments.", thread.id)?;
    }
    Ok(())
}

fn write_thread_heading(out: &mut dyn Write, thread: &ThreadView) -> io::Result<()> {
    writeln!(
        out,
        "#{} [{}] {}",
        thread.id,
        line(&thread.mailbox_name),
        line(&thread.subject)
    )
}

pub(crate) fn write_drafts(out: &mut dyn Write, drafts: &[DraftSummary]) -> io::Result<()> {
    for draft in drafts {
        writeln!(
            out,
            "#{} account={} identity={} [{}] {}",
            draft.id,
            draft.account_id,
            line(&draft.identity_name),
            draft.state,
            line(&draft.subject)
        )?;
        writeln!(
            out,
            "  to: {} • attachments: {} • updated: {}",
            line(&draft.recipient_summary),
            draft.attachment_count,
            line(&draft.updated_at)
        )?;
    }
    Ok(())
}

/// `draft`: its headers, body and attachments. A draft can come from a
/// `mailto:` link or a reply, so its text is untrusted too.
pub(crate) fn write_draft(out: &mut dyn Write, draft: &DraftDetail) -> io::Result<()> {
    writeln!(
        out,
        "#{} account={} identity={} <{}> [{}]",
        draft.id,
        draft.account_id,
        line(&draft.identity_name),
        line(&draft.identity_email),
        draft.state
    )?;
    writeln!(out, "To: {}", join_or_placeholder(&draft.to))?;
    writeln!(out, "Cc: {}", join_or_placeholder(&draft.cc))?;
    writeln!(out, "Bcc: {}", join_or_placeholder(&draft.bcc))?;
    if draft.subject.is_empty() {
        writeln!(out, "Subject: (no subject)")?;
    } else {
        writeln!(out, "Subject: {}", line(&draft.subject))?;
    }
    writeln!(out, "Updated: {}", line(&draft.updated_at))?;
    if let Some(queued_at) = draft.queued_at.as_deref() {
        writeln!(out, "Queued: {}", line(queued_at))?;
    }
    if let Some(sent_at) = draft.sent_at.as_deref() {
        writeln!(out, "Sent: {}", line(sent_at))?;
    }
    if let Some(error) = draft.last_error_message.as_deref() {
        writeln!(out, "Last error: {}", line(error))?;
    }
    writeln!(out)?;
    writeln!(out, "{}", text(&draft.body))?;
    if !draft.attachments.is_empty() {
        writeln!(out)?;
        writeln!(out, "Attachments:")?;
        for attachment in &draft.attachments {
            writeln!(
                out,
                "  #{} {} ({}, {} bytes)",
                attachment.id,
                line(&attachment.filename),
                line(&attachment.content_type),
                attachment.size_bytes
            )?;
        }
    }
    Ok(())
}

pub(crate) fn write_outbox(out: &mut dyn Write, jobs: &[OutboxJobSummary]) -> io::Result<()> {
    for job in jobs {
        writeln!(
            out,
            "#{} draft={} account={} [{}] attempts={}",
            job.id, job.draft_id, job.account_id, job.state, job.attempt_count
        )?;
        if let Some(next_attempt_at) = job.next_attempt_at.as_deref() {
            writeln!(out, "  next attempt: {}", line(next_attempt_at))?;
        }
        if let Some(error) = job.last_error_message.as_deref() {
            writeln!(out, "  last error: {}", line(error))?;
        }
    }
    Ok(())
}

pub(crate) fn write_dead_op(out: &mut dyn Write, op: &PendingOp) -> io::Result<()> {
    writeln!(
        out,
        "op {} • account {} • {:?} {} in {} -> {} • attempts: {} • last error: {}",
        op.id,
        op.account_id,
        op.kind,
        line(&op.remote_id),
        line(&op.source_mailbox),
        line(op.target_mailbox.as_deref().unwrap_or("-")),
        op.attempts,
        line(op.last_error.as_deref().unwrap_or("none")),
    )
}

fn join_or_placeholder(values: &[String]) -> String {
    if values.is_empty() {
        "(none)".to_owned()
    } else {
        line(&values.join(", "))
    }
}
