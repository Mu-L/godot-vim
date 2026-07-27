//! Shell-side action plane.
//!
//! Everything the plugin does outside an attached `CodeEdit` — panel focus,
//! dock item navigation, FileSystem operations — resolves through here.
//!
//! Built in phases (see `docs/DESIGN-rebindable-nav.md`):
//! - [`keys`] — one key vocabulary: the ordered probe list that replaced
//!   three ad-hoc per-site keycode fallbacks.

pub(crate) mod keys;
