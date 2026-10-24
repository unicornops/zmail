//! Formatted HTML mail: the web renderer (`zmail-render-web`, ADR 0006).
//!
//! On macOS an expanded HTML message shows its body in a locked-down
//! `WKWebView` placed in the conversation list by GPUI's native window
//! composition. What the page shows is the message prepared by
//! `zmail_render::prepare` (sanitised HTML whose links are `zmail-link:<n>`
//! ids, blocked remote content); a link click comes back as its id and
//! opens the same dialog as a link in the text view. The body is one list item as tall as the measured page;
//! the reader's popovers, dialogs and tooltips draw above it. Other
//! platforms, and messages the user switched to the Simple view (message
//! More menu), keep the text view. A page that cannot be shown falls back
//! to the text view with a notice.
//!
//! Set `ZMAIL_WEB_RENDERER=0` to turn the web renderer off.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use gpui::{AnyElement, AppContext as _, Context, IntoElement, Window, px};
use zmail_core::ThreadMessage;
use zmail_render::{
    MessageRenderer, PrepareOptions, PreparedMessage, RemoteContentDecision, ThemeHint, prepare,
    render_source,
};
use zmail_render_web::{Appearance, WebDocument, WebEvent, WebHost, WebRenderer};
use zmail_store::LocalStore;

use super::ReaderView;
use super::links::{LinkPrompt, link_target};
use crate::components::{Banner, BannerTone, ComponentTheme};
use crate::styles::UiColors;

/// The height a page takes in the list until it is measured.
const UNMEASURED_HEIGHT: f32 = 120.;

/// The reader's web renderer state.
pub(super) struct ReaderWeb {
    /// Whether this build and platform render HTML mail formatted.
    enabled: bool,
    /// Where each message's render source (raw HTML, inline parts) is read.
    store: Arc<LocalStore>,
    /// Created at the first render, which has the window.
    host: Option<WebHost>,
    /// The expanded messages of the open conversation, prepared, with the
    /// body text they were prepared for (a new body prepares again).
    prepared: HashMap<i64, (String, Rc<PreparedMessage>)>,
    /// The app is in a dark theme.
    dark: bool,
    /// Each message's page, while the conversation is open.
    documents: HashMap<i64, (RemoteContentDecision, Rc<WebDocument>)>,
    /// Each page's measured height.
    heights: HashMap<i64, f32>,
    /// Messages switched to the Simple view.
    simple: HashSet<i64>,
    /// Messages whose page could not be shown, and why.
    failed: HashMap<i64, String>,
    /// The message whose page was pressed last: Copy and Select All go to
    /// its page.
    active: Option<i64>,
}

impl ReaderWeb {
    pub(super) fn new(store: Arc<LocalStore>) -> Self {
        let disabled = std::env::var_os("ZMAIL_WEB_RENDERER").is_some_and(|value| value == "0");
        Self {
            // GPUI's test platform has no native composition; view tests
            // use the text view on every OS.
            enabled: zmail_render_web::SUPPORTED && !disabled && !cfg!(test),
            store,
            host: None,
            prepared: HashMap::new(),
            dark: false,
            documents: HashMap::new(),
            heights: HashMap::new(),
            simple: HashSet::new(),
            failed: HashMap::new(),
            active: None,
        }
    }

    /// Forgets everything but `message_ids` (a new conversation keeps
    /// nothing).
    pub(super) fn retain(&mut self, message_ids: &HashSet<i64>) {
        self.prepared.retain(|id, _| message_ids.contains(id));
        self.documents.retain(|id, _| message_ids.contains(id));
        self.heights.retain(|id, _| message_ids.contains(id));
        self.simple.retain(|id| message_ids.contains(id));
        self.failed.retain(|id, _| message_ids.contains(id));
        if self.active.is_some_and(|id| !message_ids.contains(&id)) {
            self.active = None;
        }
        if let Some(host) = &self.host {
            host.retain(|key| message_ids.contains(&(key as i64)));
        }
    }

    /// Hides every page: the reader's list is not on screen.
    pub(super) fn hide_all(&self) {
        if let Some(host) = &self.host {
            host.hide_all();
        }
    }

    /// The host, once created.
    pub(super) fn host(&self) -> Option<&WebHost> {
        self.host.as_ref()
    }

