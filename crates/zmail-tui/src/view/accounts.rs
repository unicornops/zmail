//! Drawing Setup → Accounts: the list, each step of Add, and Change.
//!
//! Password fields show `[hidden]` once something is typed, never the
//! text or its length. Everything from a server, DNS or Proton goes
//! through `sanitize_line` here, as it is drawn.

use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
    text::Span,
};
use zmail_autodiscover::{AuthMethod, MailSettings, Provider, StepOutcome};
use zmail_core::{AuthKind, ProtocolKind, SyncPhase, text::count_noun};
use zmail_security::terminal::sanitize_line;
use zmail_setup::{
    AccountField,
    accounts::{auth_label, budget_label},
    check::{CheckPart, check_label},
    discovery::{self, describe_server, describe_source, short_error, step_label},
    form::security_label,
    google,
    oauth::REMOTE_LOOPBACK_NOTE,
    providers::app_password_help,
};
use zmail_sync::SYNCS_ELSEWHERE;

use super::{Theme, draw_pane};
use crate::{
    accounts::{AddAccount, EDIT_ROWS, EditAccount, Method, Row, Step, methods},
    app::App,
    pane::{PaneId, PaneLine},
    text::{self, fit, wrap},
};

/// Where text starts.
const MARGIN: &str = "  ";

/// Lines being built for a pane `width` wide.
struct Lines {
    lines: Vec<PaneLine>,
    width: usize,
    theme: Theme,
    /// Where the heading ends: errors go there, where they are seen.
    after_heading: Option<usize>,
}

impl Lines {
    fn new(width: u16, theme: Theme) -> Self {
        Self {
            lines: Vec::new(),
            width: usize::from(width),
            theme,
            after_heading: None,
        }
    }

    fn blank(&mut self) {
        self.lines.push(PaneLine::default());
    }

    /// One line, cut to the width.
    fn line(&mut self, text: &str, style: Style) {
        self.lines.push(PaneLine::new(Span::styled(
            fit(
                &format!("{MARGIN}{text}"),
                self.width,
                self.theme.ellipsis(),
            ),
            style,
        )));
    }

    /// A paragraph, wrapped, each row starting at `indent` more cells.
    fn para(&mut self, text: &str, style: Style, indent: usize) {
        let pad = " ".repeat(indent);
        let room = self.width.saturating_sub(MARGIN.len() + indent).max(10);
        for row in wrap(text, room) {
            self.lines.push(PaneLine {
                joiner: row.joiner,
                ..PaneLine::new(Span::styled(format!("{MARGIN}{pad}{}", row.text), style))
            });
        }
    }

    fn heading(&mut self, title: &str, detail: &str) {
        self.line(title, self.theme.bold());
        if !detail.is_empty() {
            self.para(detail, self.theme.faint(), 0);
        }
        self.blank();
        self.after_heading = Some(self.lines.len());
    }

    /// An error and its title just under the heading, filtered.
    fn error_on_top(&mut self, title: Option<&str>, text: &str) {
        let mut error = Lines::new(self.width as u16, self.theme);
        if let Some(title) = title {
            error.line(title, self.theme.flag());
        }
        error.error(text);
        error.blank();
        let at = self.after_heading.unwrap_or(self.lines.len());
        let count = error.lines.len();
        self.lines.splice(at..at, error.lines);
        self.after_heading = Some(at + count);
    }

    /// An error, filtered: it may quote a server.
    fn error(&mut self, text: &str) {
        self.para(&sanitize_line(text), self.theme.flag(), 0);
    }
}

