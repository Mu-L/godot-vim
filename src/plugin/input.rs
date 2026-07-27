//! Input handler implementations for the two Godot entry points: global
//! `input()` (cross-panel/dock navigation) and per-editor `gui_input()`
//! (keystroke processing through the Vim engine).

// Promote #[must_use] warnings to errors so that dropping an EngineOutcome
// without calling .apply_ui_update() or .discard() is a compile-time error.
#![deny(unused_must_use)]

use godot::classes::{CodeEdit, Control, EditorInterface, InputEvent, InputEventKey, Viewport};
use godot::global::Key;
use godot::prelude::*;

use crate::actions::action::{ActionCtx, Params};
use crate::actions::outcome::Outcome;
use crate::actions::resolve::{
    self, Candidate, CandidateTarget, Disposition, Resolution, ResolveInput,
};
use crate::actions::surface::{FocusChain, Seal, SurfacePath};
use crate::bridge;
use crate::controller::VimController;
use crate::ui::UiCoordinator;

use super::outcome::EngineOutcome;
use super::processing_guard::ProcessingKeyGuard;
use super::GodotVimCore;

/// The sampled chain, and the state it was sampled against.
///
/// `FocusChain::sample` walks the focus owner's ancestors and asks each one
/// six `is_class` questions, then runs an unbounded `is_ancestor_of` and, for
/// a `LineEdit`, a depth-20 sibling DFS. That is far too much to do at the OS
/// key-repeat rate, so it is done once per *distinct* key below and reused.
///
/// Every field of the key is a fact a probe reads. `editor_mode` is in there
/// for a reason worth stating: a mode change does **not** change the focus
/// owner, so keying on focus alone would leave a stale `Normal` behind when
/// the user typed `i` — and `editor.insert` would stop being a barrier while
/// Ctrl+H is backspace. Caching the expensive half and re-stamping the cheap
/// half would work equally well; keying on all of it is simpler and cannot
/// silently miss a field.
#[derive(Debug)]
pub(super) struct ChainCache {
    key: ChainKey,
    chain: FocusChain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChainKey {
    focus: Option<InstanceId>,
    attached: Option<InstanceId>,
    mode: Option<vim_core::primitives::Mode>,
    prompt: Option<InstanceId>,
    /// Bumped whenever the binding index is rebuilt, which is what makes a
    /// config reload invalidate the cache.
    generation: u64,
}

impl GodotVimCore {
    /// Global `input()` handler (Godot stage 1 — fires before `gui_input`).
    ///
    /// The staged model of `docs/DESIGN-rebindable-nav.md` §5, in order:
    /// transport guards → surface sampling → barrier → probes → per-surface
    /// hooks → resolution → arbitration → execution → consumption.
    ///
    /// Nothing here knows what a dock is, which widget classes navigate, or
    /// which key does what. It samples where the keystroke is, asks the
    /// binding plane what that means, runs the answer, and applies the
    /// answer's declared consumption policy. Adding a panel touches this
    /// function zero times.
    ///
    /// # Ordering note
    ///
    /// The design lists key identity (S1) before sampling (S2); this samples
    /// first and decodes after the barrier check. The two are observably
    /// identical — building a probe list has no side effects — and the swap
    /// buys back the early return the old dispatcher had: typing in Insert
    /// mode or inside a foreign `LineEdit` hits a `Barrier` and never runs
    /// `translate_key` a second time at the OS repeat rate.
    pub(super) fn handle_input_impl(&mut self, event: Gd<InputEvent>) {
        // ── S0: transport guards, unchanged ──────────────────────────
        if !self.enabled {
            return;
        }
        let Ok(key_event) = event.try_cast::<InputEventKey>() else {
            return;
        };
        if !key_event.is_pressed() {
            return;
        }
        if matches!(
            key_event.get_keycode(),
            Key::SHIFT
                | Key::CTRL
                | Key::ALT
                | Key::META
                | Key::CAPSLOCK
                | Key::NUMLOCK
                | Key::SCROLLLOCK
        ) {
            return;
        }

        let Some(base_control) = EditorInterface::singleton().get_base_control() else {
            return;
        };
        let Some(mut viewport) = base_control.get_viewport() else {
            return;
        };

        // Stale-editor self-heal. Phase 1 holds only an immutable borrow of
        // `attached_editor`; phase 2 releases it before taking `&mut self`.
        let stale = matches!(self.attached_editor.as_ref(), Some(e) if !e.is_instance_valid());
        if stale {
            // Deref-free: drop the stale handle. `detach()` is self-completing.
            self.detach();
            self.last_editor_id = None;
        }

        // ── S2: sample the chain (cached) and classify it ────────────
        let Some(path) = self.surface_path(&viewport) else {
            // Unreachable with the shipped forest — `unknown` probes
            // unconditionally — but a third-party forest with no total probe
            // must not consume the key.
            log::error!("input: no surface claimed the focus chain");
            return;
        };

        // ── S3: a barrier is a total hard stop, before any hook ──────
        if path.seal == Seal::Barrier {
            return;
        }

        // ── S1: one key vocabulary for the whole shell-side surface ──
        let probes = crate::actions::keys::probes(&key_event, self.langmap.as_ref());
        if probes.is_empty() {
            return;
        }
        let is_echo = key_event.is_echo();

        // ── S4: per-surface hooks, before any lookup ─────────────────
        self.run_surface_hooks(&path, &viewport);

        // ── S5/S6: resolve, then arbitrate ───────────────────────────
        let resolution = {
            let controller = self.controller.as_ref();
            // The polarity flip lives in `engine_claims`, written down once
            // and unit-tested there: no controller must mean INTERCEPT.
            let claims = |k: vim_core::keymap::KeyEvent| {
                resolve::engine_claims(controller, k, VimController::could_start_mapping)
            };
            resolve::resolve(&ResolveInput {
                probes: &probes,
                path: &path,
                index: &self.bindings,
                registry: &self.actions,
                vim_claims: &claims,
            })
        };
        let Resolution::Run {
            matched,
            candidates,
        } = resolution
        else {
            return;
        };

        // ── S7/S8: run the plan, then apply its consumption policy ───
        let target = viewport
            .gui_get_focus_owner()
            .map(godot::obj::Gd::upcast::<Control>);
        let disposition = resolve::dispose(&candidates, is_echo, |candidate| {
            self.run_candidate(candidate, target.clone())
        });

        // ── S9: commit on THIS transport's viewport ──────────────────
        if disposition == Disposition::Consume {
            log::trace!(
                "input: consumed {matched} via {}",
                candidates.first().map_or("<none>", |c| c.surface)
            );
            viewport.set_input_as_handled();
        }
    }

