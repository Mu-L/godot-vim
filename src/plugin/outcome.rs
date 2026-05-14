//! `#[must_use]` wrapper enforcing UI updates after engine state changes.
//!
//! Every code path that modifies engine state MUST produce an [`EngineOutcome`].
//! The compiler warns (or errors, via `#[deny(unused_must_use)]`) if the
//! outcome is dropped without calling [`apply_ui_update`] or [`discard`].
//!
//! This consolidates the previously scattered `ui.update()` + caret
//! reconciliation logic into a single exit point, preventing the class of
//! bugs where a new code path forgets to refresh the UI.
//!
//! [`apply_ui_update`]: EngineOutcome::apply_ui_update
//! [`discard`]: EngineOutcome::discard

use godot::classes::CodeEdit;
use godot::prelude::*;

use crate::controller::PipelineOutcome;
use crate::types::UiSnapshot;
use crate::ui::UiCoordinator;

use super::caret_reconcile::CaretReconciler;

/// Wraps a [`PipelineOutcome`] and an optional [`UiSnapshot`], enforcing
/// that the caller either applies the UI update or explicitly discards it.
///
/// Constructed via [`with_snapshot`] (normal path) or [`no_update`]
/// (passthrough / no-op paths). Consumed via [`apply_ui_update`] (pushes
/// snapshot to UI + sets caret expectation) or [`discard`] (explicit no-op).
///
/// [`with_snapshot`]: EngineOutcome::with_snapshot
/// [`no_update`]: EngineOutcome::no_update
/// [`apply_ui_update`]: EngineOutcome::apply_ui_update
/// [`discard`]: EngineOutcome::discard
#[must_use = "engine state was modified -- call .apply_ui_update() or .discard()"]
pub(super) struct EngineOutcome {
    snapshot: Option<UiSnapshot>,
    pipeline: PipelineOutcome,
}

/// Post-update handle returned by [`EngineOutcome::apply_ui_update`].
///
/// Provides access to the [`PipelineOutcome`] for downstream decisions
/// (e.g. `should_mark_handled()`, `log_label()`).
pub(super) struct AppliedOutcome {
    pub(super) pipeline: PipelineOutcome,
}

impl EngineOutcome {
    /// Normal path: engine produced a snapshot that needs pushing to the UI.
    pub(super) fn with_snapshot(snapshot: UiSnapshot, pipeline: PipelineOutcome) -> Self {
        Self {
            snapshot: Some(snapshot),
            pipeline,
        }
    }

    /// No-op path: engine state was not modified in a way that requires a
    /// UI refresh (e.g. passthrough, early return before process_cycle).
    #[allow(dead_code)] // API completeness: available for future call sites
    pub(super) fn no_update() -> Self {
        Self {
            snapshot: None,
            pipeline: PipelineOutcome::Passthrough,
        }
    }

    /// THE single UI update exit point. Pushes the snapshot (if present) to
    /// the UI coordinator and automatically sets a caret expectation when
    /// the pipeline indicates Vim may have moved the cursor.
    ///
    /// The caret reconciliation is intentionally unconditional when
    /// `may_have_moved_cursor()` is true: even if the cursor didn't actually
    /// move, the reconciler's position-based approach is self-healing -- the
    /// next `caret_changed` signal will see the same position as the
    /// expectation and classify it as `VimDriven`, which correctly suppresses
    /// the no-op signal.
    pub(super) fn apply_ui_update(
        self,
        ui: &mut UiCoordinator,
        editor: &mut Gd<CodeEdit>,
        reconciler: &mut CaretReconciler,
    ) -> AppliedOutcome {
        if let Some(ref snap) = self.snapshot {
            ui.update(snap, editor);
        }
        // Automatic caret expectation when engine may have moved cursor.
        if self.pipeline.may_have_moved_cursor() {
            let line = editor.get_caret_line();
            let col = editor.get_caret_column();
            reconciler.expect_vim_move(line, col);
        }
        AppliedOutcome {
            pipeline: self.pipeline,
        }
    }

    /// Explicitly acknowledge that no UI update is needed. Consumes `self`
    /// so the `#[must_use]` obligation is satisfied without triggering a
    /// compiler warning.
    #[allow(dead_code)] // API completeness: available for future call sites
    pub(super) fn discard(self) {
        // Intentional no-op: consuming `self` is the entire point.
    }
}
