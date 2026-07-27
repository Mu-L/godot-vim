//! `dock.debugger` — the debugger's stack-frame and breakpoint trees.
//!
//! This file is the design's extensibility claim, executed. §7.1 promises that
//! a new shell-side subsystem costs **one file here plus one line in
//! [`PROVIDERS`](super::PROVIDERS)** — no edit to `src/plugin/input.rs`, to
//! `src/actions/resolve.rs`, to `src/actions/caps.rs`, or to any match arm
//! anywhere. Everything below is surface declaration, verb declaration and
//! default bindings written in the same `panelmap` text a user types. Nothing
//! in the dispatcher knows this file exists.
//!
//! # What it claims, and why the probe is narrow
//!
//! Godot's debugger dock is `EditorDebuggerNode`
//! (`editor/debugger/editor_debugger_node.h:47`, `GDCLASS(EditorDebuggerNode,
//! EditorDock)` in 4.8-dev). It holds a `TabContainer` of `ScriptEditorDebugger`
//! tabs, and the Stack Trace tab builds two focusable `Tree`s:
//! `stack_dump` — column title "Stack Frames" — and `breakpoints_tree`
//! (`editor/debugger/script_editor_debugger.cpp:2230` and `:2269`). Both class
//! strings were read from the checked-out engine source rather than guessed: a
//! wrong one produces a probe that silently never matches, and no headless test
//! can catch that.
//!
//! The probe is the conjunction "inside `EditorDebuggerNode` **and** focused on
//! a `Tree`", which is the same shape `dock.filesystem` uses and for the same
//! reason: `index_of_ancestor` alone would claim the tab bar, the thread
//! `OptionButton` and the variable filter `LineEdit`, and `y` would then try to
//! yank a stack frame from a focused button.
//!
//! The two trees are **not** discriminated from one another. They could be —
//! `breakpoints_tree`'s parent is an `HSplitContainer` while `stack_dump`'s is
//! a `VBoxContainer` — but that is editor-internal layout with no stable
//! guarantee across versions, and a probe built on it would fail silently the
//! first time someone reorders a container. So the verbs are named and
//! described for what they actually do: move within the focused debugger tree.
//!
//! # What this surface gets for free
//!
//! Naming `dock` as the parent is the entire cost of inheriting `j`/`k`
//! (`godotvim.item.next`/`prev`), `h`/`l`, `/` (`godotvim.dock.search`), `<CR>`
//! (`godotvim.item.activate` — which is what jumps to a breakpoint's source,
//! since `breakpoints_tree` connects `item_activated`), and `<Esc>`
//! (`godotvim.focus.editor`). Ctrl+hjkl still moves between panels, because
//! `panel` is `dock`'s ancestor and this seal is `Open`. All of it is listed by
//! `:panelmap`, explained by `:panelmap <lhs>`, and rebindable:
//!
//! ```vim
//! panelunmap dock.debugger J
//! panelmap   dock.debugger <C-n> godotvim.debugger.frame_next
//! panelmap   dock.debugger 3K    godotvim.debugger.frame_prev count=3
//! nnoremap   <leader>dy   <Action>(godotvim.debugger.yank_frame)
//! ```
//!
//! # What it deliberately does NOT ship
//!
//! §7.2 sketches `godotvim.debugger.step_over` delegating to Godot's own
//! `debugger/step_over` shortcut (F10, registered at
//! `editor/debugger/debugger_editor_plugin.cpp:49`). That verb is **not** here,
//! and the omission is the honest one. `RuleTarget::Shortcut` parses, registers
//! and lists today, but `GodotVimCore::run_candidate` declines it
//! (`src/plugin/input.rs:467-481`): delegating means re-injecting an event into
//! the same `_input` flush that dispatched it, and the injection-cycle audit
//! and per-frame budget that make that safe are a phase of their own. Shipping
//! `panelmap dock.debugger s <Shortcut>(debugger/step_over)` as a default would
//! be a key that consumes nothing and does nothing — the exact silent dead key
//! the whole design exists to prevent. F10/F11/F12 keep working because we
//! never claim them.
#![allow(
    dead_code,
    reason = "the surface and its verbs are reached through PROVIDERS, not by name"
)]

use godot::classes::{DisplayServer, Tree};
use godot::prelude::*;

