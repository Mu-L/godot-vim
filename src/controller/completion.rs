//! Completion-aware key routing for CodeEdit's autocomplete popup.
//!
//! Godot's CodeEdit autocomplete is driven by `_gui_input()`, which never
//! fires when Vim consumes the key via `set_input_as_handled()`. This module
//! intercepts completion-relevant keys *before* the engine so the popup can
//! trigger, filter, navigate, and confirm — all without engine changes.
//!
//! The interception has two phases:
//! - **Pre-engine** ([`try_handle_completion`]): routes whatever the user has
//!   bound on the `editor.completion` surface — by default Ctrl+@, Ctrl+N/P,
//!   Tab, Enter, Escape and the arrow keys.
//! - **Post-engine** ([`maybe_retrigger_completion`]): re-triggers the popup
//!   after printable/backspace keystrokes so filtering stays in sync.
//!
//! # Rebindable, and still on this transport (P9)
//!
//! The key table used to be a `match` on literals here. It is now eight
//! `panelmap` lines in `actions::providers::completion`, resolved by
//! `GodotVimCore::handle_gui_input_impl` against the same `BindingIndex` the
//! panel keys use, and handed down as an `&'static ActionSpec`.
//!
//! What did **not** move is the transport. These keys stay on `gui_input`, for
//! three reasons that were each load-bearing and none of which a rebindable
//! table changes: `_input` is registered per viewport and never fires for a
//! floated script editor; `_input` runs outside the IME guard above this call,
//! so a CJK preedit would lose `<CR>`; and `_input`'s consumption model cannot
//! express `Some(false)` — "handled by us, engine skipped, event deliberately
//! NOT consumed" — which is what Up/Down need. [`CompletionPort`] carries that
//! third state; see [`verdict`].

use godot::classes::CodeEdit;
use godot::prelude::*;
use vim_core::execution::{VimEngine, VimSession};
use vim_core::keymap::{Key, KeyEvent, Modifiers};

use crate::actions::action::{ActionCtx, ActionSpec, CompletionOps, Params};
use crate::actions::outcome::Outcome;
use crate::bridge;
use crate::bridge::codec::usize_to_i32;
use crate::bridge::godot_host::GodotHost;

/// Godot returns -1 when no completion popup is visible.
fn is_completion_active(editor: &Gd<CodeEdit>) -> bool {
    editor.get_code_completion_selected_index() >= 0
}

/// The one real [`CompletionOps`], holding the two things no test can build.
///
/// Everything a completion verb decides is decided against this trait; the
/// verbs themselves live in `actions::providers::completion` and are tested
/// against a plain-data fake. That split is the only reason the `Some(true)` /
/// `Some(false)` / `None` trichotomy has a headless characterization suite at
/// all — `Gd<CodeEdit>` and `VimSession<GodotHost>` cannot be constructed under
/// `cargo test` in a `cdylib`.
struct CompletionPort<'a> {
    session: &'a mut VimSession<GodotHost>,
    editor: &'a mut Gd<CodeEdit>,
    /// Set by [`CompletionOps::hand_to_editor`]. Read by [`verdict`].
    handed_off: bool,
}

impl CompletionOps for CompletionPort<'_> {
    fn popup_visible(&self) -> bool {
        is_completion_active(self.editor)
    }

    fn completion_enabled(&self) -> bool {
        self.editor.is_code_completion_enabled()
    }

    fn option_count(&self) -> i32 {
        usize_to_i32(self.editor.get_code_completion_options().len())
    }

    fn selected_index(&self) -> i32 {
        self.editor.get_code_completion_selected_index()
    }

    fn request(&mut self, force: bool) {
        self.editor.request_code_completion_ex().force(force).done();
    }

    fn select(&mut self, index: i32) {
        self.editor.set_code_completion_selected_index(index);
    }

    fn confirm(&mut self) {
        confirm_and_reconcile_completion(self.session, self.editor);
    }

    fn cancel(&mut self) {
        self.editor.cancel_code_completion();
    }

    fn hand_to_editor(&mut self) {
        self.handed_off = true;
    }
}