    /// Sample the focus chain (cached) and classify it into a surface path.
    ///
    /// Classifies against the index's OWN forest rather than a fresh
    /// `providers::forest()`: that constructor allocates a `Vec` of every
    /// declared surface, and this runs at the OS key-repeat rate. Reading the
    /// index's copy also guarantees the classification and the trie lookups
    /// that follow agree about what the forest is.
    fn surface_path(&mut self, viewport: &Gd<Viewport>) -> Option<SurfacePath> {
        self.refresh_focus_chain(viewport);
        let chain = &self.chain_cache.as_ref()?.chain;
        self.bindings.forest().classify(chain)
    }

    /// Re-sample the chain if anything a probe reads has changed.
    fn refresh_focus_chain(&mut self, viewport: &Gd<Viewport>) {
        let key = ChainKey {
            focus: viewport
                .gui_get_focus_owner()
                .map(|owner| owner.instance_id()),
            attached: self
                .attached_editor
                .as_ref()
                .filter(|e| e.is_instance_valid())
                .map(Gd::instance_id),
            mode: self.controller.as_ref().map(VimController::mode),
            prompt: self.fs_explorer.prompt_instance(),
            generation: self.bindings.generation,
        };
        if self.chain_cache.as_ref().is_none_or(|c| c.key != key) {
            log::trace!("input: resampling focus chain");
            let chain = FocusChain::sample(viewport, key.attached, key.mode, key.prompt);
            self.chain_cache = Some(ChainCache { key, chain });
        } else if let Some(cached) = self.chain_cache.as_ref() {
            // A hit whose chain disagrees with its own key means the key and
            // the sampler read different things — the exact desync that would
            // leave `editor.insert` classified as `editor.nav`. Cheap enough
            // to assert on every keystroke in a debug build.
            debug_assert_eq!(cached.chain.attached_editor, key.attached);
            debug_assert_eq!(cached.chain.editor_mode, key.mode);
        }
    }

