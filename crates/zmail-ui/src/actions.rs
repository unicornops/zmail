//! Every command the UI offers, as a GPUI action.
//!
//! Buttons dispatch these actions instead of calling view methods, and the
//! default keymap (`keymaps/default.json`) binds keys to them, so a command
//! behaves the same however it is triggered. The names are `zmail::Name`,
//! which is what the keymap file uses.
//!
//! Contexts, from the outside in (deeper contexts win when two bindings
//! share a keystroke):
//!
//! | Context | Element |
//! |---|---|
//! | `Workspace` | the window root |
//! | `Sidebar` | accounts and mailboxes |
//! | `MessageList` | threads, search results, drafts, outbox |
//! | `Reader` | the open conversation |
//! | `Compose` | the open draft |
//! | `AccountSetup` | the account form |
//! | `TextInput` | any text field (owned by the text input component) |
//!
//! The application commands at the top (Quit, Hide, Minimize, …) have no
//! context: [`crate::menus::init`] handles them app-wide, so they work from
//! the native macOS menu bar even when no Zmail element has focus.

use gpui::{Action, actions};

actions!(
    zmail,
    [
        // ---- Application (handled app-wide) ------------------------------
        /// Quit Zmail.
        Quit,
        /// Hide Zmail (macOS).
        Hide,
        /// Hide every other application (macOS).
        HideOthers,
        /// Show every hidden application (macOS).
        ShowAll,
        /// Minimise the active window.
        Minimize,
        /// Zoom (maximise or restore) the active window.
        Zoom,
        // ---- Workspace --------------------------------------------------
        /// Start a new message.
        NewMessage,
        /// Open the account setup form.
        AddAccount,
        /// Show the selected mailbox's conversations.
        ShowMail,
        /// Show the drafts list.
        ShowDrafts,
        /// Show the outbox.
        ShowOutbox,
        /// Retry sending the selected outbox message.
        RetryOutbox,
        /// Move focus to the search field.
        FocusSearch,
        /// Search local mail for the text in the search field.
        RunSearch,
        /// Sync the selected account.
        SyncAccount,
        /// Sync every account.
        SyncAll,
        /// Switch between the light and dark theme.
        ToggleTheme,
        /// Show or hide the sidebar.
        ToggleSidebar,
        /// In the single-pane layout, go back from the detail pane to the
        /// list.
        GoBack,
        /// Move focus to the next pane (sidebar, list, detail).
        FocusNextPane,
        /// Move focus to the previous pane.
        FocusPreviousPane,
        /// Close the topmost overlay: the account form, the error banner,
        /// the status message, or search.
        Cancel,
        /// Reverse the most recent archive, delete, move, flag or read
        /// change.
        Undo,
        // ---- Lists (Sidebar, MessageList) --------------------------------
        /// Select the next row.
        SelectNext,
        /// Select the previous row.
        SelectPrevious,
        /// Select the first row.
        SelectFirst,
        /// Select the last row.
        SelectLast,
        /// Open the selected row and move focus to where it opened.
        OpenSelected,
        // ---- Conversation (MessageList, Reader) --------------------------
        /// Reply to the sender of the latest message.
        Reply,
        /// Reply to everyone on the latest message.
        ReplyAll,
        /// Forward the latest message.
        Forward,
        /// Archive the selected conversation.
        Archive,
        /// Delete the selected conversation.
        Delete,
        /// Flag or unflag the selected conversation.
        ToggleFlag,
        /// Mark the selected conversation read or unread.
        ToggleRead,
        /// Save every attachment of the selected conversation.
        SaveAttachments,
        // ---- Reader --------------------------------------------------------
        /// Scroll the conversation up a little.
        ScrollUp,
        /// Scroll the conversation down a little.
        ScrollDown,
        /// Scroll the conversation up a page.
        PageUp,
        /// Scroll the conversation down a page.
        PageDown,
        // ---- Forms (Compose, AccountSetup) -------------------------------
        // Tab between fields is the text input's own binding.
        /// Send the open draft.
        Send,
        /// Save the open draft.
        SaveDraft,
        /// Attach the file named in the attachment field.
        AddAttachment,
        /// Save the account form and start the first sync.
        SaveAccount,
    ]
);

/// Move the selected conversation to a mailbox. It needs a mailbox, so it
/// comes from a button (and later the Move to… picker), not the keymap.
#[derive(Clone, Debug, PartialEq, Eq, Action)]
#[action(namespace = zmail, no_json)]
pub struct MoveToMailbox {
    /// Destination mailbox id.
    pub mailbox_id: i64,
}

/// Remove one attachment from the open draft.
#[derive(Clone, Debug, PartialEq, Eq, Action)]
#[action(namespace = zmail, no_json)]
pub struct RemoveAttachment {
    /// Draft attachment id.
    pub attachment_id: i64,
}

/// Send the open draft from another of the account's identities.
#[derive(Clone, Debug, PartialEq, Eq, Action)]
#[action(namespace = zmail, no_json)]
pub struct SelectIdentity {
    /// Identity id.
    pub identity_id: i64,
}

/// Actions the default keymap leaves unbound, and why.
#[cfg(test)]
pub(crate) const ACTIONS_WITHOUT_KEYS: &[(&str, &str)] = &[
    ("zmail::MoveToMailbox", "needs a mailbox argument"),
    ("zmail::RemoveAttachment", "needs an attachment argument"),
    ("zmail::SelectIdentity", "needs an identity argument"),
    ("zmail::RunSearch", "Enter in the search field submits it"),
    (
        "zmail::ShowAll",
        "the macOS menu bar has no standard key for it",
    ),
    (
        "zmail::Zoom",
        "the macOS menu bar has no standard key for it",
    ),
    (
        "zmail::AddAttachment",
        "Enter in the attachment field submits it",
    ),
];
