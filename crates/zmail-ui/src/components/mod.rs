//! Reusable UI components shared by the Zmail views.
//!
//! Each component lives in its own module and is re-exported from here.

pub(crate) mod actions;
pub(crate) mod avatar;
pub(crate) mod badge;
pub(crate) mod button;
pub(crate) mod gallery;
pub(crate) mod kbd;
pub(crate) mod menu;
pub(crate) mod modal;
pub(crate) mod popover;
pub(crate) mod scrollbar;
pub(crate) mod select;
pub(crate) mod skeleton;
pub(crate) mod split_handle;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod text_input;
pub(crate) mod theme;
pub(crate) mod title_bar;
pub(crate) mod toast;
pub(crate) mod tooltip;
pub(crate) mod window_frame;

pub(crate) use actions::init;
pub(crate) use avatar::Avatar;
pub(crate) use badge::{Badge, BadgeTone};
pub(crate) use button::{Button, ButtonSize, ButtonVariant, IconButton};
pub(crate) use kbd::Kbd;
pub(crate) use menu::{Menu, MenuEntry};
pub(crate) use modal::{Modal, ModalState};
pub(crate) use popover::{overlay_at, popover_below, popover_below_end};
pub(crate) use scrollbar::Scrollbar;
pub(crate) use select::{Select, SelectEvent};
pub(crate) use skeleton::SkeletonRow;
pub(crate) use split_handle::{PaneSide, SplitHandle};
pub(crate) use text_input::{TextInput, TextInputEvent};
pub(crate) use theme::ComponentTheme;
pub(crate) use title_bar::TitleBar;
pub(crate) use toast::{Toast, ToastHost, ToastId, ToastKind};
pub(crate) use window_frame::window_frame;
