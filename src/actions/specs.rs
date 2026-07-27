//! The shipped keyset, as named verbs.
//!
//! One `ActionSpec` per thing the plugin can do outside the editor. Naming
//! them is the whole point: inside the editor `<C-w>h` already had a name and
//! was therefore remappable, while the identical action from a dock was an
//! anonymous match arm. Once both address the same spec, one binding table
//! can serve both.
//!
//! Specs are gathered in a plain `const` array rather than registered by
//! link-time magic. A `cdylib` that Godot dlopens and hot-reloads under
//! `lto = "fat"` is the wrong place for life-before-main constructors, and an
//! array is compile-time checked, reviewable in a diff, and costs exactly one
//! line to extend.

use super::action::{ActionCtx, ActionSpec};
use super::caps::Caps;
use super::outcome::Outcome;
use crate::navigation::dock_nav::{
    handle_hierarchy, handle_navigation, HierarchyAction, NavDirection,
};
use crate::navigation::window::{WindowNavDirection, WindowNavResult};

/// Run a nav executor against the context's target, translating its `bool`
/// into the tri-state. `false` means "nothing to move to" — the end of a
/// list — which is a declination, not a failure: the key falls through and
/// Godot's own handling proceeds.
fn nav(cx: &mut ActionCtx<'_>, direction: NavDirection) -> Outcome {
    let count = cx.params.count();
    let Some(target) = cx.target().cloned() else {
        return Outcome::Declined;
    };
    let mut moved = false;
    for _ in 0..count {
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

fn hierarchy(cx: &mut ActionCtx<'_>, action: HierarchyAction) -> Outcome {
    let Some(target) = cx.target().cloned() else {
        return Outcome::Declined;
    };
    if handle_hierarchy(&target, action) {
        Outcome::Handled
    } else {
        Outcome::Declined
    }
}

// ── Item navigation ──────────────────────────────────────────────────────

pub(crate) static ITEM_NEXT: ActionSpec = ActionSpec {
    id: "godotvim.item.next",
    desc: "Move to the next item",
    // VNAV, not "has a list": Tree, ItemList AND RichTextLabel all answer to
    // j/k today, and the docs panel scrolls rather than selecting.
    requires: Caps::VNAV,
    host_invocable: false,
    run: |cx| nav(cx, NavDirection::Next),
};

pub(crate) static ITEM_PREV: ActionSpec = ActionSpec {
    id: "godotvim.item.prev",
    desc: "Move to the previous item",
    requires: Caps::VNAV,
    host_invocable: false,
    run: |cx| nav(cx, NavDirection::Prev),
};

pub(crate) static ITEM_COLLAPSE: ActionSpec = ActionSpec {
    id: "godotvim.item.collapse",
    desc: "Collapse the current item",
    // The capability gate that replaces `matches!(dock_kind, DockKind::Tree)`.
    // An ItemList does not offer HIERARCHY, so this is skipped there without
    // the dispatcher naming a widget class.
    requires: Caps::HIERARCHY,
    host_invocable: false,
    run: |cx| hierarchy(cx, HierarchyAction::Collapse),
};

pub(crate) static ITEM_EXPAND: ActionSpec = ActionSpec {
    id: "godotvim.item.expand",
    desc: "Expand the current item",
    requires: Caps::HIERARCHY,
    host_invocable: false,
    run: |cx| hierarchy(cx, HierarchyAction::Expand),
};

pub(crate) static ITEM_ACTIVATE: ActionSpec = ActionSpec {
    id: "godotvim.item.activate",
    desc: "Open or activate the current item",
    requires: Caps::ACTIVATE,
    host_invocable: false,
    run: |cx| {
        // Delegates rather than re-emitting by hand, and that is not
        // fastidiousness. The two widgets have DIFFERENT signal contracts:
        // `Tree::item_activated` takes no parameters and `item_selected` is
        // not emitted at all, while `ItemList` emits both WITH an index and
        // only when something is selected (godot scene/gui/tree.cpp:7522,7534
        // vs item_list.cpp:2482,2486). Passing an argument to Tree's zero-arg
        // signal is CALL_ERROR_TOO_MANY_ARGUMENTS — the editor's handler
        // silently does not run, so Enter on the Scene tree stops working.
        // A hand-written "equivalent" got exactly that wrong; this cannot.
        let Some(target) = cx.target().cloned() else {
            return Outcome::Declined;
        };
        let Some(kind) = crate::navigation::dock::dock_kind_of(&target) else {
            return Outcome::Declined;
        };
        crate::navigation::dock::handle_enter(&target, kind)
    },
};

// ── Cross-panel focus ────────────────────────────────────────────────────
//
// These require NO capability. That is what lets them still fire when there
// is no focus owner at all — the case the dispatcher must consume for. The
// `cx.target()` guard below is the verbatim transcription of the old
// `input.rs:127-130`, where `handle_window_nav` was skipped with no focus
// owner and `set_input_as_handled()` fired anyway; `Consumption::Void` on the
// four `panel` rules supplies the consume that `:132` supplied.

/// Directional cross-panel movement.
///
/// The `Declined` on a miss is not busywork even though every shipped binding
/// for it is `Void`: a user who writes `panelmap panel <M-h> godotvim.focus.left`
/// gets an elastic rule, and then "no panel that way" must leave the chord to
/// Godot rather than swallowing it.
fn focus_dir(cx: &mut ActionCtx<'_>, direction: WindowNavDirection) -> Outcome {
    let Some(target) = cx.target().cloned() else {
        return Outcome::Declined;
    };
    match crate::navigation::handle_window_nav(&target, direction) {
        WindowNavResult::Focused => Outcome::FocusChanged,
        WindowNavResult::Ignored => Outcome::Declined,
    }
}

fn focus_cycle(cx: &mut ActionCtx<'_>, action: crate::effects::WindowNavAction) -> Outcome {
    let Some(target) = cx.target().cloned() else {
        return Outcome::Declined;
    };
    crate::navigation::handle_window_nav_action(&target, action);
    Outcome::FocusChanged
}

pub(crate) static FOCUS_LEFT: ActionSpec = ActionSpec {
    id: "godotvim.focus.left",
    desc: "Move focus to the panel on the left",
    requires: Caps::empty(),
    host_invocable: true,
    run: |cx| focus_dir(cx, WindowNavDirection::Left),
};

pub(crate) static FOCUS_RIGHT: ActionSpec = ActionSpec {
    id: "godotvim.focus.right",
    desc: "Move focus to the panel on the right",
    requires: Caps::empty(),
    host_invocable: true,
    run: |cx| focus_dir(cx, WindowNavDirection::Right),
};

pub(crate) static FOCUS_UP: ActionSpec = ActionSpec {
    id: "godotvim.focus.up",
    desc: "Move focus to the panel above",
    requires: Caps::empty(),
    host_invocable: true,
    run: |cx| focus_dir(cx, WindowNavDirection::Up),
};

pub(crate) static FOCUS_DOWN: ActionSpec = ActionSpec {
    id: "godotvim.focus.down",
    desc: "Move focus to the panel below",
    requires: Caps::empty(),
    host_invocable: true,
    run: |cx| focus_dir(cx, WindowNavDirection::Down),
};

pub(crate) static FOCUS_CYCLE_NEXT: ActionSpec = ActionSpec {
    id: "godotvim.focus.cycle_next",
    desc: "Cycle focus to the next panel",
    requires: Caps::empty(),
    host_invocable: true,
    run: |cx| focus_cycle(cx, crate::effects::WindowNavAction::CycleNext),
};

pub(crate) static FOCUS_CYCLE_PREV: ActionSpec = ActionSpec {
    id: "godotvim.focus.cycle_prev",
    desc: "Cycle focus to the previous panel",
    requires: Caps::empty(),
    host_invocable: true,
    run: |cx| focus_cycle(cx, crate::effects::WindowNavAction::CyclePrev),
};

pub(crate) static FOCUS_EDITOR: ActionSpec = ActionSpec {
    id: "godotvim.focus.editor",
    desc: "Return focus to the script editor",
    requires: Caps::empty(),
    host_invocable: true,
    // Needs no target: it locates the script editor itself, and declines when
    // there is none — which is why `Caps::ESCAPE` was deleted as a gate with
    // no possible grantor.
    run: |_cx| crate::navigation::dock::handle_escape_from_dock(),
};

// ── Dock filter box ──────────────────────────────────────────────────────

pub(crate) static DOCK_SEARCH: ActionSpec = ActionSpec {
    id: "godotvim.dock.search",
    desc: "Focus the dock's filter box",
    // No capability: the depth-20 sibling DFS runs once per `/` press and
    // declines when it finds nothing, which is a better gate than any bit —
    // it asks the actual scene tree rather than the widget class.
    requires: Caps::empty(),
    host_invocable: false,
    run: |cx| {
        let Some(target) = cx.target().cloned() else {
            return Outcome::Declined;
        };
        crate::navigation::dock::handle_slash(&target)
    },
};

pub(crate) static SEARCH_ACCEPT: ActionSpec = ActionSpec {
    id: "godotvim.search.accept",
    desc: "Leave the filter box, keeping the filter",
    requires: Caps::TEXTENTRY,
    host_invocable: false,
    run: |cx| {
        let Some(target) = cx.target().cloned() else {
            return Outcome::Declined;
        };
        crate::navigation::dock::leave_search(&target)
    },
};

// ── FileSystem operations ────────────────────────────────────────────────
//
// `host_invocable` because each can locate its own target, so `:action
// godotvim.fs.create` works from the editor as well as from the dock.
//
// Only `create` needs `&mut FileSystemExplorer` — it owns the prompt — and
// only the key transports lend one (`ActionCtx::with_fs`). The other four are
// free functions over the target, which is what makes `host_invocable: true`
// honest for them rather than aspirational.

/// The focused control and the signal contract it follows.
///
/// `Declined` on a missing `DockKind` reproduces the old dispatch
/// precondition exactly: `FocusContext::Dock(kind, control)` could only be
/// constructed for a Tree, ItemList or RichTextLabel, so `handle_key` was
/// unreachable for anything else.
fn dock_target(
    cx: &mut ActionCtx<'_>,
) -> Option<(
    godot::prelude::Gd<godot::classes::Control>,
    crate::navigation::dock::DockKind,
)> {
    let target = cx.target().cloned()?;
    let kind = crate::navigation::dock::dock_kind_of(&target)?;
    Some((target, kind))
}

pub(crate) static FS_CREATE: ActionSpec = ActionSpec {
    id: "godotvim.fs.create",
    desc: "Create a file or folder",
    requires: Caps::FILEOPS,
    host_invocable: true,
    run: |cx| {
        let Some((target, kind)) = dock_target(cx) else {
            return Outcome::Declined;
        };
        let Some(fs) = cx.fs() else {
            // The host transport lends no explorer. Decline loudly rather
            // than half-running: `begin_create` without the prompt would
            // report success and show nothing.
            log::warn!("godotvim.fs.create: no FileSystem explorer on this transport");
            return Outcome::Declined;
        };
        fs.begin_create(&target, kind)
    },
};

pub(crate) static FS_DELETE: ActionSpec = ActionSpec {
    id: "godotvim.fs.delete",
    desc: "Delete the selected path",
    requires: Caps::FILEOPS,
    host_invocable: true,
    run: |_cx| crate::navigation::filesystem_explorer::delete_selected(),
};

pub(crate) static FS_RENAME: ActionSpec = ActionSpec {
    id: "godotvim.fs.rename",
    desc: "Rename the selected path",
    requires: Caps::FILEOPS,
    host_invocable: true,
    run: |_cx| crate::navigation::filesystem_explorer::rename_selected(),
};

pub(crate) static FS_YANK_PATH: ActionSpec = ActionSpec {
    id: "godotvim.fs.yank_path",
    desc: "Copy the selected path to the clipboard",
    requires: Caps::FILEOPS,
    host_invocable: true,
    run: |cx| {
        let Some((target, kind)) = dock_target(cx) else {
            return Outcome::Declined;
        };
        crate::navigation::filesystem_explorer::yank_selected_path(&target, kind)
    },
};

pub(crate) static FS_REFRESH: ActionSpec = ActionSpec {
    id: "godotvim.fs.refresh",
    desc: "Rescan the filesystem",
    requires: Caps::FILEOPS,
    host_invocable: true,
    run: |_cx| crate::navigation::filesystem_explorer::scan_filesystem(),
};

/// Every shipped action.
///
/// Adding one here is the entire cost of adding a verb — no dispatcher edit,
/// no match arm, no widget taxonomy.
///
/// This array is **not** the only registration point, and deliberately so:
/// `Provider::actions` is the other, and it is the one a new subsystem uses.
/// Build the registry through [`registry`] rather than looping this directly,
/// or a provider's verbs go missing and its shipped defaults fail to load.
pub(crate) const SHIPPED: &[&ActionSpec] = &[
    &ITEM_NEXT,
    &ITEM_PREV,
    &ITEM_COLLAPSE,
    &ITEM_EXPAND,
    &ITEM_ACTIVATE,
    &FOCUS_LEFT,
    &FOCUS_RIGHT,
    &FOCUS_UP,
    &FOCUS_DOWN,
    &FOCUS_CYCLE_NEXT,
    &FOCUS_CYCLE_PREV,
    &FOCUS_EDITOR,
    &DOCK_SEARCH,
    &SEARCH_ACCEPT,
    &FS_CREATE,
    &FS_DELETE,
    &FS_RENAME,
    &FS_YANK_PATH,
    &FS_REFRESH,
];

/// The whole registry: the core keyset above, then every provider's own verbs.
///
/// The single seam. Before it existed the registry was assembled by five
/// separate `for spec in SHIPPED` loops — one in `GodotVimCore::init` and four
/// in test modules — and none of them consulted `PROVIDERS`, so a new provider
/// that shipped a verb had to edit all five. That is exactly the per-subsystem
/// cost §7.1 claims not to charge, which is why it is one function now.
///
/// Order is `SHIPPED` first, then `PROVIDERS` order. Ids are minted by an
/// append-only interner, so that order is what makes `ActionId`s stable across
/// a rebuild — the introspector's golden snapshots depend on it.
pub(crate) fn registry() -> super::action::ActionRegistry {
    let mut r = super::action::ActionRegistry::new();
    for spec in SHIPPED {
        r.register(spec);
    }
    for spec in super::providers::actions() {
        r.register(spec);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::action::{is_valid_action_id, ActionCtx, ActionRegistry};

    #[test]
    fn every_shipped_id_is_well_formed() {
        // P3 splits its namespace from Godot's shortcut paths on the dot, so
        // an id without one would be ambiguous.
        for spec in SHIPPED {
            assert!(is_valid_action_id(spec.id), "{} is malformed", spec.id);
            assert!(!spec.desc.is_empty(), "{} has no description", spec.id);
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut seen: Vec<&str> = SHIPPED.iter().map(|s| s.id).collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total, "duplicate action id");
    }

    #[test]
    fn registration_is_idempotent_and_round_trips() {
        let mut r = registry();
        let before = r.len();
        for spec in SHIPPED {
            r.register(spec);
        }
        assert_eq!(r.len(), before, "re-registering must not grow the registry");

        for spec in SHIPPED {
            let id = r.id_of(spec.id).expect("registered");
            assert_eq!(r.name_of(id), Some(spec.id));
            assert_eq!(r.get(id).map(|s| s.id), Some(spec.id));
        }
    }

    #[test]
    fn an_id_carries_itself_as_a_pseudo_key() {
        let r = registry();
        let id = r.id_of("godotvim.focus.left").unwrap();
        assert_eq!(id.as_key().key(), vim_core::keymap::Key::Action(id.0));
    }

    #[test]
    fn activation_declines_without_a_target() {
        // It delegates to `handle_enter`, which needs a real control. Pinned
        // here: it declines rather than consuming the key.
        let mut effects = Vec::new();
        let r = registry();
        let id = r.id_of("godotvim.item.activate").unwrap();
        let mut cx = ActionCtx::recording(&mut effects);
        assert_eq!(r.run(id, &mut cx), Outcome::Declined);
        assert!(effects.is_empty());
    }

    // ── The capability gate ──────────────────────────────────────────
    //
    // These assert the DECLARED requirement, not a runtime Outcome. That
    // distinction is the whole point: `ActionCtx::recording` has no Godot
    // target, so an executor declines for want of one — the same value a
    // capability miss produces. A test asserting `Declined` cannot tell the
    // two apart, and mutating `requires` to nonsense would not fail it.
    // Asserting the requirement does fail, which is what makes these guards.

    /// A spec whose body is observable without a Godot target, so the registry
    /// contract can be tested independently of any shipped action.
    static PROBE: ActionSpec = ActionSpec {
        id: "test.probe.ran",
        desc: "records that it ran",
        requires: Caps::FILEOPS,
        host_invocable: true,
        run: |cx| {
            cx.emit("ran", None);
            Outcome::Handled
        },
    };

    #[test]
    fn run_does_not_consult_requires() {
        // The rule the design states three times: Caps gates BINDINGS, never
        // invocation. `:action godotvim.fs.refresh` from the command line has
        // no keystroke, no surface and no sampled widget, so it arrives with
        // no capabilities — and must still reach its body.
        let mut r = ActionRegistry::new();
        let id = r.register(&PROBE);
        let mut effects = Vec::new();
        let mut cx = ActionCtx::recording(&mut effects);
        assert_eq!(r.run(id, &mut cx), Outcome::Handled);
        assert_eq!(effects.len(), 1, "the body must actually have run");
        // ...while the binding path DOES gate, on the same spec.
        assert!(!r.caps_allow(id, Caps::empty()));
        assert!(r.caps_allow(id, Caps::FILEOPS));
    }

    #[test]
    fn hierarchy_actions_require_hierarchy() {
        let r = registry();
        for name in ["godotvim.item.collapse", "godotvim.item.expand"] {
            let id = r.id_of(name).unwrap();
            assert_eq!(r.get(id).unwrap().requires, Caps::HIERARCHY, "{name}");
            let item_list = Caps::VNAV | Caps::ACTIVATE;
            assert!(
                !r.caps_allow(id, item_list),
                "{name} must be inert on a list"
            );
            assert!(
                r.caps_allow(id, item_list | Caps::HIERARCHY),
                "{name} on a tree"
            );
        }
    }

    #[test]
    fn item_navigation_requires_only_vnav() {
        // THE regression the vocabulary exists to prevent: j/k work on
        // RichTextLabel today — the docs panel and the Output log scroll.
        let r = registry();
        for name in ["godotvim.item.next", "godotvim.item.prev"] {
            let id = r.id_of(name).unwrap();
            assert_eq!(r.get(id).unwrap().requires, Caps::VNAV, "{name}");
            for class in ["Tree", "ItemList", "RichTextLabel"] {
                assert!(
                    r.caps_allow(id, Caps::of_control(|c| c == class)),
                    "{name} must work on a {class}"
                );
            }
        }
    }

    #[test]
    fn activation_requires_activation() {
        let r = registry();
        let id = r.id_of("godotvim.item.activate").unwrap();
        assert_eq!(r.get(id).unwrap().requires, Caps::ACTIVATE);
        assert!(!r.caps_allow(id, Caps::of_control(|c| c == "RichTextLabel")));
        assert!(r.caps_allow(id, Caps::of_control(|c| c == "Tree")));
    }

    #[test]
    fn focus_movement_requires_no_capability() {
        let r = registry();
        for name in [
            "godotvim.focus.left",
            "godotvim.focus.right",
            "godotvim.focus.up",
            "godotvim.focus.down",
            "godotvim.focus.cycle_next",
            "godotvim.focus.cycle_prev",
            "godotvim.focus.editor",
        ] {
            let id = r.id_of(name).unwrap();
            assert_eq!(r.get(id).unwrap().requires, Caps::empty(), "{name}");
            assert!(r.caps_allow(id, Caps::empty()), "{name}");
        }
    }

    #[test]
    fn filesystem_actions_require_fileops() {
        let r = registry();
        for name in [
            "godotvim.fs.create",
            "godotvim.fs.delete",
            "godotvim.fs.rename",
            "godotvim.fs.yank_path",
            "godotvim.fs.refresh",
        ] {
            let id = r.id_of(name).unwrap();
            assert_eq!(r.get(id).unwrap().requires, Caps::FILEOPS, "{name}");
            assert!(!r.caps_allow(id, Caps::VNAV), "{name}");
            assert!(r.caps_allow(id, Caps::VNAV | Caps::FILEOPS), "{name}");
        }
    }

    #[test]
    fn an_action_with_no_target_declines() {
        // The no-focus-owner path, which is a real and mandatory state:
        // `Anchor::Rootless` reaches `panel`'s Ctrl+hjkl rules with
        // `cx.target()` at `None`. Every body that needs a control must open
        // with a `let ... else { return Declined }`, because `Handled` means
        // "consume the key AND report success" — which would destroy the
        // keystroke and do nothing.
        //
        // `Consumption::Void` is what still consumes there; the action's
        // honesty and the rule's policy are two separate decisions, and this
        // asserts the first.
        //
        // Excluded: the three targetless FileSystem verbs. They reach
        // `EditorInterface::singleton()` unconditionally, which panics
        // outside a running editor — that is what makes them
        // `host_invocable` and is asserted by shape, in `SHIPPED`, rather
        // than by running them.
        let mut effects = Vec::new();
        let r = registry();
        for name in [
            "godotvim.focus.left",
            "godotvim.focus.right",
            "godotvim.focus.up",
            "godotvim.focus.down",
            "godotvim.focus.cycle_next",
            "godotvim.focus.cycle_prev",
            "godotvim.dock.search",
            "godotvim.search.accept",
            "godotvim.fs.create",
            "godotvim.fs.yank_path",
            "godotvim.item.next",
            "godotvim.item.prev",
            "godotvim.item.collapse",
            "godotvim.item.expand",
            "godotvim.item.activate",
        ] {
            let id = r.id_of(name).unwrap();
            let mut cx = ActionCtx::recording(&mut effects);
            assert_eq!(r.run(id, &mut cx), Outcome::Declined, "{name} must decline");
        }
        assert!(
            effects.is_empty(),
            "a targetless action must not emit anything: {effects:?}"
        );
    }

    #[test]
    fn an_unknown_id_declines_instead_of_panicking() {
        let r = registry();
        let mut effects = Vec::new();
        let mut cx = ActionCtx::recording(&mut effects);
        let bogus = crate::actions::action::ActionId(9999);
        assert_eq!(r.run(bogus, &mut cx), Outcome::Declined);
    }

    #[test]
    fn only_self_locating_actions_are_host_invocable() {
        // `:action godotvim.fs.create` works from anywhere because the FS
        // actions can find their own target. `godotvim.item.next` cannot —
        // it needs a focused control — so a host request must fail loudly
        // rather than decline invisibly.
        let r = registry();
        for (name, want) in [
            ("godotvim.fs.create", true),
            ("godotvim.focus.left", true),
            ("godotvim.item.next", false),
            ("godotvim.dock.search", false),
        ] {
            let spec = r.get(r.id_of(name).unwrap()).unwrap();
            assert_eq!(spec.host_invocable, want, "{name}");
        }
    }

    #[test]
    fn the_registry_enumerates_everything_for_the_introspector() {
        let r = registry();
        let provider_verbs = crate::actions::providers::actions();
        assert_eq!(r.iter().count(), SHIPPED.len() + provider_verbs.len());
        let names: Vec<&str> = r.iter().map(|(_, s)| s.id).collect();
        assert!(names.contains(&"godotvim.fs.refresh"));
        // The union, asserted from the other side: a provider verb that never
        // reaches the registry is a shipped default that cannot load.
        for spec in provider_verbs {
            assert!(names.contains(&spec.id), "{} is not registered", spec.id);
        }
    }

    // ── The host bridge namespace split (P3) ─────────────────────────

    /// Mirrors the split at the `HostRequest::RunAction` arm: dotted and
    /// slash-free is ours, anything else falls through to Godot's shortcuts.
    fn is_registry_name(name: &str) -> bool {
        name.contains('.') && !name.contains('/')
    }

    #[test]
    fn every_shipped_action_is_claimed_by_the_registry_probe() {
        for spec in SHIPPED {
            assert!(is_registry_name(spec.id), "{} would fall through", spec.id);
        }
    }

    #[test]
    fn godot_editor_shortcut_paths_are_never_claimed() {
        // These are Godot's own namespace. Claiming one would shadow a real
        // editor shortcut and silently break it.
        for path in [
            "filesystem_dock/delete",
            "filesystem_dock/rename",
            "scene_tree/rename",
            "editor/save_scene",
            "debugger/step_over",
        ] {
            assert!(!is_registry_name(path), "{path} must reach Godot");
        }
    }

    #[test]
    fn a_bare_name_is_not_claimed_either() {
        // No dot means no namespace. Falling through is the safe direction:
        // Godot answers or nothing happens, rather than us guessing.
        for name in ["save", "quit", "ui_accept"] {
            assert!(!is_registry_name(name), "{name} must not be claimed");
        }
    }

    #[test]
    fn only_self_locating_actions_accept_a_host_invocation() {
        // `host_invocable: false` must FAIL LOUDLY rather than decline
        // invisibly — an action with nothing to act on is a user error worth
        // reporting, not a silent no-op.
        let r = registry();
        for (name, want) in [
            ("godotvim.fs.create", true),
            ("godotvim.fs.refresh", true),
            ("godotvim.focus.left", true),
            ("godotvim.item.next", false),
            ("godotvim.item.activate", false),
            ("godotvim.dock.search", false),
        ] {
            let spec = r.get(r.id_of(name).unwrap()).unwrap();
            assert_eq!(spec.host_invocable, want, "{name}");
        }
    }

    #[test]
    fn host_invocation_ignores_capabilities_entirely() {
        // `:action godotvim.fs.refresh` from the command line arrives with no
        // capabilities at all. If the host path gated on them it would
        // decline everything — which is exactly what host_invocable exists to
        // prevent. Proven against a probe spec so no shipped body is involved.
        let mut r = ActionRegistry::new();
        let id = r.register(&PROBE);
        let mut effects = Vec::new();
        let mut cx = ActionCtx::recording(&mut effects);
        assert_eq!(r.run(id, &mut cx), Outcome::Handled);
        assert!(
            !r.caps_allow(id, Caps::empty()),
            "the BINDING path still gates"
        );
    }
}