use crate::actions::action::ActionSpec;
use crate::actions::caps::Caps;
use crate::actions::outcome::Outcome;
use crate::actions::surface::{Anchor, Seal, SurfaceSpec};
use crate::navigation::dock_nav::{handle_navigation, NavDirection};

use super::Provider;

/// Godot's debugger dock, and the ancestor the probe looks for.
///
/// Verified against `editor/debugger/editor_debugger_node.h:47-48`. A
/// `ScriptEditorDebugger` tab is always a descendant of one
/// (`editor_debugger_node.cpp:115-140`), so anchoring on the dock rather than
/// on the tab covers every tab this surface might grow into.
const DEBUGGER_DOCK_CLASS: &str = "EditorDebuggerNode";

/// Upper bound on a walk to the end of a debugger tree.
///
/// `handle_navigation` is itself bounded at 1000 items per call
/// (`dock_nav.rs:37`), so an unbounded outer loop would be a frozen editor
/// rather than a slow keystroke. A call stack deeper than this is pathological
/// and stopping short of the true end is a far better failure than a hang.
const MAX_WALK: u32 = 200;

pub(crate) static DOCK_DEBUGGER: SurfaceSpec = SurfaceSpec {
    id: "dock.debugger",
    parent: Some("dock"),
    seal: Seal::Open,
    // Nothing. `Tree` already contributes VNAV/HIERARCHY/ACTIVATE, and being a
    // debugger adds no affordance — which is the point §7.3 makes about
    // `Caps` being a closed vocabulary: this surface self-restricts in its
    // probe instead of inventing a `STACKFRAME` bit.
    grants: |_| Caps::empty(),
    probe: |chain| {
        (chain.index_of_ancestor(DEBUGGER_DOCK_CLASS).is_some() && chain.focus_is("Tree"))
            .then_some(Anchor::Node(0))
    },
    on_key: None,
    refuses_positional: false,
    yields_to_engine: false,
};

/// The focused control as a `Tree`, or `None`.
///
/// Every body below opens with this rather than assuming the probe already
/// proved it: `:action godotvim.debugger.yank_frame` and
/// `<Action>(godotvim.debugger.frame_next)` reach the same verb with no
/// surface, no keystroke and possibly no focus owner at all.
fn tree_target(cx: &mut crate::actions::action::ActionCtx<'_>) -> Option<Gd<Tree>> {
    cx.target().cloned()?.try_cast::<Tree>().ok()
}

/// Walk the focused tree, translating "nothing to move to" into a declination.
///
/// Declining is how this composes with Godot: at the end of the stack the key
/// falls through to the `Tree`'s own handling instead of dying here. An action
/// that cannot act and consumes anyway is a key sink on its surface.
fn walk(
    cx: &mut crate::actions::action::ActionCtx<'_>,
    direction: NavDirection,
    steps: u32,
) -> Outcome {
    let Some(target) = cx.target().cloned() else {
        return Outcome::Declined;
    };
    let mut moved = false;
    for _ in 0..steps {
        if handle_navigation(&target, direction, 0) {
            moved = true;
        } else {
            break;
        }
    }
    if moved {
        Outcome::Handled
    } else {
        Outcome::Declined
    }
}

pub(crate) static FRAME_NEXT: ActionSpec = ActionSpec {
    id: "godotvim.debugger.frame_next",
    desc: "Debugger: select the next stack frame or breakpoint",
    // The affordance, not the widget class — exactly as `godotvim.item.next`
    // does. A future debugger panel that is an ItemList inherits this for free.
    requires: Caps::VNAV,
    // Meaningless without a focused debugger tree, so a `:action` invocation
    // must fail loudly rather than decline invisibly.
    host_invocable: false,
    run: |cx| {
        let steps = cx.params.count();
        walk(cx, NavDirection::Next, steps)
    },
};

pub(crate) static FRAME_PREV: ActionSpec = ActionSpec {
    id: "godotvim.debugger.frame_prev",
    desc: "Debugger: select the previous stack frame or breakpoint",
    requires: Caps::VNAV,
    host_invocable: false,
    run: |cx| {
        let steps = cx.params.count();
        walk(cx, NavDirection::Prev, steps)
    },
};

pub(crate) static FRAME_LAST: ActionSpec = ActionSpec {
    id: "godotvim.debugger.frame_last",
    desc: "Debugger: select the deepest stack frame",
    requires: Caps::VNAV,
    host_invocable: false,
    run: |cx| walk(cx, NavDirection::Next, MAX_WALK),
};