/// The glyph a progress line starts with.
fn glyph(theme: Theme, state: Progress) -> &'static str {
    match (theme.unicode, state) {
        (true, Progress::Done) => "✓",
        (true, Progress::Nothing) => "–",
        (true, Progress::Failed) => "!",
        (true, Progress::Running) => "…",
        (true, Progress::Waiting) => "·",
        (false, Progress::Done) => "[ok]",
        (false, Progress::Nothing) => "[--]",
        (false, Progress::Failed) => "[!!]",
        (false, Progress::Running) => "[..]",
        (false, Progress::Waiting) => "[  ]",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Progress {
    Done,
    Nothing,
    Failed,
    Running,
    Waiting,
}

// ---------------------------------------------------------------------------
// The Accounts list
// ---------------------------------------------------------------------------

pub(super) fn draw_accounts(app: &mut App, frame: &mut Frame, theme: Theme, body: Rect) {
    let mut out = Lines::new(body.width, theme);
    out.para(
        "Your mail accounts, kept in the encrypted mail store with their passwords in the \
         system keychain. A adds one; C changes the one under the cursor; D deletes it from \
         this computer.",
        theme.faint(),
        0,
    );
    out.blank();
    let header = out.lines.len();
    if app.accounts.is_empty() {
        out.line("No accounts yet. A adds one.", Style::new());
    }
    let name_width = 22.min(out.width / 3);
    for (index, account) in app.accounts.iter().enumerate() {
        let status = match account.sync_status.phase {
            SyncPhase::Failed => "Sync failed".to_owned(),
            SyncPhase::Connecting | SyncPhase::Discovering | SyncPhase::Syncing => {
                "Syncing".to_owned()
            }
            _ if account.sync_status.last_success_at.is_some() => "Up to date".to_owned(),
            _ => "Not synced yet".to_owned(),
        };
        let kind = format!(
            "{} · {}",
            protocol_label(account.protocol),
            auth_label(app.account_auth(account.id))
        );
        let row = format!(
            "{}  {}  {}  {}",
            fit(
                &sanitize_line(&account.display_name),
                name_width,
                theme.ellipsis()
            ),
            fit(
                &sanitize_line(&account.username),
                name_width + 2,
                theme.ellipsis()
            ),
            fit(&kind, 16, theme.ellipsis()),
            status
        );
        let style = if index == app.accounts_cursor {
            theme.reversed()
        } else {
            Style::new()
        };
        out.lines.push(PaneLine {
            target: Some(index),
            ..PaneLine::new(Span::styled(
                fit(&format!("{MARGIN}{row}"), out.width, theme.ellipsis()),
                style,
            ))
        });
    }
    let cursor = (!app.accounts.is_empty()).then_some(header + app.accounts_cursor);
    draw_pane(app, frame, PaneId::Accounts, body, out.lines, cursor);
}

fn protocol_label(protocol: ProtocolKind) -> &'static str {
    match protocol {
        ProtocolKind::Imap => "IMAP",
        ProtocolKind::Jmap => "JMAP",
        ProtocolKind::Proton => "Proton",
    }
}

// ---------------------------------------------------------------------------
// Change
// ---------------------------------------------------------------------------

pub(super) fn draw_edit(app: &mut App, frame: &mut Frame, theme: Theme, body: Rect) {
    let over_ssh = app.env.over_ssh;
    let Some(edit) = app.edit.as_ref() else {
        return;
    };
    let mut out = Lines::new(body.width, theme);
    out.heading(
        &format!("Change {}", sanitize_line(&edit.account.display_name)),
        "Up and Down choose a row; Enter changes it.",
    );
    let first = out.lines.len();
    let sign_in = match edit.auth {
        AuthKind::Password => format!(
            "New password : {}   (Enter saves it)",
            if edit.password.is_empty() {
                ""
            } else {
                "[hidden]"
            }
        ),
        AuthKind::OAuth2 => "Sign in again in the browser (Enter)".to_owned(),
        AuthKind::ProtonSession => {
            "Signing in to Proton again: delete the account and add it again".to_owned()
        }
    };
    let rows = [
        sign_in,
        format!("Cache budget : < {} >", budget_label(edit.budget)),
    ];
    for (index, text) in rows.iter().enumerate().take(EDIT_ROWS) {
        let style = if index == edit.cursor {
            theme.reversed()
        } else {
            Style::new()
        };
        out.lines.push(PaneLine {
            target: Some(index),
            ..PaneLine::new(Span::styled(
                fit(&format!("{MARGIN}{text}"), out.width, theme.ellipsis()),
                style,
            ))
        });
    }
    out.blank();
    out.para(
        "The cache budget caps the downloaded bodies and attachments kept for this account; \
         the oldest go first and download again when opened.",
        theme.faint(),
        0,
    );
    let identity_lines = identity_lines(&mut out, edit);
    identity_keys(&mut out, edit);
    if let Some(url) = &edit.sign_in_url {
        out.blank();
        browser_lines(&mut out, url, over_ssh);
    }
    let cursor = match edit.cursor.checked_sub(EDIT_ROWS) {
        Some(index) => identity_lines + index,
        None => first + edit.cursor,
    };
    let password_row = edit.cursor == 0 && edit.auth == AuthKind::Password;
    draw_pane(
        app,
        frame,
        PaneId::EditAccount,
        body,
        out.lines,
        Some(cursor),
    );
    if password_row {
        let x = MARGIN.len() + text::width("New password : ");
        place_cursor(app, frame, PaneId::EditAccount, body, cursor, x);
    }
}

