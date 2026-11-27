//! `mailto:` links handed to the running app (#457): by the system's
//! open-URL event (GPUI's `Application::on_open_urls`; macOS sends a link
//! this way, at launch and while Zmail runs), and by a second launch,
//! which hands its link to this process instead of starting another
//! window (`zmail-app`'s `single_instance`). Each link opens a compose
//! window on a new draft filled from it.
//!
//! Links come through a channel ([`url_channel`]), since neither source
//! runs with the app at hand. Every link is parsed by
//! [`zmail_security::mailto::parse_mailto`], the parser the command line
//! uses; anything else is dropped. A link that arrives before the store is
//! unlocked waits ([`OpenUrlInbox::held`]) and opens once the main window
//! does.

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures_util::StreamExt as _;
use gpui::{App, Global, WindowHandle};
use zmail_security::mailto::{MailtoDraft, parse_mailto};

use crate::WorkspaceView;

/// Hands links to the app; cheap to clone and usable from any thread.
#[derive(Clone)]
pub struct UrlSender(UnboundedSender<String>);

impl UrlSender {
    /// Queues `url` for the app. `false` once the app has quit.
    pub fn send(&self, url: String) -> bool {
        self.0.unbounded_send(url).is_ok()
    }
}

/// The app's end of [`url_channel`], given to [`crate::run_unlocking`].
pub struct UrlReceiver {
    sender: UrlSender,
    receiver: UnboundedReceiver<String>,
}

impl UrlReceiver {
    /// A sender into this channel.
    pub fn sender(&self) -> UrlSender {
        self.sender.clone()
    }
}

/// A channel of links for the app to open: the sender for whatever
/// receives them (a second launch), the receiver for
/// [`crate::run_unlocking`].
pub fn url_channel() -> (UrlSender, UrlReceiver) {
    let (sender, receiver) = unbounded();
    let sender = UrlSender(sender);
    (sender.clone(), UrlReceiver { sender, receiver })
}

/// The links waiting for the main window, and that window once it opens.
#[derive(Default)]
pub(crate) struct OpenUrlInbox {
    window: Option<WindowHandle<WorkspaceView>>,
    held: Vec<MailtoDraft>,
}

impl Global for OpenUrlInbox {}

impl OpenUrlInbox {
    /// The drafts still waiting, for tests.
    #[cfg(test)]
    pub(crate) fn held(cx: &App) -> Vec<MailtoDraft> {
        cx.try_global::<Self>()
            .map(|inbox| inbox.held.clone())
            .unwrap_or_default()
    }
}

/// Registers the system's open-URL event on `app`, before it runs (macOS
/// can send a link while the app is still launching).
pub(crate) fn register(app: &gpui::Application, urls: &UrlReceiver) {
    let sender = urls.sender();
    app.on_open_urls(move |urls| {
        for url in urls {
            sender.send(url);
        }
    });
}

/// Opens each link `urls` brings from now on.
pub(crate) fn listen(urls: UrlReceiver, cx: &mut App) {
    let UrlReceiver {
        mut receiver,
        sender,
    } = urls;
    cx.spawn(async move |cx| {
        // Keeps the channel open while the app runs, whoever else holds a
        // sender.
        let _sender = sender;
        while let Some(url) = receiver.next().await {
            cx.update(|cx| receive(&url, cx));
        }
    })
    .detach();
}

/// A link for the app: a new message from it, now or once the main window
/// opens. Anything but a `mailto:` link the parser accepts is dropped.
pub(crate) fn receive(url: &str, cx: &mut App) {
    let Some(draft) = parse_mailto(url) else {
        eprintln!("zmail: ignoring a link that is not a valid mailto: link");
        return;
    };
    cx.default_global::<OpenUrlInbox>().held.push(draft);
    // Outside whatever window is being updated now (the main window
    // can't be updated from inside its own update).
    cx.defer(deliver);
}

/// The main window is open: the links that waited open in it, and later
/// ones too.
pub(crate) fn attach(window: WindowHandle<WorkspaceView>, cx: &mut App) {
    cx.default_global::<OpenUrlInbox>().window = Some(window);
    cx.defer(deliver);
}

/// Opens the waiting links in the main window, if it is open. Without
/// one (still unlocking, or closed) they keep waiting.
fn deliver(cx: &mut App) {
    let inbox = cx.default_global::<OpenUrlInbox>();
    let Some(window) = inbox.window else {
        return;
    };
    if inbox.held.is_empty() {
        return;
    }
    let held = std::mem::take(&mut inbox.held);
    let pending = held.clone();
    let opened = window.update(cx, |view, window, cx| {
        for draft in held {
            view.compose_mailto(draft, window, cx);
        }
    });
    match opened {
        // From another app: bring Zmail forward with the new message.
        Ok(()) => cx.activate(true),
        Err(_) => {
            let inbox = cx.default_global::<OpenUrlInbox>();
            inbox.window = None;
            inbox.held = pending;
        }
    }
}
