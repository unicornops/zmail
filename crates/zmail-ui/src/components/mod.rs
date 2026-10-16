//! Reusable UI components shared by the Zmail views.
//!
//! Each component lives in its own module and is re-exported from here.

pub(crate) mod actions;
pub(crate) mod button;
pub(crate) mod kbd;
#[cfg(test)]
mod test_support;
pub(crate) mod theme;
pub(crate) mod tooltip;

pub(crate) use actions::init;
pub(crate) use button::{Button, ButtonSize, ButtonVariant, IconButton};
pub(crate) use kbd::Kbd;
pub(crate) use theme::ComponentTheme;