/// Change's identities, each a row with how it stands against the
/// server, then what the one under the cursor needs, if anything. Names,
/// addresses, reply-tos and signatures come from the server, so all are
/// filtered. Returns the line the first identity's row is on.
fn identity_lines(out: &mut Lines, edit: &EditAccount) -> usize {
    use zmail_setup::identities::{
        RECREATE, REMOVE, UPDATE_SERVER, USE_SERVER, differences, identity_line, notice_text,
        notice_title, status_label,
    };
    let theme = out.theme;
    out.blank();
    out.line("Identities", theme.bold());
    let first = out.lines.len();
    for (index, identity) in edit.identities.iter().enumerate() {
        let marks = [
            identity
                .identity
                .is_primary
                .then_some(zmail_setup::identity_editor::PRIMARY),
            Some(status_label(identity.status)).filter(|status| !status.is_empty()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        let text = if marks.is_empty() {
            sanitize_line(&identity_line(identity))
        } else {
            format!(
                "{}  ({})",
                sanitize_line(&identity_line(identity)),
                marks.join(", ")
            )
        };
        let row = EDIT_ROWS + index;
        let style = if row == edit.cursor {
            theme.reversed()
        } else if identity.status.needs_attention() {
            theme.flag()
        } else {
            Style::new()
        };
        out.lines.push(PaneLine {
            target: Some(row),
            ..PaneLine::new(Span::styled(
                fit(&format!("{MARGIN}{text}"), out.width, theme.ellipsis()),
                style,
            ))
        });
    }
    let Some(identity) = edit.identity_at_cursor() else {
        return first;
    };
    let Some(title) = notice_title(identity) else {
        return first;
    };
    out.blank();
    out.para(&sanitize_line(&title), theme.bold(), 0);
    out.para(&sanitize_line(&notice_text(identity)), Style::new(), 0);
    for (field, ours, theirs) in differences(identity) {
        out.para(
            &format!(
                "{field}: Zmail has \u{201c}{}\u{201d}, the server \u{201c}{}\u{201d}",
                sanitize_line(&ours),
                sanitize_line(&theirs)
            ),
            theme.faint(),
            2,
        );
    }
    let keys = if identity.status.differs() {
        format!("U: {UPDATE_SERVER}   S: {USE_SERVER}")
    } else {
        format!("R: {RECREATE}   D: {REMOVE}")
    };
    out.para(&keys, theme.faint(), 0);
    first
}

/// What the identities' keys do, under the list.
fn identity_keys(out: &mut Lines, edit: &EditAccount) {
    use zmail_setup::identity_editor::{ADD, EDIT, MAKE_PRIMARY};
    let account = edit.account_identities();
    let theme = out.theme;
    let mut keys = format!("Ret/E: {EDIT}   D: Remove   *: {MAKE_PRIMARY}");
    if account.can_add() {
        keys.push_str(&format!("   A: {ADD}"));
    }
    out.blank();
    out.para(&keys, theme.faint(), 0);
    if let Some(note) = account.add_note() {
        out.para(note, theme.faint(), 0);
    }
}

// ---------------------------------------------------------------------------
// Add
// ---------------------------------------------------------------------------

pub(super) fn draw_add(app: &mut App, frame: &mut Frame, theme: Theme, body: Rect) {
    let over_ssh = app.env.over_ssh;
    let syncs_here = app.sync.as_ref().map(zmail_sync::SyncService::runs_sync);
    let Some(add) = app.add.as_ref() else {
        return;
    };
    let mut out = Lines::new(body.width, theme);
    let mut focus_line = None;
    match add.step {
        Step::Address => {
            out.heading(
                "Add an account",
                "Your email address first. Zmail finds your provider's settings from it; your \
                 password isn't sent anywhere until Zmail signs in.",
            );
            focus_line = form_lines(&mut out, add);
            #[cfg(feature = "proton")]
            {
                out.blank();
                out.para(
                    "Proton Mail: ^P signs in to Proton instead (paid Proton plans; Zmail \
                     signs in the way Proton Mail Bridge does).",
                    theme.faint(),
                    0,
                );
            }
        }
        Step::Discovering => discovering_lines(&mut out, add),
        Step::Method => method_lines(&mut out, add, over_ssh),
        Step::Credentials => {
            credentials_intro(&mut out, add, over_ssh);
            focus_line = form_lines(&mut out, add);
        }
        Step::Servers => {
            out.heading(
                "Your servers",
                "Check them, or change them if your provider says otherwise.",
            );
            found_lines(&mut out, add);
            focus_line = form_lines(&mut out, add);
            if add.form.uses_cleartext() {
                out.blank();
                out.para(
                    "Without encryption your password and your mail cross the network as \
                     plain text, and anyone on the way can read or change them. Only use this \
                     for a server on your own computer or network.",
                    if add.form.allow_cleartext {
                        theme.flag()
                    } else {
                        theme.faint()
                    },
                    0,
                );
            }
        }
        Step::Browser => {
            out.heading("Sign in in your browser", "");
            browser_lines(
                &mut out,
                add.sign_in_url.as_deref().unwrap_or_default(),
                over_ssh,
            );
        }
        Step::Testing => testing_lines(&mut out, add),
        Step::Identity => {
            out.heading(
                "Your identity",
                "The connection works. This is the name and address your mail is sent with; \
                 the signature goes under messages you write.",
            );
            focus_line = form_lines(&mut out, add);
        }
        Step::Done => done_lines(&mut out, add, syncs_here),
        #[cfg(feature = "proton")]
        Step::ProtonSignIn => {
            out.heading(
                "Sign in to Proton Mail",
                "Use your Proton address or username and your Proton password.",
            );
            out.line(zmail_setup::proton::RISK_TITLE, theme.bold());
            out.para(zmail_setup::proton::RISK_NOTE, Style::new(), 0);
            out.blank();
            focus_line = form_lines(&mut out, add);
        }
        #[cfg(feature = "proton")]
        Step::ProtonVerification => {
            out.heading("Verify it's you", zmail_setup::proton::VERIFICATION_DETAIL);
            out.para(
                "Zmail opened Proton's page in your browser. If it didn't open, open this \
                 address yourself:",
                Style::new(),
                0,
            );
            out.blank();
            out.para(
                &sanitize_line(add.proton.verify_url.as_deref().unwrap_or_default()),
                theme.link(),
                2,
            );
            out.blank();
            out.para(zmail_setup::proton::VERIFICATION_CAPTION, theme.faint(), 0);
        }
        #[cfg(feature = "proton")]
        Step::ProtonTotp => {
            out.heading(
                "Two-factor authentication",
                "Enter the code your authenticator app shows for Proton.",
            );
            focus_line = form_lines(&mut out, add);
        }
        #[cfg(feature = "proton")]
        Step::ProtonMailboxPassword => {
            out.heading(
                "Mailbox password",
                "This Proton account has a second password that unlocks its mail.",
            );
            focus_line = form_lines(&mut out, add);
        }
    }
    // Errors go under the heading, where they are seen; the form's lines
    // move down by as much.
    let before = out.lines.len();
    #[cfg(feature = "proton")]
    if add.step.is_proton() {
        if add.proton.busy {
            out.blank();
            out.line("Talking to Proton…", theme.faint());
        } else if let Some(error) = &add.proton.error {
            out.error_on_top(Some("Couldn't sign in"), error);
        }
    }
    if let Some(error) = &add.error {
        out.error_on_top(None, error);
    }
    let inserted = if add.proton_busy() {
        0
    } else {
        out.lines.len() - before
    };
    let focus_line = focus_line.map(|(line, column)| (line + inserted, column));
    let focus = focus_line.map(|(line, _)| line);
    let method_cursor = (add.step == Step::Method).then(|| {
        out.lines
            .iter()
            .position(|line| line.target == Some(add.method_cursor))
            .unwrap_or(0)
    });
    let cursor_column = focus_line.and_then(|(_, column)| column);
    draw_pane(
        app,
        frame,
        PaneId::AddAccount,
        body,
        out.lines,
        focus.or(method_cursor),
    );
    if let (Some(line), Some(column)) = (focus, cursor_column) {
        place_cursor(app, frame, PaneId::AddAccount, body, line, column);
    }
}

/// Shows the terminal's cursor at `column` of pane line `line`, when that
/// line is on screen.
fn place_cursor(app: &App, frame: &mut Frame, id: PaneId, body: Rect, line: usize, column: usize) {
    let scroll = app.scroll.get(&id).copied().unwrap_or(0);
    if line < scroll || line >= scroll + usize::from(body.height) {
        return;
    }
    let x = column.min(usize::from(body.width).saturating_sub(1));
    frame.set_cursor_position(Position::new(
        body.x + x as u16,
        body.y + (line - scroll) as u16,
    ));
}

/// The step's form rows, with their errors under them. Returns the
/// focused row's line and, for a text field, where the cursor goes.
fn form_lines(out: &mut Lines, add: &AddAccount) -> Option<(usize, Option<usize>)> {
    let theme = out.theme;
    let rows = add.rows();
    let label_width = rows
        .iter()
        .map(|row| text::width(row.label(&add.form)))
        .max()
        .unwrap_or(0);
    let mut focus = None;
    for (index, row) in rows.iter().enumerate() {
        let focused = index == add.focus;
        let label = row.label(&add.form);
        let prefix = format!(
            "{MARGIN}{}{label} : ",
            " ".repeat(label_width.saturating_sub(text::width(label)))
        );
        let (value, placeholder) = row_value(add, *row);
        let value_style = if placeholder {
            theme.faint()
        } else {
            Style::new()
        };
        let line = out.lines.len();
        let label_style = if focused {
            theme.reversed()
        } else {
            theme.bold()
        };
        let shown = fit(
            &value,
            out.width.saturating_sub(text::width(&prefix)),
            theme.ellipsis(),
        );
        out.lines.push(PaneLine {
            target: Some(index),
            spans: vec![
                Span::styled(prefix.clone(), label_style),
                Span::styled(shown.clone(), value_style),
            ],
            ..PaneLine::default()
        });
        if focused {
            let column = matches!(row, Row::Field(_))
                .then(|| text::width(&prefix) + if placeholder { 0 } else { text::width(&shown) });
            focus = Some((line, column));
        }
        let error = match row {
            Row::Field(field) => add.errors.get(field),
            Row::ImapSecurity => add.errors.get(&AccountField::ImapSecurity),
            Row::SmtpSecurity => add.errors.get(&AccountField::SmtpSecurity),
            Row::Cleartext => add.errors.get(&AccountField::AllowCleartext),
            Row::Protocol => None,
        };
        if let Some(error) = error {
            out.para(error, theme.flag(), label_width + 3);
        }
    }
    focus
}

/// A row's value as shown, and whether it is the placeholder.
fn row_value(add: &AddAccount, row: Row) -> (String, bool) {
    let form = &add.form;
    let choice = |text: &str| (format!("< {text} >"), false);
    match row {
        Row::Protocol => choice(match form.protocol {
            ProtocolKind::Jmap => "JMAP",
            _ => "IMAP and SMTP",
        }),
        Row::ImapSecurity => choice(security_label(form.imap_security)),
        Row::SmtpSecurity => choice(security_label(form.smtp_security)),
        Row::Cleartext => (
            if form.allow_cleartext { "[X]" } else { "[ ]" }.to_owned(),
            false,
        ),
        Row::Field(field) => {
            let value = add.value(field);
            if value.is_empty() {
                let placeholder = match field {
                    AccountField::OauthClientSecret => {
                        zmail_setup::form::oauth_secret_placeholder(form.provider)
                    }
                    AccountField::Name => "Jane Doe",
                    field => field.placeholder(),
                };
                return (placeholder.to_owned(), true);
            }
            if field.is_secret() {
                // Never the text, nor its length.
                return ("[hidden]".to_owned(), false);
            }
            // What was typed or pasted is filtered as it arrives; prefilled
            // values may come from a server.
            (sanitize_line(value), false)
        }
    }
}

fn discovering_lines(out: &mut Lines, add: &AddAccount) {
    let theme = out.theme;
    let Some(discovery) = &add.discovery else {
        return;
    };
    let domain = sanitize_line(discovery.email().domain());
    out.heading(
        "Finding your settings",
        &format!(
            "Zmail is asking {domain} how to connect. Your password isn't sent anywhere until \
             Zmail signs in."
        ),
    );
    for step in discovery.steps() {
        let report = discovery
            .reports()
            .iter()
            .find(|report| report.step == step);
        let running = add.running == Some(step);
        let (state, detail) = match report.map(|report| &report.outcome) {
            _ if running => (Progress::Running, None),
            Some(StepOutcome::Found { note }) => (Progress::Done, note.clone()),
            Some(StepOutcome::NothingFound { note }) => (
                Progress::Nothing,
                Some(note.clone().unwrap_or_else(|| "Nothing found".to_owned())),
            ),
            Some(StepOutcome::Failed(error)) => (Progress::Failed, Some(short_error(error))),
            Some(StepOutcome::Skipped) => {
                (Progress::Nothing, Some(discovery::SKIPPED_NOTE.to_owned()))
            }
            None => (Progress::Waiting, None),
        };
        let style = match state {
            Progress::Running => theme.bold(),
            Progress::Waiting => theme.faint(),
            _ => Style::new(),
        };
        out.line(
            &format!("{} {}", glyph(theme, state), step_label(step, &domain)),
            style,
        );
        if let Some(detail) = detail {
            let indent = text::width(glyph(theme, state)) + 1;
            out.para(&sanitize_line(&detail), theme.faint(), indent);
        }
    }
    if add.running.is_none() && discovery.needs_consent() {
        out.blank();
        out.line("Ask Mozilla's database?", theme.bold());
        out.para(
            &format!(
                "{domain} doesn't publish its mail settings. Thunderbird's provider database \
                 (ISPDB) has the settings of many mail providers. {} Y asks it; N doesn't.",
                discovery::consent_explanation(&domain)
            ),
            Style::new(),
            0,
        );
    }
}

fn method_lines(out: &mut Lines, add: &AddAccount, over_ssh: bool) {
    let theme = out.theme;
    let provider = add.form.provider;
    let title = match provider {
        Some(provider) => format!("How do you sign in to {provider}?"),
        None => "How do you sign in?".to_owned(),
    };
    out.heading(&title, "");
    found_lines(out, add);
    let methods = methods(provider);
    for (index, method) in methods.iter().enumerate() {
        let selected = index == add.method_cursor;
        let marker = if selected { ">" } else { " " };
        out.lines.push(PaneLine {
            target: Some(index),
            ..PaneLine::new(Span::styled(
                fit(
                    &format!("{MARGIN}{marker} {}", method.label(provider)),
                    out.width,
                    theme.ellipsis(),
                ),
                if selected {
                    theme.reversed()
                } else {
                    Style::new()
                },
            ))
        });
    }
    out.blank();
    let method = methods
        .get(add.method_cursor)
        .copied()
        .unwrap_or(Method::Password);
    method_hint(out, method, provider, over_ssh);
}

/// What the highlighted way of signing in needs.
fn method_hint(out: &mut Lines, method: Method, provider: Option<Provider>, over_ssh: bool) {
    let theme = out.theme;
    match method {
        Method::Password => out.para(
            "Your password stays on this computer, in the system keychain.",
            theme.faint(),
            0,
        ),
        Method::AppPassword => match provider.and_then(app_password_help) {
            Some(help) => {
                out.para(help.text, Style::new(), 0);
                out.line(&format!("How to create one: {}", help.url), theme.link());
                if provider == Some(Provider::Google) {
                    out.line(
                        &format!("Create one: {}", google::APP_PASSWORDS_URL),
                        theme.link(),
                    );
                }
            }
            None => out.para(
                "If your provider gives mail apps passwords of their own, use one here. It \
                 stays on this computer, in the system keychain.",
                theme.faint(),
                0,
            ),
        },
        Method::OAuth => {
            let text = match provider {
                Some(Provider::Google) => {
                    "An app password is quicker. If you can't use one, create a free OAuth \
                     client in Google Cloud: about ten minutes, once. The next step lists how. \
                     Zmail never sees your Google password."
                }
                Some(Provider::Microsoft365) => {
                    "Microsoft 365 only allows OAuth sign-in. Zmail doesn't have its own \
                     Microsoft app registration yet, so enter an OAuth client ID from your \
                     organisation."
                }
                _ => "Enter your provider's OAuth client ID and its endpoints.",
            };
            out.para(text, Style::new(), 0);
            if over_ssh {
                out.blank();
                out.para(REMOTE_LOOPBACK_NOTE, theme.flag(), 0);
            }
        }
    }
}

fn credentials_intro(out: &mut Lines, add: &AddAccount, over_ssh: bool) {
    let theme = out.theme;
    let provider = add.form.provider;
    let title = match provider {
        Some(provider) => format!("Sign in to {provider}"),
        None => "Sign in".to_owned(),
    };
    if !add.form.use_oauth {
        out.heading(&title, "");
        method_hint(out, add.method, provider, over_ssh);
        out.blank();
        return;
    }
    if provider == Some(Provider::Google) {
        out.heading(
            "Use your own Google OAuth client",
            "Zmail has no Google client of its own. Create one in Google Cloud, then paste \
             its ID and secret below.",
        );
        for (number, step) in google::GUIDE_STEPS.iter().enumerate() {
            out.line(&format!("{}. {}", number + 1, step.title), theme.bold());
            out.para(step.body, Style::new(), 3);
            if let Some((_, url)) = step.link {
                out.para(url, theme.link(), 3);
            }
        }
        out.blank();
        out.line("Sign-ins expire after 7 days in Testing", theme.bold());
        out.para(google::TESTING_EXPIRY, theme.faint(), 0);
        out.line(
            &format!("The full guide: {}", google::GUIDE_URL),
            theme.link(),
        );
    } else {
        out.heading(&title, "");
        method_hint(out, Method::OAuth, provider, false);
    }
    if over_ssh {
        out.blank();
        out.para(REMOTE_LOOPBACK_NOTE, theme.flag(), 0);
    }
    out.blank();
}

/// What a preset or discovery found, and where.
fn found_lines(out: &mut Lines, add: &AddAccount) {
    let theme = out.theme;
    let Some(found) = &add.found else {
        if add.discovery.is_some() {
            out.para(
                "Zmail couldn't find your settings. Enter them from your provider's help \
                 pages; it guessed the usual names.",
                theme.flag(),
                0,
            );
            out.blank();
        }
        return;
    };
    let server = |out: &mut Lines, title: &str, value: String, source: String| {
        out.line(&format!("{title}: {}", sanitize_line(&value)), Style::new());
        out.para(
            &format!("Found in {}", sanitize_line(&source)),
            theme.faint(),
            4,
        );
    };
    match found {
        MailSettings::Jmap(jmap) => server(
            out,
            "JMAP",
            jmap.value.session_url.clone(),
            describe_source(&jmap.source),
        ),
        MailSettings::ImapSmtp { imap, smtp } => {
            server(
                out,
                "Incoming (IMAP)",
                describe_server(&imap.value),
                describe_source(&imap.source),
            );
            server(
                out,
                "Outgoing (SMTP)",
                describe_server(&smtp.value),
                describe_source(&smtp.source),
            );
            if imap.value.auth == AuthMethod::OAuth2 && !add.form.use_oauth {
                out.para(
                    "Your provider lists only OAuth sign-in. If your password is refused, \
                     create an app password, or choose OAuth.",
                    theme.faint(),
                    0,
                );
            }
        }
    }
    if discovery::is_guess(found) {
        out.para(
            "Best guess: these servers answered on the usual ports, but nothing confirmed \
             they are your provider's. If signing in fails, check them against your \
             provider's help pages.",
            theme.flag(),
            0,
        );
    }
    out.blank();
}

/// The OAuth2 URL to open, and what happens while Zmail waits.
fn browser_lines(out: &mut Lines, url: &str, over_ssh: bool) {
    let theme = out.theme;
    if over_ssh {
        out.para(REMOTE_LOOPBACK_NOTE, theme.flag(), 0);
        out.blank();
        out.para(
            "If you still want to try (with the port forwarded, say), open this address:",
            Style::new(),
            0,
        );
    } else {
        out.para(
            "Zmail opened your browser at your provider's sign-in page. If it didn't open, \
             open this address yourself:",
            Style::new(),
            0,
        );
    }
    out.blank();
    // In full, so it can be copied; it is Zmail's own URL, with the
    // client ID the user typed.
    out.para(&sanitize_line(url), theme.link(), 2);
    out.blank();
    out.para(
        &format!(
            "Waiting for the browser to send the sign-in back to 127.0.0.1:{}. Esc stops \
             waiting.",
            zmail_setup::form::OAUTH2_REDIRECT_PORT
        ),
        theme.faint(),
        0,
    );
}

fn testing_lines(out: &mut Lines, add: &AddAccount) {
    let theme = out.theme;
    out.heading(
        "Testing the connection",
        "Zmail signs in and out again, and sends nothing.",
    );
    let Ok((account, _)) = zmail_setup::build_account_from_form(&add.form) else {
        return;
    };
    let mut parts = vec![CheckPart::Incoming];
    if account.protocol == ProtocolKind::Imap {
        parts.push(CheckPart::Outgoing);
    }
    let running = add.checking.unwrap_or(CheckPart::Incoming);
    for part in parts {
        let state = match (running, part) {
            (CheckPart::Outgoing, CheckPart::Incoming) => Progress::Done,
            (now, part) if now == part => Progress::Running,
            _ => Progress::Waiting,
        };
        out.line(
            &format!(
                "{} {}",
                glyph(theme, state),
                sanitize_line(&check_label(&account, part))
            ),
            if state == Progress::Running {
                theme.bold()
            } else {
                Style::new()
            },
        );
    }
}

fn done_lines(out: &mut Lines, add: &AddAccount, syncs_here: Option<bool>) {
    let theme = out.theme;
    let address = sanitize_line(&add.form.email);
    out.heading(&format!("{address} is added"), "");
    match syncs_here {
        Some(false) => {
            out.para(
                &format!(
                    "{SYNCS_ELSEWHERE}: that Zmail starts syncing the new account within a few \
                     seconds."
                ),
                Style::new(),
                0,
            );
        }
        None => out.para(
            "This Zmail doesn't sync; the next Zmail that does picks the account up.",
            Style::new(),
            0,
        ),
        Some(true) => match &add.progress {
            Some(progress) if progress.error.is_some() => {
                out.error(&format!(
                    "The first sync failed: {}",
                    progress.error.as_deref().unwrap_or_default()
                ));
                out.para(
                    "The account is saved. Fix it later in Setup → Accounts (C changes it).",
                    theme.faint(),
                    0,
                );
            }
            Some(progress) if progress.done => out.line(
                &format!(
                    "Your mail is here: {}, {}.",
                    count_noun(progress.folders, "folder", "folders"),
                    count_noun(progress.conversations, "conversation", "conversations")
                ),
                Style::new(),
            ),
            Some(progress) => {
                out.line("Getting your mail…", theme.bold());
                if !progress.detail.is_empty() {
                    out.para(&sanitize_line(&progress.detail), theme.faint(), 0);
                }
            }
            None => out.line("Getting your mail…", theme.bold()),
        },
    }
    out.blank();
    out.para(
        "Enter goes back to your accounts. The mail shows in the Folder List (L) as it \
         arrives.",
        theme.faint(),
        0,
    );
}
