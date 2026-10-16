//! Reusable UI components shared by the Zmail views.
//!
//! Each component lives in its own module and is re-exported from here.

pub(crate) mod actions;
pub(crate) mod avatar;
pub(crate) mod badge;
pub(crate) mod button;
pub(crate) mod kbd;
pub(crate) mod menu;
pub(crate) mod modal;
pub(crate) mod popover;
pub(crate) mod select;
pub(crate) mod skeleton;
#[cfg(test)]
mod test_support;
pub(crate) mod theme;
pub(crate) mod tooltip;

pub(crate) use actions::init;
pub(crate) use avatar::Avatar;
pub(crate) use badge::{Badge, BadgeTone};
pub(crate) use button::{Button, ButtonSize, ButtonVariant, IconButton};
pub(crate) use kbd::Kbd;
pub(crate) use menu::{Menu, MenuEntry};
pub(crate) use modal::{Modal, ModalState};
pub(crate) use popover::{overlay_at, popover_below};
pub(crate) use select::{Select, SelectEvent};
pub(crate) use skeleton::SkeletonRow;
pub(crate) use theme::ComponentTheme;
