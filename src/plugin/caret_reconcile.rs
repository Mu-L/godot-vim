//! Position-based caret suppression replacing the heuristic counter.
//!
//! When the Vim engine moves the cursor, Godot fires a deferred
//! `caret_changed` signal. The plugin must distinguish these Vim-driven
//! signals from genuine external changes (mouse clicks). Instead of a
//! blind counter, [`CaretReconciler`] records the expected position and
//! compares it when the signal arrives.
//!
//! Key design properties:
//! - `expect_vim_move` **overwrites** (not accumulates) because Godot's
//!   signal coalescing guarantees at most one deferred `caret_changed`
//!   per cursor mutation batch.
//! - `check_and_consume` uses `take()` for self-healing: even if a
//!   signal is lost, the stale expectation is consumed, preventing drift.

/// Expected cursor position set by the Vim engine after processing a key.
struct CaretExpectation {
    line: i32,
    col: i32,
}

/// Tracks whether a `caret_changed` signal was caused by the Vim engine
/// or by an external source (mouse click, Find-and-Replace, etc.).
pub(super) struct CaretReconciler {
    pending: Option<CaretExpectation>,
}

/// Result of [`CaretReconciler::check_and_consume`].
pub(super) enum CaretOrigin {
    /// Position matches expectation -- this was a Vim-driven move. Suppress.
    VimDriven,
    /// Position differs or no expectation -- this was external (mouse click). Process.
    External,
}

impl CaretReconciler {
    pub(super) fn new() -> Self {
        Self { pending: None }
    }

    /// Record where Vim just moved the cursor. Called after `process_cycle`.
    ///
    /// Overwrites any previous expectation -- Godot coalesces deferred signals,
    /// so only the final position matters.
    pub(super) fn expect_vim_move(&mut self, line: i32, col: i32) {
        self.pending = Some(CaretExpectation { line, col });
    }

    /// Check whether the `caret_changed` position matches the expectation.
    ///
    /// Consumes the expectation regardless (self-healing via `take()`).
    /// If the position matches, the signal was Vim-driven and should be
    /// suppressed. Otherwise it was external and should be processed.
    pub(super) fn check_and_consume(&mut self, line: i32, col: i32) -> CaretOrigin {
        match self.pending.take() {
            Some(exp) if exp.line == line && exp.col == col => CaretOrigin::VimDriven,
            _ => CaretOrigin::External,
        }
    }

    /// Clear all expectations (detach, panic recovery).
    pub(super) fn reset(&mut self) {
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_expectation_returns_external() {
        let mut r = CaretReconciler::new();
        assert!(matches!(
            r.check_and_consume(5, 10),
            CaretOrigin::External
        ));
    }

    #[test]
    fn matching_position_returns_vim_driven() {
        let mut r = CaretReconciler::new();
        r.expect_vim_move(5, 10);
        assert!(matches!(
            r.check_and_consume(5, 10),
            CaretOrigin::VimDriven
        ));
    }

    #[test]
    fn mismatching_position_returns_external() {
        let mut r = CaretReconciler::new();
        r.expect_vim_move(5, 10);
        // Different line
        assert!(matches!(
            r.check_and_consume(6, 10),
            CaretOrigin::External
        ));
    }

    #[test]
    fn mismatching_col_returns_external() {
        let mut r = CaretReconciler::new();
        r.expect_vim_move(5, 10);
        // Same line, different col
        assert!(matches!(
            r.check_and_consume(5, 11),
            CaretOrigin::External
        ));
    }

    #[test]
    fn overwrite_semantics() {
        let mut r = CaretReconciler::new();
        r.expect_vim_move(1, 0);
        r.expect_vim_move(5, 10); // overwrites
        // Old position no longer matches
        assert!(matches!(
            r.check_and_consume(1, 0),
            CaretOrigin::External
        ));
    }

    #[test]
    fn overwrite_new_position_matches() {
        let mut r = CaretReconciler::new();
        r.expect_vim_move(1, 0);
        r.expect_vim_move(5, 10); // overwrites
        // Must re-set since previous check consumed
        let mut r2 = CaretReconciler::new();
        r2.expect_vim_move(5, 10);
        assert!(matches!(
            r2.check_and_consume(5, 10),
            CaretOrigin::VimDriven
        ));
    }

    #[test]
    fn take_self_healing_no_drift() {
        let mut r = CaretReconciler::new();
        r.expect_vim_move(5, 10);
        // First check consumes regardless of match
        let _ = r.check_and_consume(5, 10);
        // Second check has no expectation -- external
        assert!(matches!(
            r.check_and_consume(5, 10),
            CaretOrigin::External
        ));
    }

    #[test]
    fn take_self_healing_after_mismatch() {
        let mut r = CaretReconciler::new();
        r.expect_vim_move(5, 10);
        // Mismatch consumes the expectation
        let _ = r.check_and_consume(0, 0);
        // Subsequent check has no expectation
        assert!(matches!(
            r.check_and_consume(5, 10),
            CaretOrigin::External
        ));
    }

    #[test]
    fn reset_clears_expectation() {
        let mut r = CaretReconciler::new();
        r.expect_vim_move(5, 10);
        r.reset();
        assert!(matches!(
            r.check_and_consume(5, 10),
            CaretOrigin::External
        ));
    }
}
