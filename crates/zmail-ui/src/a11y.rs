//! Screen-reader support on top of GPUI's AccessKit integration.
//!
//! GPUI reports an element to assistive technology only when it has both
//! an `id` and a `role`; plain string children are not in the
//! accessibility tree. So every control and every row names itself with
//! [`AccessibleExt::accessible`], which sets the role and the label
//! together. Decorative pieces (icons, avatars, badges, key caps) have no
//! node of their own: the row or button around them says what they mean.
//!
//! GPUI has builders for most ARIA-style properties (`aria_selected`,
//! `aria_expanded`, `aria_level`, …) but not for disabled, modal,
//! multi-selectable or live regions. [`AccessibleExt::a11y_flags`] sets
//! those straight on the element's AccessKit node.
//!
//! A live region is announced when it appears or its text changes:
//! AccessKit raises an announcement on macOS, a UIA LiveRegionChanged
//! event on Windows and an AT-SPI announcement on Linux. [`live_region`]
//! draws an invisible one whose id should change with each announcement,
//! so every new text is a new node (AT-SPI announces only new nodes).
//!
//! None of this is observable in tests: GPUI's test platform never turns
//! AccessKit on, so the tree is never built. Instead, in test builds,
//! [`AccessibleExt::accessible`] records each role and label it sets, and
//! [`audit`] collects them while a view renders.

use gpui::{
    Div, ElementId, Role, SharedString, Stateful, StatefulInteractiveElement, Text, accesskit, div,
    prelude::*, px,
};

/// Accessibility builders for any element with an id.
pub(crate) trait AccessibleExt: StatefulInteractiveElement + Sized {
    /// Expose the element to assistive technology as `role`, named
    /// `label`. The label must say what the element is or does on its own,
    /// without the icons or layout around it; the view tests fail on an
    /// empty one.
    fn accessible(self, role: Role, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        #[cfg(test)]
        audit::record(role, &label);
        self.role(role).aria_label(label)
    }

    /// Set the AccessKit properties GPUI has no builder for. GPUI keeps
    /// one such hook per element, so pass every flag in one call.
    fn a11y_flags(self, flags: A11yFlags) -> Self {
        if flags == A11yFlags::default() {
            return self;
        }
        self.a11y_synthetic_children(move |builder| flags.apply(builder.parent_node()))
    }
}

impl<E: StatefulInteractiveElement> AccessibleExt for E {}

/// Text a screen reader can reach. Plain string children are drawn but
/// not in the accessibility tree; use this for text that says something
/// no control's label already does, such as an empty state or a subject.
/// `id` must be unique among the texts under the same parent with an id.
pub(crate) fn readable(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Text {
    Text::new(id.into(), text.into())
}

/// An invisible element a screen reader announces when it appears: its
/// text is both the name (what Windows and Linux read) and the value
/// (what macOS reads). Give each announcement its own `id`, such as one
/// from [`announcement_key`], and place it inside a positioned parent: it
/// sits in that parent's top-left corner, one pixel square.
pub(crate) fn live_region(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    politeness: Politeness,
) -> Stateful<Div> {
    let text = text.into();
    let role = match politeness {
        Politeness::Polite => Role::Status,
        Politeness::Assertive => Role::Alert,
    };
    div()
        .id(id)
        .absolute()
        .top_0()
        .left_0()
        .size(px(1.0))
        .overflow_hidden()
        .accessible(role, text.clone())
        .aria_value(text)
        .a11y_flags(A11yFlags {
            live: Some(politeness),
            ..A11yFlags::default()
        })
}

/// A stable number for an announcement's text (32-bit FNV-1a), for a live
/// region id that changes exactly when the text does.
pub(crate) fn announcement_key(text: &str) -> u64 {
    u64::from(text.bytes().fold(0x811C_9DC5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    }))
}

/// What a screen reader hears as the highlight moves through a list whose
/// focus stays in a text field (the palette, Move to…, recipient
/// suggestions): GPUI only reports an active descendant under the focused
/// element, so the highlighted option is announced instead.
pub(crate) fn highlight_announcement(label: &str, position: usize, count: usize) -> String {
    if count == 0 {
        "No matches".to_owned()
    } else {
        format!("{label}, {} of {count}", position + 1)
    }
}

/// Something to announce, numbered so that saying the same thing twice
/// is announced twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Announcement {
    pub(crate) serial: u64,
    pub(crate) text: SharedString,
    pub(crate) politeness: Politeness,
}

/// The latest announcement a view has to make.
#[derive(Debug, Default)]
pub(crate) struct Announcer {
    next_serial: u64,
    current: Option<Announcement>,
}

impl Announcer {
    /// Replace the announcement with `text`.
    pub(crate) fn announce(&mut self, text: impl Into<SharedString>, politeness: Politeness) {
        self.next_serial += 1;
        self.current = Some(Announcement {
            serial: self.next_serial,
            text: text.into(),
            politeness,
        });
    }