    /// Prepares `message` for the web renderer, once per body: reads its
    /// render source (the raw HTML part and inline parts) from the store
    /// and runs it through `zmail_render::prepare`. Only where the web
    /// renderer runs: elsewhere nothing is prepared and every message
    /// keeps the text view.
    pub(super) fn prepare_message(&mut self, message: &ThreadMessage) {
        if !self.enabled
            || self
                .prepared
                .get(&message.id)
                .is_some_and(|(body, _)| *body == message.rendered_body)
        {
            return;
        }
        let source = match render_source(&self.store, message.id) {
            Ok(Some(source)) => source,
            Ok(None) => return,
            Err(error) => {
                eprintln!(
                    "zmail: message {} shows as text: its HTML could not be read: {error:#}",
                    message.id
                );
                return;
            }
        };
        let options = PrepareOptions {
            theme: if self.dark {
                ThemeHint::Dark
            } else {
                ThemeHint::Light
            },
            ..PrepareOptions::default()
        };
        let prepared = prepare(&source, &options);
        if let Err(zmail_render::HtmlUnavailable::TooComplex(reason)) = prepared.html() {
            eprintln!("zmail: message {} shows as text: {reason:?}", message.id);
        }
        self.prepared.insert(
            message.id,
            (message.rendered_body.clone(), Rc::new(prepared)),
        );
        self.documents.remove(&message.id);
    }

    /// Follows the app theme, for messages prepared from now on.
    pub(super) fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
    }
}