/// Fold an action's [`Outcome`] and the port's routing flag into the
/// transport's tri-state.
///
/// - `Some(true)` — consumed here; `set_input_as_handled()` fires.
/// - `Some(false)` — engine skipped, event deliberately **not** consumed, so
///   `CodeEdit::_gui_input` moves its own popup selection.
/// - `None` — not ours; the vim engine processes the key normally.
///
/// The third case falls straight out of [`Outcome::Declined`], which is why no
/// fourth `Outcome` variant was needed: a verb that cannot act (no popup up)
/// declines, and declining on this transport already means "the engine gets
/// it". `godotvim.completion.dismiss` uses exactly that to cancel the popup
/// *and* let `<Esc>` leave insert mode in one keypress.
const fn verdict(outcome: Outcome, handed_off: bool) -> Option<bool> {
    if handed_off {
        return Some(false);
    }
    if outcome.is_consumed() {
        Some(true)
    } else {
        None
    }
}

/// Pre-engine interception for completion and trigger keys.
///
/// `binding` is whatever the `editor.completion` surface resolved this key to,
/// or `None` when the user has nothing bound — in which case there is nothing
/// to intercept and the engine gets the key untouched.
///
/// Returns `Some(consumed)` if the key was handled here (skip engine).
/// Returns `None` if the engine should process the key normally.
pub(crate) fn try_handle_completion(
    session: &mut VimSession<GodotHost>,
    editor: &mut Gd<CodeEdit>,
    binding: Option<&'static ActionSpec>,
) -> Option<bool> {
    let spec = binding?;

    // The mode gate stays on the transport rather than moving into the verbs,
    // and stays exactly where it was: `in_insert` guarded every arm of the old
    // table. It is not a capability and not a surface predicate — `Caps` is
    // sampled from a focus chain this surface never classifies — so the one
    // place that can honestly ask is the one place holding the session.
    let mode = session.engine().mode();
    if !mode.is_insert() && !mode.is_replace() {
        return None;
    }

    let mut port = CompletionPort {
        session,
        editor,
        handed_off: false,
    };
    let outcome = {
        // The action borrows the port; the verdict reads it back. Scoped so no
        // borrow of `port` outlives the call, mirroring `run_candidate`'s
        // copy-out-the-spec discipline in `plugin::input`.
        let mut cx = ActionCtx::new(None, Params::new()).with_completion(&mut port);
        (spec.run)(&mut cx)
    };
    let handed_off = port.handed_off;
    log::trace!(
        "completion: {} -> {outcome:?} handed_off={handed_off}",
        spec.id
    );
    verdict(outcome, handed_off)
}

/// After the engine processes an insert-mode key, re-trigger or dismiss
/// CodeEdit's completion popup to match Godot's native behavior.
///
/// Godot natively calls the private `_filter_code_completion_candidates_impl`
/// after each typed character, which re-filters candidates and cancels the
/// popup when the word prefix is empty. We replicate that cancel logic here:
/// word chars and completion-prefix chars (`.`, etc.) retrigger; everything
/// else (`;`, `)`, space) cancels. Prefix chars come from CodeEdit's
/// `code_completion_prefixes` — a per-language set, not a hardcoded list.
///
/// Gated on `code_complete_enabled` so typing doesn't auto-trigger the popup
/// when the user has disabled auto-completion in EditorSettings.
pub(crate) fn maybe_retrigger_completion(
    engine: &VimEngine,
    key: KeyEvent,
    editor: &mut Gd<CodeEdit>,
    code_complete_enabled: bool,
) {
    if !code_complete_enabled {
        return;
    }

    let mode = engine.mode();
    if !mode.is_insert() && !mode.is_replace() {
        return;
    }

    match key.key() {
        Key::Char(c) if !c.is_control() && key.modifiers() == Modifiers::NONE => {
            if c.is_alphanumeric() || c == '_' || is_completion_prefix(editor, c) {
                editor.request_code_completion_ex().force(false).done();
            } else {
                editor.cancel_code_completion();
            }
        }
        Key::Backspace => {
            editor.request_code_completion_ex().force(false).done();
        }
        _ => {}
    }
}

