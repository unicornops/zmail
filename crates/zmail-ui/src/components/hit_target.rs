//! Pointer target size (WCAG 2.2 success criterion 2.5.8, Target Size
//! (Minimum)): everything that takes a click is at least
//! [`MIN_TARGET_SIZE`] square at the default text size, and larger text
//! only makes targets bigger.
//!
//! Interactive elements mark themselves with [`HitTargetExt::hit_target`],
//! which sets their debug selector. In tests it also records the selector,
//! so `view_tests::hit_targets` can find every target a frame drew and
//! check its bounds without a hand-kept list.
//!
//! Two kinds of target are smaller, as documented exceptions with
//! keyboard and wheel alternatives rather than a pass:
//!
//! - the split handles between panes are 6px wide (arrow keys, Home and
//!   End resize them, and a double click restores the default width);
//! - the overlay scrollbar's strip is 12px to 15px wide, though its thumb
//!   is at least 24px long (the wheel, the trackpad and Page Up/Down
//!   scroll).
//!
//! Widening either would overlap the other (each pane's scrollbar sits
//! beside a split handle) and the leading edge of the rows next to them.

use gpui::InteractiveElement;

/// The smallest width and height of a pointer target, in pixels at the
/// default text size.
pub(crate) const MIN_TARGET_SIZE: f32 = 24.0;

/// Marks an element as a pointer target.
pub(crate) trait HitTargetExt: InteractiveElement + Sized {
    /// Mark this element as a pointer target named `selector`, which is
    /// also its debug selector for tests.
    fn hit_target(self, selector: impl FnOnce() -> String) -> Self {
        #[cfg(test)]
        {
            let selector = selector();
            recorded::record(&selector);
            self.debug_selector(move || selector)
        }
        #[cfg(not(test))]
        self.debug_selector(selector)
    }
}

impl<E: InteractiveElement> HitTargetExt for E {}

/// The targets drawn on this thread since the last [`recorded::take`].
#[cfg(test)]
pub(crate) mod recorded {
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    thread_local! {
        static RECORDED: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    }

    pub(super) fn record(selector: &str) {
        RECORDED.with(|recorded| {
            let mut recorded = recorded.borrow_mut();
            if !recorded.contains(selector) {
                recorded.insert(selector.to_owned());
            }
        });
    }

    /// The selectors of the targets rendered since the last call, and
    /// forget them.
    pub(crate) fn take() -> Vec<String> {
        RECORDED
            .with(|recorded| std::mem::take(&mut *recorded.borrow_mut()))
            .into_iter()
            .collect()
    }
}