impl ReaderView {
    /// Creates the web host the first time the reader renders in a window.
    pub(super) fn ensure_web_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.web.enabled || self.web.host.is_some() {
            return;
        }
        let reader = cx.entity().downgrade();
        let handle = window.window_handle();
        let app = cx.to_async();
        self.web.host = Some(WebHost::new(move |key, event| {
            // From AppKit, outside GPUI's update cycle: handle it on the
            // next turn of the foreground executor.
            let reader = reader.clone();
            app.spawn(async move |cx| {
                let _ = cx.update_window(handle, |_, window, cx| {
                    reader.update(cx, |reader, cx| {
                        reader.web_event(key as i64, event, window, cx);
                    })
                });
            })
            .detach();
        }));
    }

    /// Whether the web renderer could show `message` formatted at all.
    pub(super) fn can_show_formatted(&self, message: &ThreadMessage) -> bool {
        self.web.enabled
            && self.web.host.as_ref().is_none_or(WebHost::is_available)
            && self
                .web
                .prepared
                .get(&message.id)
                .is_some_and(|(_, prepared)| WebRenderer.accepts(prepared))
    }

    /// Whether `message`'s body shows in a web view.
    pub(super) fn shows_web(&self, message: &ThreadMessage) -> bool {
        self.can_show_formatted(message)
            && !self.web.simple.contains(&message.id)
            && !self.web.failed.contains_key(&message.id)
    }

    /// The web view element for `message`'s body, when it shows formatted.
    pub(super) fn web_body(
        &mut self,
        message: &ThreadMessage,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.shows_web(message) {
            return None;
        }
        let host = self.web.host.clone()?;
        let (_, prepared) = self.web.prepared.get(&message.id)?;
        let appearance = Appearance {
            dark: !ComponentTheme::get(cx).is_light,
            background: colors.surface,
            foreground: colors.foreground,
            link: colors.accent,
        };
        // The user's choice for this message: "Load remote content" (this
        // session) or "Always for this sender" (stored). The message was
        // prepared with remote content blocked; an allowed decision makes
        // Zmail fetch its public https images for the page.
        let remote = RemoteContentDecision::new(
            self.state.remote_content_messages.contains(&message.id),
            message.remote_content_sender_allowed,
        );
        let document = match self.web.documents.get(&message.id) {
            Some((decision, document))
                if document.appearance == appearance && *decision == remote =>
            {
                document.clone()
            }
            _ => {
                let document =
                    Rc::new(WebDocument::from_prepared(prepared, appearance, remote).ok()?);
                self.web
                    .documents
                    .insert(message.id, (remote, document.clone()));
                document
            }
        };
        let height = self
            .web
            .heights
            .get(&message.id)
            .copied()
            .unwrap_or(UNMEASURED_HEIGHT);
        Some(
            host.body(message.id as u64, document, px(height))
                .into_any_element(),
        )
    }

    /// The notice on a message whose page could not be shown.
    pub(super) fn web_failure_banner(&self, message: &ThreadMessage) -> Option<AnyElement> {
        let reason = self.web.failed.get(&message.id)?;
        Some(
            Banner::new(
                ("web-failed", message.id as u64),
                format!(
                    "This message could not be shown formatted ({reason}), so it shows as text."
                ),
            )
            .tone(BannerTone::Caution)
            .into_any_element(),
        )
    }

    /// Switches the message at `index` between the formatted view and the
    /// Simple view. Switching back to formatted also retries a page that
    /// failed.
    pub(super) fn toggle_simple_view(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(message) = self
            .state
            .thread
            .as_ref()
            .and_then(|thread| thread.messages.get(index))
        else {
            return;
        };
        let id = message.id;
        if self.shows_web(message) {
            self.web.simple.insert(id);
        } else {
            self.web.simple.remove(&id);
            self.web.failed.remove(&id);
            self.web.heights.remove(&id);
        }
        self.selection = None;
        self.relayout_message(index, cx);
    }

    /// The menu entry that switches `message`'s view, if it has two.
    pub(super) fn simple_view_label(&self, message: &ThreadMessage) -> Option<&'static str> {
        if !self.can_show_formatted(message) {
            return None;
        }
        Some(if self.shows_web(message) {
            "Show Simple View"
        } else {
            "Show Formatted View"
        })
    }

    /// Copies the selection in the page pressed last, when there is one.
    /// Returns false when the reader has no page to copy from.
    pub(super) fn copy_from_web(&self) -> bool {
        match (self.web.active, self.web.host.as_ref()) {
            (Some(id), Some(host)) => {
                host.copy(id as u64);
                true
            }
            _ => false,
        }
    }

    /// Selects all of `message`'s page, when it shows formatted.
    pub(super) fn select_all_in_web(&mut self, message: &ThreadMessage) -> bool {
        if !self.shows_web(message) {
            return false;
        }
        let Some(host) = self.web.host.as_ref() else {
            return false;
        };
        host.select_all(message.id as u64);
        self.web.active = Some(message.id);
        true
    }

    /// Hides the pages: the shell shows another pane in the reader's place.
    pub(crate) fn hide_web(&self) {
        self.web.hide_all();
    }

    fn message_index(&self, message_id: i64) -> Option<usize> {
        self.state
            .thread
            .as_ref()?
            .messages
            .iter()
            .position(|message| message.id == message_id)
    }

    /// Something a page reported.
    fn web_event(
        &mut self,
        message_id: i64,
        event: WebEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.message_index(message_id) else {
            return;
        };
        match event {
            WebEvent::Measured { height } => {
                let changed = self
                    .web
                    .heights
                    .insert(message_id, height)
                    .is_none_or(|old| (old - height).abs() >= 0.5);
                if changed {
                    self.relayout_message(index, cx);
                }
            }
            WebEvent::Pressed => {
                self.web.active = Some(message_id);
                self.selection = None;
                self.conversation.set_current(index);
                window.focus(&self.focus_handle, cx);
                cx.notify();
            }
            WebEvent::LinkActivated { url } => {
                self.web_link_clicked(message_id, &url, window, cx);
            }
            WebEvent::ContentProcessTerminated => {
                self.web_failed(message_id, index, "the web engine stopped".to_owned(), cx);
            }
            WebEvent::LoadFailed(reason) => self.web_failed(message_id, index, reason, cx),
        }
    }

    fn web_failed(
        &mut self,
        message_id: i64,
        index: usize,
        reason: String,
        cx: &mut Context<Self>,
    ) {
        self.web.failed.insert(message_id, reason);
        if self.web.active == Some(message_id) {
            self.web.active = None;
        }
        if let Some(host) = &self.web.host {
            host.retain(|key| key != message_id as u64);
        }
        self.relayout_message(index, cx);
    }

    /// A link in a page was clicked: the page reports its `zmail-link:<n>`
    /// id, and the open-link dialog shows where it really goes, with the
    /// look-alike warning, exactly like a link in the text view.
    fn web_link_clicked(
        &mut self,
        message_id: i64,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, prepared)) = self.web.prepared.get(&message_id) else {
            return;
        };
        let Some(id) = zmail_render_web::document::parse_link_id(url) else {
            eprintln!("zmail: web renderer: ignored a click on {url:?}, not a link id");
            return;
        };
        // Only web and mail links open (no tel: or sms:), as in the text.
        let Some(target) = prepared
            .link(id)
            .and_then(|target| link_target(&target.url))
        else {
            return;
        };
        // The link's visible text, for the look-alike check.
        let text = prepared
            .link(id)
            .map(|link| link.text.trim())
            .filter(|text| !text.is_empty())
            .map_or_else(|| target.clone(), str::to_owned);
        self.link_prompt = Some(LinkPrompt::new(&text, &target));
        self.link_modal.open(window, cx);
        cx.notify();
    }
}
