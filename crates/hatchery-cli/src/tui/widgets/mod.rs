//! The TUI's widgets: one module per surface of the frame.
//!
//! Each widget is a borrowed view over the [`crate::tui::Model`] implementing ratatui's
//! `Widget`, so drawing stays a pure function of the model (golden-frame tested) while the
//! surfaces stay separable: the transcript, the composer, the status line, the turn indicator
//! and the toasts each own their own layout and theme roles.

pub mod composer;
pub mod indicator;
pub mod scrollbar;
pub mod status;
pub mod toasts;
pub mod transcript;
