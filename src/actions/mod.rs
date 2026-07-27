//! Shell-side action plane.
//!
//! Everything the plugin does outside an attached `CodeEdit` — panel focus,
//! dock item navigation, FileSystem operations — resolves through here.
//!
//! The plane exists because the old dispatcher fused five independent
//! decisions into single match arms: which key, which widget kinds are
//! eligible, what behaviour runs, whether it succeeded, and whether to consume
//! the event. Only the first is the user's business. Separating them is what
//! makes any of it rebindable.
//!
//! Built in phases (see `docs/DESIGN-rebindable-nav.md`):
//! - [`keys`] — one key vocabulary: the ordered probe list that replaced
//!   three ad-hoc per-site keycode fallbacks.
//! - [`outcome`] — what a handler answers, including the right to decline.
//! - [`caps`] — what a focused control can do, replacing widget-identity
//!   gates like `matches!(dock_kind, DockKind::Tree)`.
//! - [`action`] — named verbs: stable dotted ids, the registry that interns
//!   them, and the context an action runs in.
//! - [`specs`] — the shipped keyset as named verbs, in one const array.
//! - [`surface`] — where a keystroke is, as literal data: the sampled
//!   `FocusChain` and the declared surface forest above it.
//! - [`providers`] — one file per subsystem, declaring its surfaces in the
//!   probe order that *is* the classification.

pub(crate) mod action;
pub(crate) mod caps;
pub(crate) mod keys;
pub(crate) mod outcome;
pub(crate) mod providers;
pub(crate) mod specs;
pub(crate) mod surface;
