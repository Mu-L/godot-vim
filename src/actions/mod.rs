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
//! - [`bind`] — the binding plane: one `MappingTrie` per surface over a side
//!   arena of rules, and the registration-time validation that keeps a typo
//!   from becoming a silent dead key.
//! - [`resolve`] — the dispatch model itself, as a pure function: the
//!   leaf→root candidate walk, the arbitration seam, and the consumption
//!   fold. Takes no `Gd<T>` and calls no Godot API.
//! - [`sequence`] — pending prefixes: the only state the shell plane holds
//!   between keystrokes, and the reservation model that makes it safe on a
//!   host with no replay channel.
//! - [`introspect`] — `:panelmap`, which ships in the same commit as the
//!   cutover because a config surface with no way to see what is bound is how
//!   silent dead keys happen.

pub(crate) mod action;
pub(crate) mod bind;
pub(crate) mod caps;
pub(crate) mod introspect;
pub(crate) mod keys;
pub(crate) mod outcome;
pub(crate) mod providers;
pub(crate) mod resolve;
pub(crate) mod sequence;
pub(crate) mod specs;
pub(crate) mod surface;