    /// The latest announcement, if any.
    #[cfg(test)]
    pub(crate) fn current(&self) -> Option<&Announcement> {
        self.current.as_ref()
    }

    /// The live region for the latest announcement, ids prefixed with
    /// `prefix`.
    pub(crate) fn render(&self, prefix: &'static str) -> Option<Stateful<Div>> {
        self.current.as_ref().map(|announcement| {
            live_region(
                (prefix, announcement.serial),
                announcement.text.clone(),
                announcement.politeness,
            )
        })
    }
}

/// How urgently assistive technology reads out a live region's changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Politeness {
    /// After whatever is being read now: confirmations and status.
    Polite,
    /// Straight away, interrupting: errors.
    Assertive,
}

/// Node properties GPUI has no builder for.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct A11yFlags {
    /// The control is shown but cannot be used.
    pub(crate) disabled: bool,
    /// A dialog that keeps focus until it closes.
    pub(crate) modal: bool,
    /// A list in which more than one item can be selected.
    pub(crate) multiselectable: bool,
    /// Changes to the node's text are announced.
    pub(crate) live: Option<Politeness>,
}

impl A11yFlags {
    /// Only [`Self::disabled`] when `disabled`.
    pub(crate) fn disabled(disabled: bool) -> Self {
        Self {
            disabled,
            ..Self::default()
        }
    }

    /// Write the flags onto an AccessKit node.
    pub(crate) fn apply(self, node: &mut accesskit::Node) {
        if self.disabled {
            node.set_disabled();
        }
        if self.modal {
            node.set_modal();
        }
        if self.multiselectable {
            node.set_multiselectable();
        }
        if let Some(politeness) = self.live {
            node.set_live(match politeness {
                Politeness::Polite => accesskit::Live::Polite,
                Politeness::Assertive => accesskit::Live::Assertive,
            });
        }
    }
}

/// Collects the roles and labels [`AccessibleExt::accessible`] sets while
/// views render, so tests can check that every control is named.
#[cfg(test)]
pub(crate) mod audit {
    use std::cell::RefCell;

    use gpui::Role;

    thread_local! {
        static ENTRIES: RefCell<Option<Vec<(Role, String)>>> = const { RefCell::new(None) };
    }

    /// Start collecting (again) on this thread.
    pub(crate) fn start() {
        ENTRIES.with(|entries| *entries.borrow_mut() = Some(Vec::new()));
    }

    /// Stop collecting and return what was recorded since [`start`].
    pub(crate) fn finish() -> Vec<(Role, String)> {
        ENTRIES.with(|entries| entries.borrow_mut().take().unwrap_or_default())
    }

    pub(super) fn record(role: Role, label: &str) {
        ENTRIES.with(|entries| {
            if let Some(entries) = entries.borrow_mut().as_mut() {
                entries.push((role, label.to_owned()));
            }
        });
    }

    /// The labels recorded with `role`.
    pub(crate) fn labels(entries: &[(Role, String)], role: Role) -> Vec<&str> {
        entries
            .iter()
            .filter(|(entry_role, _)| *entry_role == role)
            .map(|(_, label)| label.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_highlight_reads_its_place_in_the_list() {
        assert_eq!(highlight_announcement("Archive", 0, 12), "Archive, 1 of 12");
        assert_eq!(highlight_announcement("", 0, 0), "No matches");
    }

    #[test]
    fn each_announcement_gets_a_new_serial() {
        let mut announcer = Announcer::default();
        assert!(announcer.current().is_none());
        announcer.announce("3 results", Politeness::Polite);
        let first = announcer.current().cloned().expect("an announcement");
        announcer.announce("3 results", Politeness::Polite);
        let second = announcer.current().cloned().expect("an announcement");
        assert_eq!(first.text, second.text);
        assert_ne!(first.serial, second.serial, "the same words are said again");
        assert_eq!(announcement_key("a"), announcement_key("a"));
        assert_ne!(announcement_key("a"), announcement_key("b"));
    }

    #[test]
    fn flags_set_the_matching_node_properties() {
        let mut node = accesskit::Node::new(Role::ListBox);
        A11yFlags {
            disabled: true,
            modal: false,
            multiselectable: true,
            live: Some(Politeness::Assertive),
        }
        .apply(&mut node);
        assert!(node.is_disabled());
        assert!(!node.is_modal());
        assert!(node.is_multiselectable());
        assert_eq!(node.live(), Some(accesskit::Live::Assertive));

        let mut plain = accesskit::Node::new(Role::Button);
        A11yFlags::default().apply(&mut plain);
        assert!(!plain.is_disabled());
        assert!(!plain.is_multiselectable());
        assert_eq!(plain.live(), None);
    }
}