    /// Drop the cached chain. Called wherever a fact the probes read can
    /// change without the focus owner, the mode, the prompt or the index
    /// generation changing with it — teardown and re-enable, mainly.
    pub(super) fn invalidate_focus_chain(&mut self) {
        self.chain_cache = None;
    }

    /// S4 — run every surface's `on_key` hook, deepest first.
    ///
    /// Before any lookup and regardless of whether a binding matches, because
    /// the one shipped hook belongs to no binding: it is the FileSystem
    /// dock's stale-prompt auto-dismiss, which used to run at the top of
    /// `handle_key` for every key including modified ones.
    fn run_surface_hooks(&mut self, path: &SurfacePath, viewport: &Gd<Viewport>) {
        // Collected first: the forest lookup borrows `self.bindings` and the
        // hook takes `&mut self.fs_explorer`. `ArrayVec`-free because the
        // shipped forest declares exactly one hook and the common case
        // allocates nothing at all — `Vec::new()` on the empty path.
        let hooks: Vec<fn(&mut ActionCtx<'_>)> = path
            .ids
            .iter()
            .filter_map(|id| self.bindings.forest().get(id).and_then(|spec| spec.on_key))
            .collect();
        if hooks.is_empty() {
            return;
        }
        let target = viewport
            .gui_get_focus_owner()
            .map(godot::obj::Gd::upcast::<Control>);
        let mut cx = ActionCtx::new(target, Params::new()).with_fs(&mut self.fs_explorer);
        for hook in hooks {
            hook(&mut cx);
        }
    }

    /// S7 — run one candidate.
    ///
    /// The `&'static ActionSpec` is copied out of the resolution rather than
    /// re-read from the registry, so no registry borrow spans the call and
    /// the action is free to take `&mut` on the plugin's own fields.
    fn run_candidate(&mut self, candidate: &Candidate, target: Option<Gd<Control>>) -> Outcome {
        match &candidate.target {
            CandidateTarget::Action(_, spec) => {
                let mut cx =
                    ActionCtx::new(target, candidate.params.clone()).with_fs(&mut self.fs_explorer);
                let outcome = (spec.run)(&mut cx);
                log::trace!("input: {} -> {outcome:?}", spec.id);
                outcome
            }
            CandidateTarget::Shortcut(path) => {
                // Deliberately not implemented at this phase. Delegating to
                // one of Godot's own shortcuts means re-injecting an event
                // into the same `_input` flush that dispatched it, and
                // `Input::parse_input_event` appends to the list
                // `flush_buffered_events` is draining — an unguarded
                // delegation is a hard editor hang, not a slow loop. The
                // registration-time cycle audit, the injection fingerprint
                // and the per-frame budget that make it safe are a phase of
                // their own; until then the rule declines and the key
                // reaches Godot, which is the failure direction that loses
                // nothing. No shipped default uses this target.
                log::warn!("input: <Shortcut>({path}) targets are not dispatched yet");
                Outcome::Declined
            }
        }
    }

    /// Per-editor keystroke handler. Connected to `gui_input` on the attached CodeEdit.
    pub(super) fn handle_gui_input_impl(&mut self, event: Gd<InputEvent>) {
        let Some(editor) = &self.attached_editor else {
            return;
        };
        // has_focus() guards against deferred delivery edge cases where gui_input
        // arrives after focus has moved away.
        if !editor.is_instance_valid() || !editor.has_focus() {
            return;
        }

        // ── Mouse wheel interception ─────────────────────────────────────
        // Route mouse wheel through the vim engine as Ctrl-Y (scroll up) /
        let Ok(key_event) = event.try_cast::<InputEventKey>() else {
            return;
        };
        // Accept both press and echo (key-repeat) -- held-key repeat is
        // correct Vim semantics (e.g. holding `j` to scroll down).
        if !key_event.is_pressed() {
            return;
        }
        let Some(key) = bridge::input::parse_godot_key(&key_event) else {
            return;
        };

        // IME compose guard: when TextEdit is actively composing (CJK input,
        // dead keys, alt-code unicode), don't consume the key — let it flow
        // through to TextEdit's native IME handling. Guards text-input modes
        // (Insert/Replace/CommandLine) where IME composition is meaningful.
        //
        // Escape-class keys (Escape, Ctrl+C, Ctrl+[) force-cancel the IME
        // composition so the user can always exit — even if the IME framework
        // doesn't cancel on Escape by itself.
        if editor.has_ime_text() {
            if let Some(controller) = &self.controller {
                let mode = controller.mode();
                if matches!(
                    mode,
                    vim_core::primitives::Mode::Insert
                        | vim_core::primitives::Mode::Replace
                        | vim_core::primitives::Mode::VirtualReplace
                        | vim_core::primitives::Mode::CommandLine
                ) {
                    let is_escape_key = matches!(key.key(), vim_core::keymap::Key::Escape)
                        || key == vim_core::keymap::KeyEvent::ctrl('c')
                        || key == vim_core::keymap::KeyEvent::ctrl('[');
                    if is_escape_key {
                        log::debug!("gui_input: force-cancelling IME for escape key={}", key);
                        let mut ed = editor.clone();
                        ed.cancel_ime();
                        // Fall through — Escape reaches the engine
                    } else {
                        log::trace!(
                            "gui_input: IME compose active in {:?}, passing through key={}",
                            mode,
                            key
                        );
                        return;
                    }
                } else {
                    // Stale IME composition in a non-insert mode — cancel it.
                    // This shouldn't happen (deactivate_ime cancels on mode exit),
                    // but some platforms/IMEs can leave stale state.
                    log::debug!("gui_input: cancelling stale IME in {:?}", mode);
                    let mut ed = editor.clone();
                    ed.cancel_ime();
                    // Fall through — key reaches the engine normally
                }
            }
        }

        // Cancel any pending tooltip — a new keystroke supersedes the hover.
        // Done before cloning `editor` to avoid borrow conflicts with `&mut self`.
        self.cancel_pending_tooltip();

        // Re-borrow after the mutable cancel call.
        let Some(editor) = &self.attached_editor else {
            return;
        };
        let mut ed = editor.clone();

        let outcome = {
            let _guard = ProcessingKeyGuard::new(&mut self.processing_key);
            let Some(controller) = &mut self.controller else {
                return;
            };
            controller.process_cycle(key, &mut ed)
        };

        let snap = {
            let Some(controller) = &mut self.controller else {
                return;
            };
            controller.ui_snapshot(ed.instance_id())
        };

        let applied = EngineOutcome::with_snapshot(snap, outcome)
            .apply_ui_update(&mut self.ui, &mut ed, &mut self.caret_reconciler);

        log::trace!(
            "gui_input: key={} outcome={}",
            key,
            applied.pipeline.log_label()
        );
        if applied.pipeline.should_mark_handled() {
            if let Some(mut vp) = editor.get_viewport() {
                vp.set_input_as_handled();
            }
        }

        if let Some(controller) = &mut self.controller {
            for action in controller.take_pending_ui_actions() {
                self.handle_pending_ui_action(action);
            }
        }

        // Start/restart the mapping timer if keys are buffered, stop if not.
        if let Some(controller) = &self.controller {
            if controller.has_pending_mapping() {
                if let Some(timer) = &mut self.mapping_timer {
                    let timeout_sec = controller.timeoutlen() as f64 / 1000.0;
                    timer.set_wait_time(timeout_sec);
                    timer.start();
                    log::trace!(
                        "gui_input: mapping timer started ({}ms)",
                        controller.timeoutlen()
                    );
                }
            } else if let Some(timer) = &mut self.mapping_timer {
                timer.stop();
            }
        }
    }

    /// Fired by the mapping timer after `timeoutlen` ms without further input.
    /// Flushes buffered keys as literals (or expands an exact match).
    pub(super) fn on_mapping_timeout_impl(&mut self) {
        let Some(editor) = &self.attached_editor else {
            return;
        };
        if !editor.is_instance_valid() {
            return;
        }
        let mut ed = editor.clone();

        let had_operations = {
            let _guard = ProcessingKeyGuard::new(&mut self.processing_key);
            if let Some(controller) = &mut self.controller {
                controller.resolve_mapping_timeout(&mut ed);
                controller.operations_this_cycle() > 0
            } else {
                false
            }
        };

        let editor_id = ed.instance_id();
        if let Some(controller) = &mut self.controller {
            let snap = controller.ui_snapshot(editor_id);
            // Use EngineConsumed when operations happened so apply_ui_update
            // sets a caret expectation; Passthrough otherwise so it does not.
            let pipeline = if had_operations {
                // Dummy ProcessResult -- only may_have_moved_cursor() matters,
                // which is true for EngineConsumed regardless of the payload.
                crate::controller::PipelineOutcome::EngineConsumed(
                    vim_core::execution::host_api::ProcessResult {
                        consumed: true,
                        host_requests: Vec::new(),
                        deferred_actions: Vec::new(),
                    },
                )
            } else {
                crate::controller::PipelineOutcome::Passthrough
            };
            EngineOutcome::with_snapshot(snap, pipeline)
                .apply_ui_update(&mut self.ui, &mut ed, &mut self.caret_reconciler);
        }

        if let Some(controller) = &mut self.controller {
            for action in controller.take_pending_ui_actions() {
                self.handle_pending_ui_action(action);
            }
        }

        // Timeout resolution may produce new pending keys (e.g. partial
        // match of a longer mapping) -- restart the timer so they resolve.
        if let Some(controller) = &self.controller {
            if controller.has_pending_mapping() {
                if let Some(timer) = &mut self.mapping_timer {
                    let timeout_sec = controller.timeoutlen() as f64 / 1000.0;
                    timer.set_wait_time(timeout_sec);
                    timer.start();
                }
            }
        }
    }

    /// Route a mouse-wheel event through the vim engine.
    ///
    /// Feeds Ctrl-Y (scroll up) or Ctrl-E (scroll down) through the normal
    /// `process_cycle` pipeline, repeating 3 times to match Godot's default
    /// scroll speed. Consumes the event so Godot's native scroll doesn't fire.
    /// Reconcile external cursor/selection changes with Vim engine state.
    /// Connected DEFERRED to avoid re-entrancy during text edits.
    ///
    /// Four cases based on (has_selection, vim_mode):
    /// 1. Selection + Normal  -- mouse drag entered Visual
    /// 2. No selection + Normal -- mouse click; sync sticky column
    /// 3. No selection + Visual -- click deselected; exit Visual
    /// 4. Selection + Visual  -- mouse extending; update Visual extents
    pub(super) fn on_caret_changed_impl(&mut self) {
        // Read caret position and check reconciler BEFORE any mutable borrows.
        // Uses a block to drop the immutable borrow of attached_editor before
        // cancel_pending_tooltip borrows &mut self.
        let (line, col) = {
            let Some(editor) = &self.attached_editor else {
                return;
            };
            if !editor.is_instance_valid() {
                return;
            }
            (editor.get_caret_line(), editor.get_caret_column())
        };

        match self.caret_reconciler.check_and_consume(line, col) {
            super::caret_reconcile::CaretOrigin::VimDriven => return,
            super::caret_reconcile::CaretOrigin::External => {}
        }

        self.cancel_pending_tooltip();

        let Some(controller) = &mut self.controller else {
            return;
        };

        let Some(editor) = &self.attached_editor else {
            return;
        };
        if !editor.is_instance_valid() {
            return;
        }
        let mut ed = editor.clone();
        let has_selection = ed.has_selection();
        let mode = controller.mode();

        if has_selection && !mode.is_visual_or_select() {
            log::debug!("on_caret_changed: mouse selection detected (entering visual)");
            apply_mouse_selection(
                controller,
                &mut ed,
                &mut self.caret_reconciler,
                &mut self.ui,
            );
        } else if !has_selection && mode.is_visual_or_select() {
            log::debug!("on_caret_changed: click deselect, exiting visual mode");
            let editor_id = ed.instance_id();
            controller.exit_mode_via_pipeline(&mut ed);
            controller.cleanup_visual_artifacts(editor_id, &mut ed);

            let char_col = crate::bridge::codec::i32_to_usize(ed.get_caret_column());
            let line_text = ed.get_line(ed.get_caret_line()).to_string();
            let grapheme_col = crate::bridge::codec::char_col_to_grapheme_col(&line_text, char_col);
            controller.set_engine_sticky_column(grapheme_col);

            let snap = controller.ui_snapshot(editor_id);
            EngineOutcome::with_snapshot(snap, crate::controller::PipelineOutcome::Passthrough)
                .apply_ui_update(&mut self.ui, &mut ed, &mut self.caret_reconciler);
        } else if has_selection && mode.is_visual_or_select() {
            log::trace!("on_caret_changed: visual selection updated");
            apply_mouse_selection(
                controller,
                &mut ed,
                &mut self.caret_reconciler,
                &mut self.ui,
            );
        } else {
            let char_col = crate::bridge::codec::i32_to_usize(ed.get_caret_column());
            let line_text = ed.get_line(ed.get_caret_line()).to_string();
            let grapheme_col = crate::bridge::codec::char_col_to_grapheme_col(&line_text, char_col);
            controller.set_engine_sticky_column(grapheme_col);
        }
    }
}

/// Translate Godot's selection extents into Vim anchor/head and forward to the
/// controller. Determines drag direction from caret position. Shared by Cases 1
/// (enter Visual) and 4 (extend Visual) in `on_caret_changed_impl`.
fn apply_mouse_selection(
    controller: &mut VimController,
    ed: &mut Gd<CodeEdit>,
    reconciler: &mut super::caret_reconcile::CaretReconciler,
    ui: &mut UiCoordinator,
) {
    let shape = detect_selection_shape(ed);

    let from_line = ed.get_selection_from_line();
    let from_col = ed.get_selection_from_column();
    let to_line = ed.get_selection_to_line();
    let to_col = ed.get_selection_to_column();

    // Godot puts the caret at the drag endpoint -- if caret is at the start
    // of the selection, the user dragged backward.
    let caret_line = ed.get_caret_line();
    let caret_col = ed.get_caret_column();
    let caret_at_start = caret_line == from_line && caret_col == from_col;

    let (anchor_line, anchor_col, head_line, head_col) = if caret_at_start {
        (to_line, to_col, from_line, from_col)
    } else {
        (from_line, from_col, to_line, to_col)
    };

    log::debug!(
        "apply_mouse_selection: shape={:?} anchor=({},{}) head=({},{})",
        shape,
        anchor_line,
        anchor_col,
        head_line,
        head_col
    );

    let did_change =
        controller.process_mouse_selection(ed, anchor_line, anchor_col, head_line, head_col, shape);

    if did_change {
        let editor_id = ed.instance_id();
        let snap = controller.ui_snapshot(editor_id);
        // Use EngineConsumed so apply_ui_update sets a caret expectation
        // (mouse selection moves the cursor from the engine's perspective).
        EngineOutcome::with_snapshot(
            snap,
            crate::controller::PipelineOutcome::EngineConsumed(
                vim_core::execution::host_api::ProcessResult {
                    consumed: true,
                    host_requests: Vec::new(),
                    deferred_actions: Vec::new(),
                },
            ),
        )
        .apply_ui_update(ui, ed, reconciler);
    }
}

/// Heuristic: Godot's triple-click produces a selection from col 0 to col 0
/// of the next line. Detecting this pattern lets us enter Visual Line mode
/// instead of Visual Char mode for triple-click selections.
fn detect_selection_shape(editor: &Gd<CodeEdit>) -> vim_core::primitives::SelectionShape {
    use vim_core::primitives::SelectionShape;

    let from_col = editor.get_selection_from_column();
    let to_line = editor.get_selection_to_line();
    let to_col = editor.get_selection_to_column();
    let from_line = editor.get_selection_from_line();

    if from_col == 0 && to_col == 0 && to_line > from_line {
        SelectionShape::Line
    } else {
        SelectionShape::Char
    }
}
