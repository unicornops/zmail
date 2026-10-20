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
//! `aria_expanded`, `aria_level`, …) but not for disabled, modal or
//! multi-selectable. [`AccessibleExt::a11y_flags`] sets
//! those straight on the element's AccessKit node.
//!
//! None of this is observable in tests: GPUI's test platform never turns
//! AccessKit on, so the tree is never built. Instead, in test builds,
//! [`AccessibleExt::accessible`] records each role and label it sets, and
//! [`audit`] collects them while a view renders.

use gpui::{ElementId, Role, SharedString, StatefulInteractiveElement, Text, accesskit};

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

/// Node properties GPUI has no builder for.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct A11yFlags {
    /// The control is shown but cannot be used.
    pub(crate) disabled: bool,
    /// A dialog that keeps focus until it closes.
    pub(crate) modal: bool,
    /// A list in which more than one item can be selected.
    pub(crate) multiselectable: bool,
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
    fn flags_set_the_matching_node_properties() {
        let mut node = accesskit::Node::new(Role::ListBox);
        A11yFlags {
            disabled: true,
            modal: false,
            multiselectable: true,
        }
        .apply(&mut node);
        assert!(node.is_disabled());
        assert!(!node.is_modal());
        assert!(node.is_multiselectable());

        let mut plain = accesskit::Node::new(Role::Button);
        A11yFlags::default().apply(&mut plain);
        assert!(!plain.is_disabled());
        assert!(!plain.is_multiselectable());
    }
}
