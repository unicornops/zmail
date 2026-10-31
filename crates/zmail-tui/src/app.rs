//! The terminal UI's state and what each key, click and tick does.
//!
//! [`App`] never touches the terminal. It reads the store, changes its own
//! state, and asks the runner (`lib.rs`) for anything that leaves the
//! process as an [`Effect`]: a copy, the bell, mouse capture, opening a
//! link. Drawing is in `view.rs`. Tests drive it with events and draw it
//! on ratatui's `TestBackend`.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use chrono::{DateTime, FixedOffset};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use zmail_core::{
    AccountSummary, MailboxRole, MailboxSelection, SearchScope, SyncPhase, ThreadSummary,
    VirtualMailbox,
    preferences::{CopyClipboard, TerminalPreferences},
    text::count_noun,
    time::HourCycle,
};
use zmail_notify::NewMailBatch;
use zmail_security::terminal::sanitize_line;
use zmail_store::LocalStore;
use zmail_sync::{SYNCS_ELSEWHERE, SyncService};

use crate::{
    data::{Folder, FolderRow, IndexEntry, folder_rows, index_entries},
    links::misleading_host,
    message::{self, Conversation},
    pane::{DrawnPane, PaneId, Point, Selection, logical_line, selected_text, word_at},
    text,
};

/// How often the store is read again, as the GUI does.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// Conversations read into the Message Index at a time.
const INDEX_PAGE: usize = 500;
/// Clicks closer together than this count as a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(400);
/// Lines the mouse wheel scrolls.
const WHEEL_LINES: usize = 3;
/// The narrowest terminal with room for the preview pane.
pub const PREVIEW_MIN_WIDTH: u16 = 120;

/// The current time with the viewer's UTC offset.
pub type Clock = Arc<dyn Fn() -> DateTime<FixedOffset> + Send + Sync>;

/// What the terminal and session can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Environment {
    /// Colours are allowed (`NO_COLOR` is not set).
    pub color: bool,
    /// The terminal takes UTF-8, so box drawing can be used.
    pub unicode: bool,
    /// The session runs over SSH.
    pub over_ssh: bool,
    /// Running on macOS.
    pub macos: bool,
}

impl Environment {
    /// Reads the environment: `NO_COLOR`, the locale's character set, and
    /// `SSH_CONNECTION`/`SSH_TTY`.
    pub fn detect() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.is_empty());
        let unicode = cfg!(windows)
            || locale.is_none_or(|locale| {
                let locale = locale.to_ascii_lowercase();
                locale.contains("utf-8") || locale.contains("utf8")
            });
        Self {
            color: !set("NO_COLOR"),
            unicode,
            over_ssh: crate::clipboard::over_ssh(),
            macos: cfg!(target_os = "macos"),
        }
    }
}

/// Something the app asks of the terminal or the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Put this text on the clipboard.
    Copy(String),
    /// Ring the terminal bell.
    Bell,
    /// Turn mouse capture on or off.
    MouseCapture(bool),
    /// Open this (checked) address with the system's opener.
    Open(String),
    /// Clear and redraw the whole screen.
    Redraw,
}

/// The screens, Alpine's hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The Main Menu.
    MainMenu,
    /// The Folder List.
    FolderList,
    /// The Message Index.
    Index,
    /// Message Text.
    Message,
    /// Setup.
    Setup,
    /// Help.
    Help,
}

/// A command: what a key or a click on the command bar does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    /// Help for this screen.
    Help,
    /// Up a level.
    Back,
    /// Open what the cursor is on.
    Select,
    /// The previous item.
    Prev,
    /// The next item.
    Next,
    /// Up a page.
    PageUp,
    /// Down a page.
    PageDown,
    /// Search (WhereIs).
    WhereIs,
    /// Group the index by conversation, or not.
    Threads,
    /// Show or hide the preview pane.
    Preview,
    /// To the Main Menu.
    MainMenu,
    /// To the Folder List.
    FolderList,
    /// To the Message Index.
    Index,
    /// To Setup.
    Setup,
    /// Compose (a later phase).
    Compose,
    /// The address book (a later phase).
    AddressBook,
    /// Quit, after asking.
    Quit,
    /// The next page of commands.
    OtherCmds,
    /// The next link in the message.
    NextLink,
    /// The previous link in the message.
    PrevLink,
    /// Set the mark for a keyboard selection.
    Mark,
    /// Copy the selection.
    Copy,
    /// To the first line or item.
    Home,
    /// To the last line or item.
    End,
}

/// A command bar entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandEntry {
    /// The key shown.
    pub key: &'static str,
    /// What it does.
    pub label: &'static str,
    /// The command.
    pub cmd: Cmd,
}

const fn entry(key: &'static str, label: &'static str, cmd: Cmd) -> CommandEntry {
    CommandEntry { key, label, cmd }
}

/// A question or a text field on the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// "Really quit Zmail?"
    Quit,
    /// Open link `index`, after showing its address.
    OpenLink(usize),
    /// A text field.
    Text {
        /// What it is for.
        purpose: TextPurpose,
        /// What has been typed or pasted.
        input: String,
    },
}

/// What a text field on the status line is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextPurpose {
    /// Searching the Message Index.
    SearchIndex,
    /// Finding text in Message Text.
    FindInMessage,
    /// Finding a folder by name.
    FindFolder,
    /// Setup's copy command.
    CopyCommand,
}

impl TextPurpose {
    /// The field's label.
    pub fn label(self) -> &'static str {
        match self {
            Self::SearchIndex => "WhereIs (words, from:, has:attachment, before:)",
            Self::FindInMessage => "Find in message",
            Self::FindFolder => "Folder name",
            Self::CopyCommand => "Copy command (empty for the default)",
        }
    }
}

/// Setup's rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupRow {
    /// Mouse capture.
    Mouse,
    /// The new-mail bell.
    Bell,
    /// The preview pane.
    Preview,
    /// Conversation grouping.
    Threads,
    /// ASCII drawing.
    Ascii,
    /// OSC 52's clipboard.
    Clipboard,
    /// The copy command.
    CopyCommand,
}

/// Setup's rows, in order.
pub const SETUP_ROWS: [SetupRow; 7] = [
    SetupRow::Mouse,
    SetupRow::Bell,
    SetupRow::Preview,
    SetupRow::Threads,
    SetupRow::Ascii,
    SetupRow::Clipboard,
    SetupRow::CopyCommand,
];

/// The Main Menu's items: key, name, description, command.
pub const MENU_ITEMS: [(&str, &str, &str, Cmd); 7] = [
    ("?", "HELP", "Get help using Zmail", Cmd::Help),
    (
        "C",
        "COMPOSE MESSAGE",
        "Compose a message (not in the terminal yet)",
        Cmd::Compose,
    ),
    (
        "I",
        "MESSAGE INDEX",
        "View messages in current folder",
        Cmd::Index,
    ),
    (
        "L",
        "FOLDER LIST",
        "Select a folder to view",
        Cmd::FolderList,
    ),
    (
        "A",
        "ADDRESS BOOK",
        "Address book (not in the terminal yet)",
        Cmd::AddressBook,
    ),
    ("S", "SETUP", "Configure Zmail options", Cmd::Setup),
    ("Q", "QUIT", "Leave Zmail", Cmd::Quit),
];

/// The Message Index's state.
#[derive(Debug, Default)]
pub struct IndexState {
    /// The conversations read so far, newest first.
    pub threads: Vec<ThreadSummary>,
    /// Conversations in the folder.
    pub total: usize,
    /// The rows shown.
    pub entries: Vec<IndexEntry>,
    /// The row the cursor is on.
    pub cursor: usize,
    /// The search the index shows, instead of the folder.
    pub search: Option<String>,
}