pub(crate) static YANK_FRAME: ActionSpec = ActionSpec {
    id: "godotvim.debugger.yank_frame",
    desc: "Debugger: copy the selected row to the clipboard",
    // VNAV plus a cast, rather than a new capability bit. `Caps` has no "is a
    // Tree" bit and must not grow one: §7.3's rule is that a widget constraint
    // belongs in the probe (which already demands a `Tree`) and in the body,
    // not in the closed vocabulary every other surface has to carry.
    requires: Caps::VNAV,
    host_invocable: false,
    run: |cx| {
        let Some(tree) = tree_target(cx) else {
            return Outcome::Declined;
        };
        let Some(item) = tree.get_selected() else {
            return Outcome::Declined;
        };
        let text = item.get_text(0).to_string();
        if text.is_empty() {
            return Outcome::Declined;
        }
        DisplayServer::singleton().clipboard_set(&GString::from(&text));
        log::info!("debugger: yanked '{text}'");
        Outcome::Handled
    },
};

/// The verbs this provider contributes to the registry.
///
/// They live here rather than in `actions::specs::SHIPPED` on purpose: that is
/// what makes "one file plus one manifest line" true. `specs::SHIPPED` holds
/// the core keyset extracted in P2 and is not a registration point a third
/// party has to touch.
const ACTIONS: &[&ActionSpec] = &[&FRAME_NEXT, &FRAME_PREV, &FRAME_LAST, &YANK_FRAME];

/// Authored in the same text a user types and read by the same parser, so a
/// shipped default cannot drift into a dialect the documented grammar does not
/// describe.
///
/// No `<physical>` anywhere. The FileSystem and dock keysets carry it because
/// their meaning is a QWERTY *position* (hjkl, `/`); `J`/`K`/`G`/`y` here are
/// mnemonic — frame down, frame up, deepest, yank — so a positional probe
/// would be a guess about intent with nothing to gain.
///
/// Single-key throughout. A multi-key LHS such as `gg` is legal on this surface
/// (it is not editor-reachable, so V8 does not apply), but it would reserve `g`
/// on `dock.debugger` and arm P8's pending-prefix timer for a keyset this small
/// — cost with no payoff.
const DEFAULTS: &str = "\
panelmap dock.debugger J godotvim.debugger.frame_next
panelmap dock.debugger K godotvim.debugger.frame_prev
panelmap dock.debugger G godotvim.debugger.frame_last
panelmap dock.debugger y godotvim.debugger.yank_frame
";