/// Check if `ch` is in CodeEdit's `code_completion_prefixes` (e.g., `.` for
/// member access). These are per-language trigger characters configured by
/// Godot's script language providers.
fn is_completion_prefix(editor: &Gd<CodeEdit>, ch: char) -> bool {
    let prefixes = editor.get_code_completion_prefixes();
    let mut buf = [0u8; 4];
    let ch_str = ch.encode_utf8(&mut buf);
    prefixes.iter_shared().any(|p| *p.to_string() == *ch_str)
}

/// Confirm the selected completion and reconcile the text delta with the
/// engine so dot-repeat and macro recording capture the completed text.
///
/// Strategy: snapshot text before/after Godot's confirm, compute a minimal
/// contiguous diff (common-prefix / common-suffix), and feed it to the
/// engine as an `ExternalEdit`. The engine records the net-new text
/// internally for dot-repeat.
fn confirm_and_reconcile_completion(
    session: &mut VimSession<GodotHost>,
    editor: &mut Gd<CodeEdit>,
) {
    let before_text = editor.get_text().to_string();

    // CodeEdit replaces `code_completion_base` (the typed prefix) with the
    // selected item's `insert_text`. This is the only mutation.
    editor.confirm_code_completion_ex().replace(false).done();

    // Fix 4B: Invalidate cache IMMEDIATELY after confirm so that
    // host.text() reflects post-completion state for undo node sync.
    session.host_mut().invalidate_cache();

    let after_text = editor.get_text().to_string();
    let after_index = bridge::codec::LineIndex::new(&after_text);
    let after_byte = after_index.line_col_to_byte(
        &after_text,
        editor.get_caret_line(),
        editor.get_caret_column(),
    );

    super::reconcile::reconcile_external_text_change(
        session.engine_mut(),
        &before_text,
        &after_text,
        after_byte,
        vim_core::execution::ExternalEditKind::Completion,
    );

    // Fix 4A: Sync undo nodes so pressing `u` past a completion doesn't
    // silently skip it. The engine created an undo node during
    // reconciliation; we must create a matching UndoStore snapshot.
    super::sync_undo_nodes_after_external_edit(session, &before_text);
}

#[cfg(test)]
mod tests {
    use super::*;

    // The transport-side half of the P9 characterization suite. The verb-side
    // half lives in `actions::providers::completion`, driven by a fake popup;
    // this pins the fold that turns an `Outcome` plus one flag back into the
    // `Option<bool>` `process_cycle` has always spoken.

    #[test]
    fn a_handled_verb_consumes_the_key() {
        assert_eq!(verdict(Outcome::Handled, false), Some(true));
        assert_eq!(verdict(Outcome::FocusChanged, false), Some(true));
    }

    #[test]
    fn a_declined_verb_hands_the_key_to_the_vim_engine() {
        // Not "nothing happened". `godotvim.completion.dismiss` cancels the
        // popup and THEN declines, so `<Esc>` closes the popup and leaves
        // insert mode on one press. Collapsing this to `Some(false)` would
        // trap the user in insert mode; collapsing it to `Some(true)` would
        // stop `<CR>` inserting a newline whenever nothing was bound.
        assert_eq!(verdict(Outcome::Declined, false), None);
    }

    #[test]
    fn handing_off_beats_the_outcome_in_both_directions() {
        // `Some(false)` is the state `Outcome` cannot express: engine skipped,
        // event NOT consumed, control gets the key. The flag therefore has to
        // win over whatever the verb returned, and asserting BOTH rows is what
        // makes that a decision rather than an accident of ordering.
        assert_eq!(verdict(Outcome::Handled, true), Some(false));
        assert_eq!(verdict(Outcome::Declined, true), Some(false));
    }
}