/// Message Text's state.
#[derive(Debug)]
pub struct MessageState {
    /// The conversation.
    pub conversation: Conversation,
    /// The message to show first, for a single-message row.
    pub focus: Option<i64>,
    /// Whether the focus has been scrolled to.
    pub focused: bool,
    /// The link Tab moved to.
    pub selected_link: Option<usize>,
    /// The text Find looks for.
    pub find: Option<String>,
    /// The keyboard selection's caret, while the mark is set.
    pub caret: Option<Point>,
}

/// The mouse button's state.
#[derive(Debug, Default)]
struct MouseState {
    /// Where the left button went down: pane, cell, and how many clicks
    /// in a row it is.
    press: Option<(PaneId, Point, u8)>,
    /// The button moved since it went down.
    dragged: bool,
    /// The last press, for counting double and triple clicks.
    last_press: Option<(Instant, u16, u16, u8)>,
    /// Where the pointer is.
    position: (u16, u16),
}

/// The terminal UI.
pub struct App {
    pub(crate) store: Arc<LocalStore>,
    pub(crate) sync: Option<SyncService>,
    pub(crate) runtime: Option<tokio::runtime::Handle>,
    pub(crate) env: Environment,
    pub(crate) prefs: TerminalPreferences,
    pub(crate) clock: Clock,
    pub(crate) hour_cycle: HourCycle,
    pub(crate) version: String,

    pub(crate) screen: Screen,
    /// Where Help returns to.
    help_from: Screen,
    pub(crate) menu_cursor: usize,
    pub(crate) accounts: Vec<AccountSummary>,
    pub(crate) folders: Vec<FolderRow>,
    pub(crate) folder_cursor: usize,
    pub(crate) folder: Option<Folder>,
    pub(crate) index: IndexState,
    pub(crate) message: Option<MessageState>,
    /// The conversation the preview pane shows.
    pub(crate) preview: Option<Conversation>,
    pub(crate) setup_cursor: usize,
    pub(crate) prompt: Option<Prompt>,
    pub(crate) status: Option<String>,
    pub(crate) command_page: usize,

    /// The panes of the last frame.
    pub(crate) panes: Vec<DrawnPane>,
    /// The command bar's entries on screen, for clicks.
    pub(crate) command_targets: Vec<(ratatui::layout::Rect, Cmd)>,
    /// Each pane's first line shown.
    pub(crate) scroll: HashMap<PaneId, usize>,
    /// Panes whose cursor moved and must be scrolled into view.
    pub(crate) reveal: HashSet<PaneId>,
    pub(crate) selection: Option<Selection>,
    mouse: MouseState,
    /// The terminal size at the last frame.
    pub(crate) size: (u16, u16),

    effects: Vec<Effect>,
    last_refresh: Instant,
    arrivals: Option<mpsc::Receiver<NewMailBatch>>,
    /// Messages from work running elsewhere (a copy command, the server
    /// search).
    notices: (mpsc::Sender<Notice>, mpsc::Receiver<Notice>),
    /// Threads whose bodies have been asked for.
    hydrating: HashSet<i64>,
    quit: bool,
}

/// News from work running elsewhere.
#[derive(Debug)]
pub enum Notice {
    /// Show this on the status line.
    Status(String),
    /// The server search for this query finished.
    ServerSearchDone {
        /// The query.
        query: String,
        /// Hits it added, or what went wrong.
        result: Result<usize, String>,
    },
}

/// What [`App::new`] needs.
pub struct AppOptions {
    /// The open store.
    pub store: Arc<LocalStore>,
    /// The sync service, when there is one (not in tests).
    pub sync: Option<SyncService>,
    /// The runtime for background work (the server search).
    pub runtime: Option<tokio::runtime::Handle>,
    /// What the terminal can do.
    pub env: Environment,
    /// The current time.
    pub clock: Clock,
    /// 12- or 24-hour times.
    pub hour_cycle: HourCycle,
    /// Zmail's version, for the title bar.
    pub version: String,
    /// New-mail batches from the TUI notifier.
    pub arrivals: Option<mpsc::Receiver<NewMailBatch>>,
}

impl App {
    /// Opens on the Main Menu with the store's accounts and folders read.
    pub fn new(options: AppOptions) -> Self {
        let prefs = options
            .store
            .ui_preferences()
            .map(|preferences| preferences.terminal)
            .unwrap_or_default();
        let mut app = Self {
            store: options.store,
            sync: options.sync,
            runtime: options.runtime,
            env: options.env,
            prefs,
            clock: options.clock,
            hour_cycle: options.hour_cycle,
            version: options.version,
            screen: Screen::MainMenu,
            help_from: Screen::MainMenu,
            menu_cursor: 2,
            accounts: Vec::new(),
            folders: Vec::new(),
            folder_cursor: 0,
            folder: None,
            index: IndexState::default(),
            message: None,
            preview: None,
            setup_cursor: 0,
            prompt: None,
            status: None,
            command_page: 0,
            panes: Vec::new(),
            command_targets: Vec::new(),
            scroll: HashMap::new(),
            reveal: HashSet::new(),
            selection: None,
            mouse: MouseState::default(),
            size: (80, 24),
            effects: Vec::new(),
            last_refresh: Instant::now(),
            arrivals: options.arrivals,
            notices: mpsc::channel(),
            hydrating: HashSet::new(),
            quit: false,
        };
        app.load_folders();
        app.open_default_folder();
        if app.accounts.is_empty() {
            app.status = Some(
                "No accounts yet: add one in the Zmail window, or with zmail-app add-imap or \
                 add-jmap"
                    .to_owned(),
            );
        }
        app
    }

    /// Whether the user has quit.
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// The effects asked for since the last call.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    /// The settings in use.
    pub fn preferences(&self) -> &TerminalPreferences {
        &self.prefs
    }

    /// A sender for notices from work started elsewhere.
    pub fn notice_sender(&self) -> mpsc::Sender<Notice> {
        self.notices.0.clone()
    }

    /// How long the event loop may wait for input before calling
    /// [`App::tick`].
    pub fn tick_interval(&self) -> Duration {
        if self.mouse.press.is_some() {
            // Keep scrolling while a drag rests past a pane's edge.
            Duration::from_millis(50)
        } else {
            Duration::from_millis(250)
        }
    }

    /// The current time.
    pub(crate) fn now(&self) -> DateTime<FixedOffset> {
        (self.clock)()
    }

    // ------------------------------------------------------------------
    // Reading the store
    // ------------------------------------------------------------------

    fn load_folders(&mut self) {
        self.accounts = self.store.list_accounts().unwrap_or_default();
        match folder_rows(&self.store, &self.accounts) {
            Ok(rows) => self.folders = rows,
            Err(error) => self.report(error),
        }
    }

    /// The folder the GUI last showed, or the first one.
    fn open_default_folder(&mut self) {
        let last = self
            .store
            .ui_preferences()
            .ok()
            .and_then(|preferences| preferences.last_mailbox);
        let position = last
            .and_then(|last| self.folder_position(&last))
            .or_else(|| {
                self.folder_position(&MailboxSelection::Virtual(VirtualMailbox::Unified(
                    MailboxRole::Inbox,
                )))
            })
            .or_else(|| {
                self.folders
                    .iter()
                    .position(|row| matches!(row, FolderRow::Folder(_)))
            });
        if let Some(position) = position {
            self.folder_cursor = position;
            if let Some(FolderRow::Folder(folder)) = self.folders.get(position) {
                self.folder = Some(folder.clone());
                self.load_index(None);
            }
        }
    }

    fn folder_position(&self, selection: &MailboxSelection) -> Option<usize> {
        self.folders.iter().position(
            |row| matches!(row, FolderRow::Folder(folder) if folder.selection == *selection),
        )
    }