pub(crate) const PROVIDER: Provider = Provider {
    tag: "godotvim.debugger",
    surfaces: &[&DOCK_DEBUGGER],
    actions: ACTIONS,
    defaults: DEFAULTS,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::surface::fixtures::*;
    use crate::actions::surface::{ChainNode, FocusChain};

    /// The real shape: focus owner, the tab's containers, the tab itself, the
    /// `TabContainer`, then the dock.
    fn in_debugger(node: ChainNode) -> FocusChain {
        FocusChain {
            nodes: vec![
                node,
                plain("VBoxContainer", 70),
                plain("HSplitContainer", 71),
                plain("ScriptEditorDebugger", 72),
                plain("TabContainer", 73),
                plain("EditorDebuggerNode", 74),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn the_stack_and_breakpoint_trees_both_anchor_here() {
        // `stack_dump` and `breakpoints_tree` are both plain `Tree`s
        // (script_editor_debugger.cpp:2230, :2269), so neither carries a
        // distinguishing class name and both must land on this surface.
        for node in [tree("Tree", 1), tree("SomeTreeSubclass", 2)] {
            assert_eq!(
                (DOCK_DEBUGGER.probe)(&in_debugger(node)),
                Some(Anchor::Node(0))
            );
        }
    }

    #[test]
    fn the_dock_ancestor_alone_is_not_enough() {
        // The thread OptionButton, the step buttons and the variable filter box
        // all live inside EditorDebuggerNode. Claiming them would hand `y` to a
        // focused button and `J` to a LineEdit the user is typing in.
        for node in [
            plain("Button", 3),
            plain("OptionButton", 4),
            line_edit(5),
            item_list("ItemList", 6),
            rich_text("RichTextLabel", 7),
        ] {
            assert_eq!((DOCK_DEBUGGER.probe)(&in_debugger(node)), None);
        }
    }

    #[test]
    fn a_tree_outside_the_debugger_is_not_ours() {
        // The Scene tree is a `Tree` too. Without the ancestor test this
        // surface would claim every dock in the editor and steal `y` from
        // `dock.filesystem`'s yank.
        let elsewhere = FocusChain {
            nodes: vec![tree("SceneTreeEditor", 8), plain("SceneTreeDock", 75)],
            ..Default::default()
        };
        assert_eq!((DOCK_DEBUGGER.probe)(&elsewhere), None);
    }

    #[test]
    fn no_focus_owner_is_not_a_debugger_tree() {
        assert_eq!((DOCK_DEBUGGER.probe)(&no_focus_owner()), None);
    }

    #[test]
    fn the_ancestor_class_string_is_the_one_godot_registers() {
        // A typo here is invisible: the probe simply never matches, every
        // binding on this surface is silently dead, and no other test can tell
        // the difference. Pinned against the literal read out of
        // editor/debugger/editor_debugger_node.h:47.
        assert_eq!(DEBUGGER_DOCK_CLASS, "EditorDebuggerNode");
    }

    #[test]
    fn it_is_declared_deeper_than_the_generic_dock() {
        // The whole of "inherits j/k/h/l, /, <CR> and <Esc>", as one link.
        assert_eq!(DOCK_DEBUGGER.parent, Some("dock"));
        assert_eq!(DOCK_DEBUGGER.seal, Seal::Open);
        assert!(!DOCK_DEBUGGER.yields_to_engine);
        assert!(!DOCK_DEBUGGER.refuses_positional);
    }

    #[test]
    fn the_surface_grants_nothing_and_the_widget_supplies_everything() {
        let chain = in_debugger(tree("Tree", 1));
        assert_eq!((DOCK_DEBUGGER.grants)(&chain), Caps::empty());
        assert_eq!(
            chain.widget_caps(),
            Caps::VNAV | Caps::HIERARCHY | Caps::ACTIVATE
        );
    }

    #[test]
    fn every_verb_is_gated_on_an_affordance_and_none_is_host_invocable() {
        // Both halves matter. `requires` is what makes a verb inert rather
        // than mis-firing on a widget that cannot answer it, and
        // `host_invocable: false` is what makes `:action` say "requires panel
        // focus" instead of declining invisibly.
        for spec in ACTIONS {
            assert_eq!(spec.requires, Caps::VNAV, "{}", spec.id);
            assert!(!spec.host_invocable, "{}", spec.id);
            assert!(!spec.desc.is_empty(), "{}", spec.id);
            assert!(
                crate::actions::action::is_valid_action_id(spec.id),
                "{}",
                spec.id
            );
            assert!(
                spec.id.starts_with("godotvim.debugger."),
                "{} escapes this provider's namespace",
                spec.id
            );
        }
    }

    #[test]
    fn every_verb_declines_without_a_target() {
        // `Anchor::Rootless` and the host transport both deliver a
        // target-less context. `Handled` there would consume the key and do
        // nothing, which is indistinguishable from a broken keyboard.
        let mut effects = Vec::new();
        for spec in ACTIONS {
            let mut cx = crate::actions::action::ActionCtx::recording(&mut effects);
            assert_eq!((spec.run)(&mut cx), Outcome::Declined, "{}", spec.id);
        }
        assert!(effects.is_empty(), "{effects:?}");
    }

    #[test]
    fn the_defaults_name_only_verbs_this_provider_declares() {
        // The anti-drift check that does not need the whole plane built: every
        // action id mentioned in the default text must be one of ours, so a
        // rename that misses the string fails here rather than at load.
        for line in DEFAULTS.lines().filter(|l| !l.is_empty()) {
            let id = line.rsplit(' ').next().expect("a target");
            assert!(
                ACTIONS.iter().any(|s| s.id == id),
                "'{id}' is not declared by this provider"
            );
        }
        assert_eq!(DEFAULTS.lines().count(), ACTIONS.len());
    }

    #[test]
    fn the_provider_tag_matches_the_verb_namespace() {
        assert_eq!(PROVIDER.tag, "godotvim.debugger");
        assert_eq!(PROVIDER.surfaces.len(), 1);
        assert_eq!(PROVIDER.actions.len(), 4);
    }
}
