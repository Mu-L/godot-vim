//! The outcome of a shell-side key handler.
//!
//! Three states, and the third is the important one. `Declined` is not a
//! failure path — it is how the plugin *composes* with Godot. `_input` is
//! dispatched strictly before `gui_input` and offers no replay channel, so
//! consuming a key destroys it permanently; declining is the only way a
//! control's own behaviour survives.
//!
//! That makes declination the operator the whole binding model is built on:
//! the resolver becomes a fold over ordered candidates, terminated by the
//! first non-declination. "FileSystem dock gets first refusal", "`h`/`l` are
//! inert on a list with no hierarchy" and "`j` at the end of a list falls
//! through to Godot" stop being three special cases and become one mechanism.

/// Tri-state outcome of a shell-side key handler.
///
/// `FocusChanged` is currently treated identically to `Handled` by every
/// caller — `is_consumed()` is the only method anyone calls. It is kept
/// distinct because moving focus is the case that will need extra
/// bookkeeping once dock keys become rebindable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(crate) enum Outcome {
    /// Event consumed — call `set_input_as_handled()`.
    Handled,
    /// Event consumed and focus moved to a different control.
    FocusChanged,
    /// Not consumed — Godot's native handling proceeds.
    ///
    /// This is a first-class outcome, not a failure. Godot dispatches
    /// `_input` strictly before `gui_input` and offers no replay channel, so
    /// consuming here destroys the event permanently; declining is the only
    /// way a control's own behaviour survives. Two unambiguous examples:
    /// `Esc` when no script editor can be found (`handle_escape_from_dock`),
    /// and `Enter` on a `RichTextLabel` (`handle_enter`).
    ///
    /// Note this variant currently conflates two different things —
    /// "recognized the key but declined to act" (the `DockKind` gates) and
    /// "never matched at all" (the modifier guards and the `_ =>` arms).
    /// Separating them belongs to the resolver, not to this enum. Until
    /// then, do not flatten this type to `bool`: a dispatcher that consumes
    /// every key it recognizes is a wall, not a keymap.
    Declined,
}

/// Display rather than Debug, per `LOGGING.md`'s "Display over Debug" rule:
/// an outcome appears in the per-keystroke summary line, where `Handled`
/// reads and `Outcome::Handled` is noise.
impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Handled => "handled",
            Self::FocusChanged => "focus-changed",
            Self::Declined => "declined",
        })
    }
}

impl Outcome {
    /// Positive exhaustive match on purpose: a future variant becomes a
    /// compile error here instead of silently defaulting to "consumed",
    /// which would swallow the key.
    pub(crate) const fn is_consumed(self) -> bool {
        match self {
            Self::Handled | Self::FocusChanged => true,
            Self::Declined => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_declined_is_unconsumed() {
        assert!(Outcome::Handled.is_consumed());
        assert!(Outcome::FocusChanged.is_consumed());
        assert!(!Outcome::Declined.is_consumed());
    }

    // Compile-time proof that `is_consumed` stays `const`-evaluable. Nothing
    // in production evaluates it in a const context yet, so these three lines
    // are the only thing holding the signature; dropping the `const` would
    // otherwise be a silent, and later expensive, change.
    const _: () = assert!(Outcome::Handled.is_consumed());
    const _: () = assert!(Outcome::FocusChanged.is_consumed());
    const _: () = assert!(!Outcome::Declined.is_consumed());
}