    /// Reads the index again, keeping the cursor on the row it was on
    /// (`keep`, else the same thread).
    fn load_index(&mut self, keep: Option<(i64, Option<i64>)>) {
        let Some(folder) = self.folder.clone() else {
            self.index = IndexState::default();
            return;
        };
        let keep = keep.or_else(|| {
            self.index
                .entries
                .get(self.index.cursor)
                .map(|entry| (entry.thread_id, entry.message_id))
        });
        let result = match self.index.search.clone() {
            Some(query) => self.search_entries(&folder, &query),
            None => self.folder_entries(&folder),
        };
        match result {
            Ok(()) => {}
            Err(error) => self.report(error),
        }
        self.index.cursor = keep
            .and_then(|(thread_id, message_id)| {
                self.index
                    .entries
                    .iter()
                    .position(|entry| {
                        entry.thread_id == thread_id
                            && (message_id.is_none() || entry.message_id == message_id)
                    })
                    .or_else(|| {
                        self.index
                            .entries
                            .iter()
                            .position(|entry| entry.thread_id == thread_id)
                    })
            })
            .unwrap_or(0)
            .min(self.index.entries.len().saturating_sub(1));
    }

    fn folder_entries(&mut self, folder: &Folder) -> anyhow::Result<()> {
        let wanted = self.index.threads.len().max(INDEX_PAGE);
        let threads = self
            .store
            .list_selection_threads(&folder.selection, 0..wanted)?;
        self.index.total = self
            .store
            .selection_counts(&folder.selection)?
            .total_threads;
        self.index.entries = index_entries(&self.store, &threads, self.prefs.group_threads)?;
        self.index.threads = threads;
        Ok(())
    }

    fn search_entries(&mut self, folder: &Folder, query: &str) -> anyhow::Result<()> {
        let hits = if query.trim().is_empty() {
            Vec::new()
        } else {
            self.store.search_threads_in(
                &SearchScope::Folder(folder.selection),
                query,
                INDEX_PAGE,
            )?
        };
        self.index.threads.clear();
        self.index.total = hits.len();
        self.index.entries = hits.iter().map(IndexEntry::from_hit).collect();
        Ok(())
    }

    /// Reads more of the folder when the cursor nears the end of what is
    /// loaded.
    fn load_more_if_needed(&mut self) {
        if self.index.search.is_some()
            || self.index.threads.len() >= self.index.total
            || self.index.cursor + 50 < self.index.entries.len()
        {
            return;
        }
        let Some(folder) = self.folder.clone() else {
            return;
        };
        let from = self.index.threads.len();
        match self
            .store
            .list_selection_threads(&folder.selection, from..from + INDEX_PAGE)
            .and_then(|more| {
                let entries = index_entries(&self.store, &more, self.prefs.group_threads)?;
                Ok((more, entries))
            }) {
            Ok((more, entries)) => {
                self.index.threads.extend(more);
                self.index.entries.extend(entries);
            }
            Err(error) => self.report(error),
        }
    }

    /// Reads the store again: accounts, folders, the index, and the open
    /// conversation when it changed. Runs every [`REFRESH_INTERVAL`].
    pub fn refresh(&mut self) {
        self.last_refresh = Instant::now();
        let current = self.folder.as_ref().map(|folder| folder.selection);
        self.load_folders();
        if let Some(selection) = current {
            match self.folder_position(&selection) {
                Some(position) => {
                    if let Some(FolderRow::Folder(folder)) = self.folders.get(position) {
                        self.folder = Some(folder.clone());
                    }
                    if !matches!(
                        self.folders.get(self.folder_cursor),
                        Some(FolderRow::Folder(_))
                    ) {
                        self.folder_cursor = position;
                    }
                }
                // The folder is gone (the account was removed).
                None => {
                    self.folder = None;
                    self.open_default_folder();
                }
            }
        } else {
            self.open_default_folder();
        }
        self.folder_cursor = self.folder_cursor.min(self.folders.len().saturating_sub(1));
        if !matches!(
            self.prompt,
            Some(Prompt::Text {
                purpose: TextPurpose::SearchIndex,
                ..
            })
        ) {
            self.load_index(None);
        }
        let stale = self.message.as_ref().and_then(|state| {
            let conversation = &state.conversation;
            let count = self
                .store
                .thread_state(conversation.thread_id)
                .ok()
                .flatten()
                .map(|thread| thread.messages.len());
            (count != Some(conversation.message_count) || conversation.missing_bodies)
                .then_some((conversation.thread_id, conversation.account_id))
        });
        if let Some((thread_id, account_id)) = stale {
            self.reload_message(thread_id, account_id);
        }
        // Read the preview again too: its conversation may have grown.
        self.preview = None;
    }

    fn load_conversation(&mut self, thread_id: i64, account_id: i64) -> Option<Conversation> {
        let zone = *self.now().offset();
        match message::load(&self.store, thread_id, account_id, &zone, self.hour_cycle) {
            Ok(Some(conversation)) => {
                if conversation.missing_bodies && self.hydrating.insert(thread_id) {
                    // Fetching a body only reads from the server, so it
                    // runs whichever process syncs.
                    if let Some(sync) = &self.sync {
                        let _ = sync.hydrate_thread_body(account_id, thread_id);
                    }
                }
                Some(conversation)
            }
            Ok(None) => None,
            Err(error) => {
                self.report(error);
                None
            }
        }
    }

    fn reload_message(&mut self, thread_id: i64, account_id: i64) {
        match self.load_conversation(thread_id, account_id) {
            Some(conversation) => {
                if let Some(state) = self.message.as_mut() {
                    state.conversation = conversation;
                }
            }
            None => {
                self.message = None;
                if self.screen == Screen::Message {
                    self.screen = Screen::Index;
                    self.status = Some("That conversation is gone".to_owned());
                }
            }
        }
    }

    /// The conversation for the preview pane, read when the cursor moves.
    pub(crate) fn preview_conversation(&mut self) -> Option<&Conversation> {
        let entry = self.index.entries.get(self.index.cursor)?.clone();
        if self
            .preview
            .as_ref()
            .is_none_or(|preview| preview.thread_id != entry.thread_id)
        {
            self.preview = self.load_conversation(entry.thread_id, entry.account_id);
        }
        self.preview.as_ref()
    }

    /// Whether the preview pane shows, for a terminal `width` wide.
    pub(crate) fn preview_shown(&self, width: u16) -> bool {
        self.screen == Screen::Index && self.prefs.preview_pane && width >= PREVIEW_MIN_WIDTH
    }

    /// Shows what went wrong on the status line, filtered (it may quote a
    /// server).
    fn report(&mut self, error: anyhow::Error) {
        self.status = Some(sanitize_line(&format!("{error:#}")));
    }

    /// The status line's right side: who syncs, and how it goes.
    pub(crate) fn sync_note(&self) -> Option<String> {
        if self.sync.as_ref().is_some_and(|sync| !sync.runs_sync()) {
            return Some(SYNCS_ELSEWHERE.to_owned());
        }
        if let Some(account) = self.accounts.iter().find(|account| {
            matches!(
                account.sync_status.phase,
                SyncPhase::Connecting | SyncPhase::Discovering | SyncPhase::Syncing
            )
        }) {
            return Some(format!("Syncing {}…", sanitize_line(&account.display_name)));
        }
        self.accounts
            .iter()
            .find(|account| account.sync_status.phase == SyncPhase::Failed)
            .map(|account| format!("{}: sync failed", sanitize_line(&account.display_name)))
    }

    // ------------------------------------------------------------------
    // Time passing
    // ------------------------------------------------------------------

    /// Called after every event and at least every
    /// [`App::tick_interval`]: reads the store every [`REFRESH_INTERVAL`],
    /// reports new mail and finished background work, and keeps a drag
    /// past a pane's edge scrolling.
    pub fn tick(&mut self) {
        let batches: Vec<NewMailBatch> = self
            .arrivals
            .as_ref()
            .map(|arrivals| arrivals.try_iter().collect())
            .unwrap_or_default();
        let mut refresh = self.last_refresh.elapsed() >= REFRESH_INTERVAL;
        for batch in batches {
            self.new_mail(&batch);
            refresh = true;
        }
        while let Ok(notice) = self.notices.1.try_recv() {
            match notice {
                Notice::Status(status) => self.status = Some(status),
                Notice::ServerSearchDone { query, result } => {
                    if self.index.search.as_deref() == Some(query.as_str()) {
                        self.load_index(None);
                    }
                    self.status = Some(match result {
                        Ok(0) => "The server search found nothing more".to_owned(),
                        Ok(found) => format!(
                            "The server search found {}",
                            count_noun(found, "more conversation", "more conversations")
                        ),
                        Err(error) => format!("Server search failed: {}", sanitize_line(&error)),
                    });
                }
            }
        }
        if refresh {
            self.refresh();
        }
        self.drag_past_edge();
    }

    fn new_mail(&mut self, batch: &NewMailBatch) {
        let count = batch.arrivals.len();
        if count == 0 {
            return;
        }
        let mut status = format!(
            "{} in {}",
            count_noun(count, "new message", "new messages"),
            sanitize_line(&batch.account)
        );
        if let Some(newest) = batch.arrivals.first() {
            status.push_str(": ");
            status.push_str(&sanitize_line(&newest.subject));
        }
        self.status = Some(status);
        if self.prefs.bell {
            self.effects.push(Effect::Bell);
        }
    }

    // ------------------------------------------------------------------
    // Events
    // ------------------------------------------------------------------

    /// Handles one terminal event.
    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.on_key(key),
            Event::Mouse(mouse) => self.on_mouse(mouse),
            Event::Paste(text) => self.on_paste(&text),
            Event::Resize(width, height) => self.size = (width, height),
            _ => {}
        }
    }

    fn on_paste(&mut self, pasted: &str) {
        // A paste is text for the focused field, never commands.
        match self.prompt.as_mut() {
            Some(Prompt::Text { input, purpose }) => {
                input.push_str(&sanitize_line(pasted));
                let purpose = *purpose;
                self.text_changed(purpose);
            }
            _ => self.status = Some("Pasting works in text fields only".to_owned()),
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.prompt.is_some() {
            self.prompt_key(key);
            return;
        }
        self.status = None;
        if control && key.code == KeyCode::Char('l') {
            self.effects.push(Effect::Redraw);
            return;
        }
        // The keyboard selection in Message Text takes the arrow keys.
        if self.keyboard_selection_key(key) {
            return;
        }
        if key.code == KeyCode::Esc {
            self.selection = None;
            if let Some(state) = self.message.as_mut() {
                state.find = None;
                state.selected_link = None;
            }
            return;
        }
        if let Some(cmd) = self.command_for(key) {
            self.run(cmd);
        }
    }

    /// The command `key` gives on this screen. Letters work in either case.
    fn command_for(&self, key: KeyEvent) -> Option<Cmd> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let in_message = self.screen == Screen::Message;
        if control {
            return match key.code {
                KeyCode::Char(' ' | '@') if in_message => Some(Cmd::Mark),
                KeyCode::Char('c') if self.selection.is_some() => Some(Cmd::Copy),
                _ => None,
            };
        }
        let cmd = match key.code {
            KeyCode::Char(ch) => match ch.to_ascii_lowercase() {
                '?' => Cmd::Help,
                '<' | ',' => Cmd::Back,
                '>' | '.' => Cmd::Select,
                'p' => Cmd::Prev,
                'n' => Cmd::Next,
                '-' => Cmd::PageUp,
                ' ' => Cmd::PageDown,
                'w' => Cmd::WhereIs,
                '/' => Cmd::Threads,
                'v' => Cmd::Preview,
                'm' => Cmd::MainMenu,
                'l' => Cmd::FolderList,
                'i' => Cmd::Index,
                'q' => Cmd::Quit,
                'o' => Cmd::OtherCmds,
                'y' => Cmd::Copy,
                's' if self.screen == Screen::MainMenu => Cmd::Setup,
                'c' if self.screen == Screen::MainMenu => Cmd::Compose,
                'a' if self.screen == Screen::MainMenu => Cmd::AddressBook,
                _ => return None,
            },
            KeyCode::Enter | KeyCode::Right => Cmd::Select,
            KeyCode::Left => Cmd::Back,
            KeyCode::Up => Cmd::Prev,
            KeyCode::Down => Cmd::Next,
            KeyCode::PageUp => Cmd::PageUp,
            KeyCode::PageDown => Cmd::PageDown,
            KeyCode::Tab if in_message => Cmd::NextLink,
            KeyCode::BackTab if in_message => Cmd::PrevLink,
            KeyCode::Home => Cmd::Home,
            KeyCode::End => Cmd::End,
            _ => return None,
        };
        Some(cmd)
    }

    /// The command bar's entries for this screen, every page.
    pub fn commands(&self) -> Vec<CommandEntry> {
        use Cmd::*;
        match self.prompt {
            Some(Prompt::Quit) => {
                return vec![entry("Y", "Yes", Select), entry("N", "No", Back)];
            }
            Some(Prompt::OpenLink(_)) => {
                return vec![
                    entry("Y", "Open", Select),
                    entry("C", "Copy", Copy),
                    entry("N", "Cancel", Back),
                ];
            }
            Some(Prompt::Text { .. }) => {
                return vec![entry("Ret", "Accept", Select), entry("Esc", "Cancel", Back)];
            }
            None => {}
        }
        match self.screen {
            Screen::MainMenu => vec![
                entry("?", "Help", Help),
                entry("P", "PrevCmd", Prev),
                entry(">", "[Select]", Select),
                entry("N", "NextCmd", Next),
                entry("L", "FldrList", FolderList),
                entry("I", "Index", Index),
                entry("S", "Setup", Setup),
                entry("Q", "Quit", Quit),
            ],
            Screen::FolderList => vec![
                entry("?", "Help", Help),
                entry("<", "Main Menu", Back),
                entry(">", "[ViewFldr]", Select),
                entry("P", "PrevFldr", Prev),
                entry("N", "NextFldr", Next),
                entry("-", "PrevPage", PageUp),
                entry("Spc", "NextPage", PageDown),
                entry("W", "WhereIs", WhereIs),
                entry("M", "Main Menu", MainMenu),
                entry("I", "Index", Index),
                entry("Q", "Quit", Quit),
            ],
            Screen::Index => vec![
                entry("?", "Help", Help),
                entry("<", "FldrList", Back),
                entry(">", "[ViewMsg]", Select),
                entry("P", "PrevMsg", Prev),
                entry("N", "NextMsg", Next),
                entry("-", "PrevPage", PageUp),
                entry("Spc", "NextPage", PageDown),
                entry("W", "WhereIs", WhereIs),
                entry(
                    "/",
                    if self.prefs.group_threads {
                        "Ungroup"
                    } else {
                        "Group"
                    },
                    Threads,
                ),
                entry("V", "Preview", Preview),
                entry("M", "Main Menu", MainMenu),
                entry("Y", "Copy", Copy),
                entry("Q", "Quit", Quit),
            ],
            Screen::Message => vec![
                entry("?", "Help", Help),
                entry("<", "MsgIndex", Back),
                entry("P", "PrevMsg", Prev),
                entry("N", "NextMsg", Next),
                entry("-", "PrevPage", PageUp),
                entry("Spc", "NextPage", PageDown),
                entry("W", "WhereIs", WhereIs),
                entry("Tab", "NextLink", NextLink),
                entry("Ret", "OpenLink", Select),
                entry("^Spc", "Mark", Mark),
                entry("Y", "Copy", Copy),
                entry("M", "Main Menu", MainMenu),
                entry("L", "FldrList", FolderList),
                entry("Q", "Quit", Quit),
            ],
            Screen::Setup => vec![
                entry("?", "Help", Help),
                entry("<", "Main Menu", Back),
                entry(">", "[Change]", Select),
                entry("P", "Prev", Prev),
                entry("N", "Next", Next),
                entry("Q", "Quit", Quit),
            ],
            Screen::Help => vec![
                entry("<", "Back", Back),
                entry("-", "PrevPage", PageUp),
                entry("Spc", "NextPage", PageDown),
                entry("M", "Main Menu", MainMenu),
                entry("Q", "Quit", Quit),
            ],
        }
    }

    /// How many command-bar slots fit, and the commands on the current
    /// page (with `O OTHER CMDS` last when there are more pages).
    pub fn command_bar(&self, width: u16) -> Vec<CommandEntry> {
        let columns = usize::from(width / 13).clamp(2, 8);
        let slots = columns * 2;
        let all = self.commands();
        if all.len() <= slots {
            return all;
        }
        let per_page = slots - 1;
        let pages = all.len().div_ceil(per_page);
        let page = self.command_page % pages;
        let mut shown: Vec<CommandEntry> = all
            .into_iter()
            .skip(page * per_page)
            .take(per_page)
            .collect();
        shown.push(entry("O", "OTHER CMDS", Cmd::OtherCmds));
        shown
    }

    /// Runs a command on the current screen.
    pub fn run(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Help => {
                if self.screen != Screen::Help {
                    self.help_from = self.screen;
                    self.goto(Screen::Help);
                }
            }
            Cmd::Back => self.back(),
            Cmd::Select => self.select(),
            Cmd::Prev => self.step(-1),
            Cmd::Next => self.step(1),
            Cmd::PageUp => self.page(-1),
            Cmd::PageDown => self.page(1),
            Cmd::WhereIs => self.where_is(),
            Cmd::Threads if self.screen == Screen::Index => self.toggle_threads(),
            Cmd::Preview if self.screen == Screen::Index => self.toggle_preview(),
            Cmd::Threads | Cmd::Preview => {}
            Cmd::MainMenu => self.goto(Screen::MainMenu),
            Cmd::FolderList => self.goto(Screen::FolderList),
            Cmd::Index => {
                if self.folder.is_some() {
                    self.goto(Screen::Index);
                } else {
                    self.status = Some("Choose a folder first".to_owned());
                    self.goto(Screen::FolderList);
                }
            }
            Cmd::Setup => self.goto(Screen::Setup),
            Cmd::Compose => {
                self.status = Some(
                    "Compose isn't in the terminal yet: use the Zmail window, or zmail-app \
                     compose"
                        .to_owned(),
                );
            }
            Cmd::AddressBook => {
                self.status = Some("The address book isn't in the terminal yet".to_owned());
            }
            Cmd::Quit => self.prompt = Some(Prompt::Quit),
            Cmd::OtherCmds => self.command_page += 1,
            Cmd::NextLink => self.move_link(1),
            Cmd::PrevLink => self.move_link(-1),
            Cmd::Mark => self.set_mark(),
            Cmd::Copy => self.copy_selection(),
            Cmd::Home | Cmd::End => {
                let by = if cmd == Cmd::Home {
                    -(isize::MAX / 2)
                } else {
                    isize::MAX / 2
                };
                match self.screen {
                    Screen::Message => self.scroll_pane(PaneId::Message, by),
                    Screen::Help => self.scroll_pane(PaneId::Help, by),
                    _ => self.step(by),
                }
            }
        }
    }

    fn goto(&mut self, screen: Screen) {
        if self.screen != screen {
            self.selection = None;
            self.command_page = 0;
        }
        self.screen = screen;
        match screen {
            Screen::FolderList => {
                self.reveal.insert(PaneId::Folders);
            }
            Screen::Index => {
                self.reveal.insert(PaneId::Index);
            }
            Screen::Help => {
                self.scroll.insert(PaneId::Help, 0);
            }
            _ => {}
        }
    }

    fn back(&mut self) {
        match self.screen {
            Screen::MainMenu => {}
            Screen::FolderList | Screen::Setup => self.goto(Screen::MainMenu),
            Screen::Index => {
                if self.index.search.take().is_some() {
                    self.load_index(None);
                    self.status = Some("Showing the whole folder again".to_owned());
                } else {
                    self.goto(Screen::FolderList);
                }
            }
            Screen::Message => {
                self.message = None;
                self.goto(Screen::Index);
            }
            Screen::Help => self.goto(self.help_from),
        }
    }

    fn select(&mut self) {
        match self.screen {
            Screen::MainMenu => {
                if let Some((_, _, _, cmd)) = MENU_ITEMS.get(self.menu_cursor) {
                    self.run(*cmd);
                }
            }
            Screen::FolderList => self.open_folder(self.folder_cursor),
            Screen::Index => self.open_entry(self.index.cursor),
            Screen::Message => {
                if let Some(link) = self.message.as_ref().and_then(|state| state.selected_link) {
                    self.prompt = Some(Prompt::OpenLink(link));
                }
            }
            Screen::Setup => self.change_setting(self.setup_cursor),
            Screen::Help => {}
        }
    }

    /// Moves the cursor (or in Message Text, to the next conversation).
    fn step(&mut self, by: isize) {
        match self.screen {
            Screen::MainMenu => {
                self.menu_cursor = offset(self.menu_cursor, by, MENU_ITEMS.len());
            }
            Screen::FolderList => {
                // Home and End: the first or last folder.
                if by.unsigned_abs() >= self.folders.len() {
                    let folders = self
                        .folders
                        .iter()
                        .enumerate()
                        .filter(|(_, row)| matches!(row, FolderRow::Folder(_)))
                        .map(|(index, _)| index);
                    let end = if by < 0 { folders.min() } else { folders.max() };
                    if let Some(end) = end {
                        self.folder_cursor = end;
                        self.reveal.insert(PaneId::Folders);
                    }
                    return;
                }
                // Skip collection titles and gaps.
                let mut cursor = self.folder_cursor;
                loop {
                    let next = offset(cursor, by, self.folders.len());
                    if next == cursor {
                        break;
                    }
                    cursor = next;
                    if matches!(self.folders.get(cursor), Some(FolderRow::Folder(_))) {
                        self.folder_cursor = cursor;
                        break;
                    }
                }
                self.reveal.insert(PaneId::Folders);
            }
            Screen::Index => self.move_index(by),
            Screen::Message => {
                let before = self.index.cursor;
                self.move_index(by);
                if self.index.cursor == before {
                    self.status = Some(
                        if by > 0 {
                            "Already on the last message"
                        } else {
                            "Already on the first message"
                        }
                        .to_owned(),
                    );
                } else {
                    self.open_entry(self.index.cursor);
                }
            }
            Screen::Setup => self.setup_cursor = offset(self.setup_cursor, by, SETUP_ROWS.len()),
            Screen::Help => self.scroll_pane(PaneId::Help, by),
        }
    }

    fn move_index(&mut self, by: isize) {
        self.index.cursor = offset(self.index.cursor, by, self.index.entries.len());
        self.reveal.insert(PaneId::Index);
        self.load_more_if_needed();
    }

    fn page(&mut self, direction: isize) {
        let pane = match self.screen {
            Screen::Message => PaneId::Message,
            Screen::Help => PaneId::Help,
            Screen::Index => PaneId::Index,
            Screen::FolderList => PaneId::Folders,
            _ => return,
        };
        let height = self.pane(pane).map_or(10, |pane| pane.height()).max(2);
        let by = direction * (height as isize - 1);
        match pane {
            PaneId::Index => self.move_index(by),
            PaneId::Folders => self.step(by),
            _ => self.scroll_pane(pane, by),
        }
    }

    pub(crate) fn pane(&self, id: PaneId) -> Option<&DrawnPane> {
        self.panes.iter().find(|pane| pane.id == id)
    }

    fn scroll_pane(&mut self, id: PaneId, by: isize) {
        let lines = self.pane(id).map_or(0, |pane| pane.lines.len());
        let height = self.pane(id).map_or(1, DrawnPane::height);
        let scroll = self.scroll.entry(id).or_default();
        *scroll = offset(*scroll, by, lines.saturating_sub(height) + 1);
    }

    fn open_folder(&mut self, position: usize) {
        let Some(FolderRow::Folder(folder)) = self.folders.get(position).cloned() else {
            return;
        };
        self.folder_cursor = position;
        let changed = self.folder.as_ref().map(|folder| folder.selection) != Some(folder.selection);
        self.folder = Some(folder.clone());
        if changed {
            self.index = IndexState::default();
            self.preview = None;
            self.scroll.insert(PaneId::Index, 0);
            self.load_index(None);
        }
        self.status = Some(format!(
            "Folder \"{}\" opened with {}",
            folder.name,
            count_noun(self.index.total, "conversation", "conversations")
        ));
        self.goto(Screen::Index);
    }

    fn open_entry(&mut self, position: usize) {
        let Some(entry) = self.index.entries.get(position).cloned() else {
            self.status = Some("No messages here".to_owned());
            return;
        };
        self.index.cursor = position;
        let Some(conversation) = self.load_conversation(entry.thread_id, entry.account_id) else {
            self.status = Some("That conversation is gone".to_owned());
            return;
        };
        self.message = Some(MessageState {
            conversation,
            focus: entry.message_id,
            focused: entry.message_id.is_none(),
            selected_link: None,
            find: None,
            caret: None,
        });
        self.scroll.insert(PaneId::Message, 0);
        self.goto(Screen::Message);
    }

    fn toggle_threads(&mut self) {
        self.prefs.group_threads = !self.prefs.group_threads;
        self.save_preferences();
        self.load_index(None);
        self.status = Some(
            if self.prefs.group_threads {
                "Listing conversations"
            } else {
                "Listing single messages"
            }
            .to_owned(),
        );
    }

    fn toggle_preview(&mut self) {
        self.prefs.preview_pane = !self.prefs.preview_pane;
        self.save_preferences();
        self.status = Some(if !self.prefs.preview_pane {
            "Preview pane off".to_owned()
        } else if self.size.0 < PREVIEW_MIN_WIDTH {
            format!("The preview pane shows in terminals {PREVIEW_MIN_WIDTH} columns or wider")
        } else {
            "Preview pane on".to_owned()
        });
    }

    fn save_preferences(&mut self) {
        let terminal = self.prefs.clone();
        if let Err(error) = self
            .store
            .update_ui_preferences(move |preferences| preferences.terminal = terminal)
        {
            self.report(error);
        }
    }

    fn change_setting(&mut self, row: usize) {
        let Some(setting) = SETUP_ROWS.get(row).copied() else {
            return;
        };
        self.setup_cursor = row;
        match setting {
            SetupRow::Mouse => {
                self.prefs.mouse = !self.prefs.mouse;
                self.effects.push(Effect::MouseCapture(self.prefs.mouse));
            }
            SetupRow::Bell => self.prefs.bell = !self.prefs.bell,
            SetupRow::Preview => self.prefs.preview_pane = !self.prefs.preview_pane,
            SetupRow::Threads => {
                self.prefs.group_threads = !self.prefs.group_threads;
                self.load_index(None);
            }
            SetupRow::Ascii => self.prefs.ascii = !self.prefs.ascii,
            SetupRow::Clipboard => {
                self.prefs.copy_clipboard = match self.prefs.copy_clipboard {
                    CopyClipboard::System => CopyClipboard::Primary,
                    CopyClipboard::Primary => CopyClipboard::System,
                };
            }
            SetupRow::CopyCommand => {
                self.prompt = Some(Prompt::Text {
                    purpose: TextPurpose::CopyCommand,
                    input: self.prefs.copy_command.clone().unwrap_or_default(),
                });
                return;
            }
        }
        self.save_preferences();
    }

    fn where_is(&mut self) {
        let purpose = match self.screen {
            Screen::Index => TextPurpose::SearchIndex,
            Screen::Message => TextPurpose::FindInMessage,
            Screen::FolderList => TextPurpose::FindFolder,
            _ => return,
        };
        let input = match purpose {
            TextPurpose::SearchIndex => self.index.search.clone().unwrap_or_default(),
            TextPurpose::FindInMessage => self
                .message
                .as_ref()
                .and_then(|state| state.find.clone())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.prompt = Some(Prompt::Text { purpose, input });
    }

    fn move_link(&mut self, by: isize) {
        let Some(state) = self.message.as_mut() else {
            return;
        };
        let count = state.conversation.links.len();
        if count == 0 {
            self.status = Some("This message has no links".to_owned());
            return;
        }
        state.selected_link = Some(match state.selected_link {
            None if by > 0 => 0,
            None => count - 1,
            Some(link) => (link as isize + by).rem_euclid(count as isize) as usize,
        });
        let link = state.selected_link.unwrap_or_default();
        self.status = Some(format!(
            "Link [{}]: {}  (Enter opens it)",
            link + 1,
            sanitize_line(&state.conversation.links[link].url)
        ));
        self.reveal.insert(PaneId::Message);
    }

    // ------------------------------------------------------------------
    // Prompts
    // ------------------------------------------------------------------

    fn prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = self.prompt.clone() else {
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match prompt {
            Prompt::Quit => match key.code {
                KeyCode::Char('y' | 'Y') => self.quit = true,
                _ => {
                    self.prompt = None;
                    self.status = None;
                }
            },
            Prompt::OpenLink(link) => {
                self.prompt = None;
                let url = self
                    .message
                    .as_ref()
                    .and_then(|state| state.conversation.links.get(link))
                    .map(|link| link.url.clone());
                let Some(url) = url else {
                    return;
                };
                match key.code {
                    KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                        self.effects.push(Effect::Open(url));
                        self.status = Some("Opening the link".to_owned());
                    }
                    KeyCode::Char('c' | 'C') => {
                        self.status = Some(format!(
                            "Copied {}",
                            count_noun(url.chars().count(), "character", "characters")
                        ));
                        self.effects.push(Effect::Copy(url));
                    }
                    _ => self.status = None,
                }
            }
            Prompt::Text { purpose, mut input } => {
                match key.code {
                    KeyCode::Esc => return self.cancel_text(purpose),
                    KeyCode::Char('c' | 'g') if control => return self.cancel_text(purpose),
                    KeyCode::Enter => return self.accept_text(purpose, input),
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char('u') if control => input.clear(),
                    KeyCode::Char(ch) if !control && !ch.is_control() => input.push(ch),
                    _ => return,
                }
                self.prompt = Some(Prompt::Text { purpose, input });
                self.text_changed(purpose);
            }
        }
    }

    /// Incremental search: the index follows the field as it is typed.
    fn text_changed(&mut self, purpose: TextPurpose) {
        if purpose != TextPurpose::SearchIndex {
            return;
        }
        let Some(Prompt::Text { input, .. }) = &self.prompt else {
            return;
        };
        let query = input.clone();
        self.index.search = (!query.trim().is_empty()).then_some(query);
        self.load_index(None);
        self.index.cursor = 0;
        self.reveal.insert(PaneId::Index);
    }

    fn cancel_text(&mut self, purpose: TextPurpose) {
        self.prompt = None;
        if purpose == TextPurpose::SearchIndex && self.index.search.take().is_some() {
            self.load_index(None);
        }
    }

    fn accept_text(&mut self, purpose: TextPurpose, input: String) {
        self.prompt = None;
        let query = input.trim().to_owned();
        match purpose {
            TextPurpose::SearchIndex => {
                if query.is_empty() {
                    self.index.search = None;
                    self.load_index(None);
                    return;
                }
                self.index.search = Some(query.clone());
                self.load_index(None);
                self.status = Some(format!(
                    "{} for \"{query}\"",
                    count_noun(self.index.entries.len(), "match", "matches")
                ));
                self.search_server(query);
            }
            TextPurpose::FindInMessage => self.find_in_message(query),
            TextPurpose::FindFolder => self.find_folder(&query),
            TextPurpose::CopyCommand => {
                self.prefs.copy_command = (!query.is_empty()).then_some(query);
                self.save_preferences();
            }
        }
    }

    /// The server search fallback, in the background, for the accounts
    /// the folder covers. Its hits land in the store, and the index reads
    /// them when it finishes.
    fn search_server(&mut self, query: String) {
        let (Some(sync), Some(runtime), Some(folder)) =
            (self.sync.clone(), self.runtime.clone(), self.folder.clone())
        else {
            return;
        };
        let accounts: Vec<i64> = match folder.account_id {
            Some(account_id) => vec![account_id],
            None => self.accounts.iter().map(|account| account.id).collect(),
        };
        let notices = self.notices.0.clone();
        runtime.spawn(async move {
            let mut found = 0;
            let mut failure = None;
            for account_id in accounts {
                match sync
                    .search_server_in_and_wait(account_id, Some(folder.selection), &query)
                    .await
                {
                    Ok(report) => found += report.hits.saturating_sub(report.unresolved),
                    Err(error) => failure = Some(format!("{error:#}")),
                }
            }
            let result = match failure {
                Some(error) if found == 0 => Err(error),
                _ => Ok(found),
            };
            let _ = notices.send(Notice::ServerSearchDone { query, result });
        });
    }

    fn find_in_message(&mut self, query: String) {
        let Some(state) = self.message.as_mut() else {
            return;
        };
        if query.is_empty() {
            state.find = None;
            return;
        }
        state.find = Some(query.clone());
        let needle = query.to_lowercase();
        let Some(pane) = self.panes.iter().find(|pane| pane.id == PaneId::Message) else {
            return;
        };
        let from = self.scroll.get(&PaneId::Message).copied().unwrap_or(0) + 1;
        let lines = &pane.lines;
        let found = (from..lines.len())
            .chain(0..from.min(lines.len()))
            .find(|&line| lines[line].text().to_lowercase().contains(&needle));
        match found {
            Some(line) => {
                self.scroll.insert(PaneId::Message, line.saturating_sub(2));
            }
            None => self.status = Some(format!("\"{query}\" isn't in this message")),
        }
    }

    fn find_folder(&mut self, query: &str) {
        let needle = query.to_lowercase();
        let count = self.folders.len();
        let found = (1..=count)
            .map(|step| (self.folder_cursor + step) % count.max(1))
            .find(|&position| {
                matches!(&self.folders[position], FolderRow::Folder(folder)
                    if folder.name.to_lowercase().contains(&needle))
            });
        match found {
            Some(position) => {
                self.folder_cursor = position;
                self.reveal.insert(PaneId::Folders);
            }
            None => self.status = Some(format!("No folder named \"{query}\"")),
        }
    }

    // ------------------------------------------------------------------
    // Selection and copy
    // ------------------------------------------------------------------

    fn set_mark(&mut self) {
        let Some(state) = self.message.as_mut() else {
            return;
        };
        if state.caret.take().is_some() {
            self.selection = None;
            self.status = Some("Mark cleared".to_owned());
            return;
        }
        let line = self.scroll.get(&PaneId::Message).copied().unwrap_or(0);
        let point = Point { line, column: 0 };
        state.caret = Some(point);
        self.selection = Some(Selection {
            pane: PaneId::Message,
            anchor: point,
            head: point,
        });
        self.status = Some("Mark set: the arrow keys select, y or Ctrl-C copies".to_owned());
    }

    /// Arrow keys move the caret while the mark is set; `true` when the
    /// key was used.
    fn keyboard_selection_key(&mut self, key: KeyEvent) -> bool {
        if self.screen != Screen::Message {
            return false;
        }
        let Some(caret) = self.message.as_ref().and_then(|state| state.caret) else {
            return false;
        };
        let lines = self
            .pane(PaneId::Message)
            .map_or(0, |pane| pane.lines.len());
        let width = self
            .pane(PaneId::Message)
            .map_or(1, |pane| usize::from(pane.area.width));
        let mut caret = caret;
        match key.code {
            KeyCode::Left if caret.column > 0 => caret.column -= 1,
            KeyCode::Left if caret.line > 0 => {
                caret.line -= 1;
                caret.column = width.saturating_sub(1);
            }
            KeyCode::Right if caret.column + 1 < width => caret.column += 1,
            KeyCode::Right if caret.line + 1 < lines => {
                caret.line += 1;
                caret.column = 0;
            }
            KeyCode::Up => caret.line = caret.line.saturating_sub(1),
            KeyCode::Down => caret.line = (caret.line + 1).min(lines.saturating_sub(1)),
            KeyCode::Home => caret.column = 0,
            KeyCode::End => caret.column = width.saturating_sub(1),
            KeyCode::Left | KeyCode::Right => {}
            KeyCode::Esc => {
                if let Some(state) = self.message.as_mut() {
                    state.caret = None;
                }
                self.selection = None;
                return true;
            }
            _ => return false,
        }
        if let Some(state) = self.message.as_mut() {
            state.caret = Some(caret);
        }
        if let Some(selection) = self.selection.as_mut() {
            selection.head = caret;
        }
        // Keep the caret on screen.
        let height = self.pane(PaneId::Message).map_or(1, DrawnPane::height);
        let scroll = self.scroll.entry(PaneId::Message).or_default();
        if caret.line < *scroll {
            *scroll = caret.line;
        } else if caret.line >= *scroll + height {
            *scroll = caret.line + 1 - height;
        }
        true
    }

    fn copy_selection(&mut self) {
        let Some(selection) = self.selection else {
            if self.screen == Screen::Message {
                self.status =
                    Some("Select text first: drag, or Ctrl-Space and the arrow keys".to_owned());
            }
            return;
        };
        let Some(pane) = self.pane(selection.pane) else {
            return;
        };
        let (start, end) = selection.ordered();
        let text = selected_text(&pane.lines, start, end);
        if let Some(state) = self.message.as_mut() {
            state.caret = None;
        }
        if text.is_empty() {
            return;
        }
        self.status = Some(format!(
            "Copied {}",
            count_noun(text.chars().count(), "character", "characters")
        ));
        self.effects.push(Effect::Copy(text));
    }

    // ------------------------------------------------------------------
    // The mouse
    // ------------------------------------------------------------------

    fn on_mouse(&mut self, mouse: MouseEvent) {
        // Shift leaves the mouse to the terminal's own selection; most
        // terminals don't even report it.
        if mouse.modifiers.contains(KeyModifiers::SHIFT) {
            return;
        }
        // While a question is asked, only its answers on the command bar
        // can be clicked.
        if self.prompt.is_some() {
            if let MouseEventKind::Down(MouseButton::Left) = mouse.kind
                && let Some(cmd) = self.command_at(mouse.column, mouse.row)
            {
                let answer = match (cmd, &self.prompt) {
                    (Cmd::Select, Some(Prompt::Text { .. })) => KeyCode::Enter,
                    (Cmd::Select, _) => KeyCode::Char('y'),
                    (Cmd::Copy, _) => KeyCode::Char('c'),
                    (_, Some(Prompt::Text { .. })) => KeyCode::Esc,
                    _ => KeyCode::Char('n'),
                };
                self.prompt_key(KeyEvent::new(answer, KeyModifiers::NONE));
            }
            return;
        }
        let (x, y) = (mouse.column, mouse.row);
        self.mouse.position = (x, y);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(x, y),
            MouseEventKind::Drag(MouseButton::Left) => self.mouse_drag(x, y),
            MouseEventKind::Up(MouseButton::Left) => self.mouse_up(x, y),
            MouseEventKind::ScrollUp => self.wheel(x, y, -1),
            MouseEventKind::ScrollDown => self.wheel(x, y, 1),
            _ => {}
        }
    }

    /// The command bar entry at (`x`, `y`).
    fn command_at(&self, x: u16, y: u16) -> Option<Cmd> {
        self.command_targets
            .iter()
            .find(|(area, _)| area.contains(ratatui::layout::Position { x, y }))
            .map(|(_, cmd)| *cmd)
    }

    fn mouse_down(&mut self, x: u16, y: u16) {
        if let Some(cmd) = self.command_at(x, y) {
            self.status = None;
            self.run(cmd);
            return;
        }
        let Some(pane) = self.panes.iter().find(|pane| pane.contains(x, y)) else {
            return;
        };
        let (id, point) = (pane.id, pane.point_at(x, y));
        let now = Instant::now();
        let clicks = match self.mouse.last_press {
            Some((at, last_x, last_y, clicks))
                if now.duration_since(at) < MULTI_CLICK && (last_x, last_y) == (x, y) =>
            {
                clicks % 3 + 1
            }
            _ => 1,
        };
        self.mouse.last_press = Some((now, x, y, clicks));
        self.mouse.press = Some((id, point, clicks));
        self.mouse.dragged = false;
        self.selection = match clicks {
            2 => self.word_selection(id, point),
            3 => self.line_selection(id, point),
            _ => None,
        };
    }

    fn word_selection(&self, id: PaneId, point: Point) -> Option<Selection> {
        let text = self.pane(id)?.lines.get(point.line)?.text();
        let (start, end) = word_at(&text, point.column)?;
        Some(Selection {
            pane: id,
            anchor: Point {
                line: point.line,
                column: start,
            },
            head: Point {
                line: point.line,
                column: end,
            },
        })
    }

    fn line_selection(&self, id: PaneId, point: Point) -> Option<Selection> {
        let pane = self.pane(id)?;
        let (first, last) = logical_line(&pane.lines, point.line);
        Some(Selection {
            pane: id,
            anchor: Point {
                line: first,
                column: 0,
            },
            head: Point {
                line: last,
                column: usize::MAX / 2,
            },
        })
    }

    fn mouse_drag(&mut self, x: u16, y: u16) {
        let Some((id, anchor, _)) = self.mouse.press else {
            return;
        };
        let Some(pane) = self.pane(id) else {
            return;
        };
        // Past the top or bottom edge, the pane scrolls.
        let top = pane.area.y;
        let bottom = pane.area.y + pane.area.height;
        if y < top {
            self.scroll_pane(id, -1);
        } else if y >= bottom {
            self.scroll_pane(id, 1);
        }
        self.extend_drag(id, anchor, x, y);
    }

    fn extend_drag(&mut self, id: PaneId, anchor: Point, x: u16, y: u16) {
        let Some(pane) = self.pane(id) else {
            return;
        };
        let scroll = self.scroll.get(&id).copied().unwrap_or(pane.scroll);
        let shown = DrawnPane {
            scroll,
            ..pane.clone()
        };
        let head = shown.point_at(x, y);
        self.mouse.dragged = true;
        self.selection = Some(Selection {
            pane: id,
            anchor,
            head,
        });
    }

    /// While a drag rests past a pane's top or bottom edge, keep
    /// scrolling.
    fn drag_past_edge(&mut self) {
        let Some((id, anchor, _)) = self.mouse.press else {
            return;
        };
        if !self.mouse.dragged {
            return;
        }
        let (x, y) = self.mouse.position;
        let Some(pane) = self.pane(id) else {
            return;
        };
        if y < pane.area.y {
            self.scroll_pane(id, -1);
        } else if y >= pane.area.y + pane.area.height {
            self.scroll_pane(id, 1);
        } else {
            return;
        }
        self.extend_drag(id, anchor, x, y);
    }

    fn mouse_up(&mut self, x: u16, y: u16) {
        let Some((id, point, clicks)) = self.mouse.press.take() else {
            return;
        };
        if self.mouse.dragged || clicks > 1 {
            // Copy on select, as Zellij's `copy_on_select`.
            self.copy_selection();
            return;
        }
        if self.pane(id).is_none_or(|pane| !pane.contains(x, y)) {
            return;
        }
        self.click(id, point);
    }

    /// A single click on `point` of pane `id`.
    fn click(&mut self, id: PaneId, point: Point) {
        self.selection = None;
        let Some(line) = self.pane(id).and_then(|pane| pane.lines.get(point.line)) else {
            return;
        };
        let target = line.target;
        let link = line
            .links
            .iter()
            .find(|(cells, _)| cells.contains(&point.column))
            .map(|(_, link)| *link);
        match id {
            PaneId::Menu => {
                if let Some(item) = target {
                    self.menu_cursor = item;
                    self.select();
                }
            }
            PaneId::Folders => {
                if let Some(row) = target {
                    self.open_folder(row);
                }
            }
            PaneId::Index => {
                if let Some(row) = target {
                    let width = self.size.0;
                    if self.preview_shown(width) && self.index.cursor != row {
                        self.index.cursor = row;
                    } else {
                        self.open_entry(row);
                    }
                }
            }
            PaneId::Message => {
                if let Some(link) = link {
                    if let Some(state) = self.message.as_mut() {
                        state.selected_link = Some(link);
                    }
                    self.prompt = Some(Prompt::OpenLink(link));
                }
            }
            PaneId::Preview => {
                if link.is_some() {
                    // Links open from Message Text, where they are numbered
                    // for the keyboard too.
                    self.open_entry(self.index.cursor);
                }
            }
            PaneId::Setup => {
                if let Some(row) = target {
                    self.change_setting(row);
                }
            }
            PaneId::Help => {}
        }
    }

    fn wheel(&mut self, x: u16, y: u16, direction: isize) {
        let Some(id) = self
            .panes
            .iter()
            .find(|pane| pane.contains(x, y))
            .map(|pane| pane.id)
        else {
            return;
        };
        let by = direction * WHEEL_LINES as isize;
        match id {
            PaneId::Index => self.move_index(by),
            PaneId::Folders => self.step(by),
            PaneId::Menu => self.step(direction),
            PaneId::Setup => {
                self.setup_cursor = offset(self.setup_cursor, direction, SETUP_ROWS.len())
            }
            PaneId::Message | PaneId::Preview | PaneId::Help => self.scroll_pane(id, by),
        }
    }

    /// The question or field on the status line, as text.
    pub(crate) fn prompt_text(&self) -> Option<String> {
        Some(match self.prompt.as_ref()? {
            Prompt::Quit => "Really quit Zmail? (y/n)".to_owned(),
            Prompt::OpenLink(link) => {
                let link = self.message.as_ref()?.conversation.links.get(*link)?;
                let warning = misleading_host(&link.text, &link.url)
                    .map(|shown| format!(" The link text shows {shown}, but it goes elsewhere."))
                    .unwrap_or_default();
                format!(
                    "Open {}?{warning} (y open, c copy, n cancel)",
                    sanitize_line(&link.url)
                )
            }
            Prompt::Text { purpose, input } => format!("{}: {input}", purpose.label()),
        })
    }

    /// The title bar's right side: the folder, and where the cursor is.
    pub(crate) fn title_detail(&self) -> String {
        let folder = self
            .folder
            .as_ref()
            .map(|folder| folder.name.clone())
            .unwrap_or_default();
        let position = match self.screen {
            Screen::Index | Screen::Message if !self.index.entries.is_empty() => {
                let noun = if self.prefs.group_threads || self.index.search.is_some() {
                    "Conv"
                } else {
                    "Msg"
                };
                let total = if self.prefs.group_threads || self.index.search.is_some() {
                    self.index.total.max(self.index.entries.len())
                } else {
                    self.index.entries.len()
                };
                format!("  {noun} {} of {total}", self.index.cursor + 1)
            }
            _ => String::new(),
        };
        let folder = match &self.index.search {
            Some(query) if matches!(self.screen, Screen::Index | Screen::Message) => {
                format!("Search \"{}\" in {folder}", text::truncate(query, 24, "…"))
            }
            _ if folder.is_empty() => String::new(),
            _ => format!("Folder: {folder}"),
        };
        format!("{folder}{position}")
    }
}

/// `position` moved `by`, kept within `0..len`.
fn offset(position: usize, by: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (position as isize + by).clamp(0, len as isize - 1) as usize
}
