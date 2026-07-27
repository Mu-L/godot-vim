//! Top-level dock input dispatcher.
//!
//! Routes plain (unmodified) keystrokes to dock navigation handlers based on
//! the focused control's `DockKind`. This gives Vim-style j/k/h/l navigation
//! within Godot's Tree, ItemList, and RichTextLabel dock controls, plus `/`
//! to focus the dock's search box and `ESC` to return to the code editor.
//!
//! Modified keys (Ctrl/Alt/Meta/Shift) always pass through: Ctrl+hjkl is
//! intercepted at a higher priority in `input.rs` for cross-panel navigation.

use godot::classes::{CodeEdit, Control, EditorInterface, Node};
use godot::prelude::*;
use vim_core::keymap::{Key as VimKey, KeyEvent, Modifiers};

use crate::actions::action::{ActionCtx, ActionRegistry, Params};
use crate::actions::caps::Caps;
use crate::actions::keys::{Probes, CMD_MODS};
use crate::actions::specs;

use super::dock_search::{find_sibling_nav_control, find_sibling_search_box};
use super::focus::DockKind;
use crate::scene_tree::{find_child_of_type, MAX_DISCOVERY_DEPTH};

/// Retained so every existing call site compiles unchanged.
///
/// A `type` alias DOES resolve variants (RFC 2338, Rust 1.37+), which is why
/// `DockInputResult::Declined` still works here — but it cannot *rename* one,
/// which is why the `Ignored` → `Declined` rename had to land first.
pub(crate) use crate::actions::outcome::Outcome as DockInputResult;

/// Direction for hjkl dock navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DockHjkl {
    Down,
    Up,
    Left,
    Right,
}

/// What a plain (unmodified) keystroke resolves to in a dock, before any
/// widget is touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DockKeyAction {
    Hjkl(DockHjkl),
    Slash,
    Enter,
    Escape,
    None,
}

/// The dock action bound to a single key interpretation.
///
/// Docks reject **every** modifier, Shift included — a modified key belongs
/// to the editor or the IDE, not here. The search box is deliberately more
/// permissive; see [`resolve_search_key`].
fn dock_action_for(key: KeyEvent) -> Option<DockKeyAction> {
    if key.modifiers() != Modifiers::NONE {
        return None;
    }
    match key.key() {
        VimKey::Char('j') => Some(DockKeyAction::Hjkl(DockHjkl::Down)),
        VimKey::Char('k') => Some(DockKeyAction::Hjkl(DockHjkl::Up)),
        VimKey::Char('h') => Some(DockKeyAction::Hjkl(DockHjkl::Left)),
        VimKey::Char('l') => Some(DockKeyAction::Hjkl(DockHjkl::Right)),
        VimKey::Char('/') => Some(DockKeyAction::Slash),
        VimKey::Enter => Some(DockKeyAction::Enter),
        VimKey::Escape => Some(DockKeyAction::Escape),
        _ => None,
    }
}

/// Resolve a keystroke to a dock action.
///
/// Every probe is tried against the **whole** keyset before the next probe is
/// tried against any of it. That ordering is what makes `/` reachable again:
/// under the old per-arm fallback the hjkl arm consulted the physical keycode
/// and returned before the arm owning `Key::SLASH` was reached, so on a layout
/// whose QWERTY-J position emits `/` the filter box was unreachable.
fn resolve_dock_key(probes: &Probes) -> DockKeyAction {
    probes
        .resolve(dock_action_for)
        .unwrap_or(DockKeyAction::None)
}

pub(crate) fn handle_dock_input(
    focused: Gd<Control>,
    probes: &Probes,
    dock_kind: DockKind,
    registry: &ActionRegistry,
) -> DockInputResult {
    log::trace!(
        "dock_input: key={:?} kind={:?}",
        probes.primary(),
        dock_kind
    );

    // One ordered probe list covers the whole keyset; see `actions::keys`.
    // Enter and Escape never receive a positional probe — that is enforced in
    // `probes_from_parts`, not left to chance.
    let action = resolve_dock_key(probes);
    if let DockKeyAction::Hjkl(direction) = action {
        // Dispatch through the action plane. The capability gate is what makes
        // `h`/`l` inert on a list with no hierarchy — no `DockKind` match arm
        // here, and no widget class named anywhere in this function.
        // Named by static, not by string literal: a typo would otherwise
        // compile clean, miss the registry, and silently kill hjkl in every
        // dock with a green test suite.
        let spec = match direction {
            DockHjkl::Down => &specs::ITEM_NEXT,
            DockHjkl::Up => &specs::ITEM_PREV,
            DockHjkl::Left => &specs::ITEM_COLLAPSE,
            DockHjkl::Right => &specs::ITEM_EXPAND,
        };
        let Some(id) = registry.id_of(spec.id) else {
            log::error!("dock_input: {} is not registered", spec.id);
            return DockInputResult::Declined;
        };
        // Capability gate lives HERE, on the binding path — never inside
        // `registry.run`. An action invoked by name from the command line has
        // no sampled widget and must not be filtered by one.
        let caps = caps_of(&focused);
        if !registry.caps_allow(id, caps) {
            return DockInputResult::Declined;
        }
        let mut cx = ActionCtx::new(Some(focused), Params::new());
        return registry.run(id, &mut cx);
    }

    match action {
        DockKeyAction::Slash => handle_slash(&focused),
        DockKeyAction::Enter => handle_enter(&focused, dock_kind),
        DockKeyAction::Escape => handle_escape_from_dock(),
        // Hjkl already returned above; None falls through.
        DockKeyAction::Hjkl(_) | DockKeyAction::None => DockInputResult::Declined,
    }
}

/// Classify a control into the dock kind whose signal contract it follows.
///
/// Survives the eventual deletion of `FocusContext` because the distinction
/// is real at the Godot API level, not merely a dispatch convenience:
/// `Tree::item_activated` takes **no** parameters while
/// `ItemList::item_activated` takes an index (godot `scene/gui/tree.cpp:7534`
/// vs `scene/gui/item_list.cpp:2486`). Emitting the wrong arity is not a
/// no-op — it is `CALL_ERROR_TOO_MANY_ARGUMENTS`, and the handler does not run.
pub(crate) fn dock_kind_of(control: &Gd<Control>) -> Option<DockKind> {
    let node = control.clone().upcast::<Node>();
    if node.is_class("Tree") {
        Some(DockKind::Tree)
    } else if node.is_class("ItemList") {
        Some(DockKind::ItemList)
    } else if node.is_class("RichTextLabel") {
        Some(DockKind::RichTextLabel)
    } else {
        None
    }
}

/// Capabilities the focused control contributes.
///
/// Uses `is_class` so the inheritance chain is walked — `FileSystemTree` is a
/// `Tree`, `FileSystemList` is an `ItemList`.
fn caps_of(control: &Gd<Control>) -> Caps {
    let node = control.clone().upcast::<Node>();
    Caps::of_control(|class| node.is_class(class))
}

/// Resolve a keystroke inside a dock search box.
///
/// What a keystroke resolves to while a dock's filter `LineEdit` has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchKeyAction {
    /// Return focus to the sibling nav control, preserving the filter text.
    LeaveSearch,
    None,
}

/// One deliberate asymmetry against [`resolve_dock_key`], pinned by tests
/// because a unified dispatcher will be tempted to erase it: **Shift is
/// tolerated.** Only Ctrl/Alt/Meta suppress handling, so `Shift+Enter` and
/// `Shift+Esc` still leave the search box, while in a dock Shift suppresses
/// everything.
///
/// The search box reads the same probe list as every other handler. That
/// costs nothing here: Enter and Escape are named keys, and a named key never
/// receives a positional probe, so probes 2 and 3 can only ever produce
/// characters this table does not bind.
fn search_action_for(key: KeyEvent) -> Option<SearchKeyAction> {
    if key.modifiers().intersects(CMD_MODS) {
        return None;
    }
    match key.key() {
        VimKey::Escape | VimKey::Enter => Some(SearchKeyAction::LeaveSearch),
        _ => None,
    }
}

fn resolve_search_key(probes: &Probes) -> SearchKeyAction {
    probes
        .resolve(search_action_for)
        .unwrap_or(SearchKeyAction::None)
}

/// Only intercepts ESC and Enter from dock search boxes — all other keys
/// pass through for normal typing. Both keys return focus to the sibling
/// nav control (Tree/ItemList), preserving the search filter text.
pub(crate) fn handle_search_input(
    line_edit: &Gd<godot::classes::LineEdit>,
    probes: &Probes,
) -> DockInputResult {
    let action = resolve_search_key(probes);

    match action {
        SearchKeyAction::LeaveSearch => {
            let control: Gd<Control> = line_edit.clone().upcast();
            if let Some(nav) = find_sibling_nav_control(&control) {
                defer_grab_focus(&nav);
                DockInputResult::FocusChanged
            } else {
                // No sibling nav control — fall back to the script editor.
                handle_escape_from_dock()
            }
        }
        SearchKeyAction::None => DockInputResult::Declined,
    }
}

/// `/` — Vim-style "search": focus the dock's filter/search LineEdit.
fn handle_slash(focused: &Gd<Control>) -> DockInputResult {
    if let Some(search_box) = find_sibling_search_box(focused) {
        defer_grab_focus(&search_box);
        let mut node: Gd<Node> = search_box.clone().upcast();
        node.call_deferred("select_all", &[]);
        DockInputResult::FocusChanged
    } else {
        DockInputResult::Declined
    }
}

/// `Enter` — emit activation signals to open the selected item.
///
/// For ItemList, both `item_selected` and `item_activated` are emitted because
/// some Godot editor docks listen to one, some to the other (e.g., the script
/// list dock uses `item_activated` to open scripts).
pub(crate) fn handle_enter(focused: &Gd<Control>, dock_kind: DockKind) -> DockInputResult {
    match dock_kind {
        DockKind::Tree => {
            let mut control = focused.clone();
            control.emit_signal("item_activated", &[]);
            DockInputResult::Handled
        }
        DockKind::ItemList => {
            let Ok(mut list) = focused.clone().try_cast::<godot::classes::ItemList>() else {
                return DockInputResult::Declined;
            };
            let selected = list.get_selected_items();
            if !selected.is_empty() {
                let idx = selected.get(0).unwrap_or(0);
                let mut control = focused.clone();
                control.emit_signal("item_selected", &[Variant::from(idx)]);
                control.emit_signal("item_activated", &[Variant::from(idx)]);
                DockInputResult::Handled
            } else {
                DockInputResult::Declined
            }
        }
        DockKind::RichTextLabel => DockInputResult::Declined,
    }
}

/// Deferred because immediate `grab_focus()` during input processing can be
/// swallowed by Godot's event dispatch loop.
fn defer_grab_focus(target: &Gd<impl Inherits<Node>>) {
    target
        .clone()
        .upcast::<Node>()
        .call_deferred("grab_focus", &[]);
}

/// `ESC` — return focus to the script editor's CodeEdit.
///
/// Tries CodeEdit first (the primary editing surface), then TextEdit (shader
/// editors), then the editor container itself as a last resort.
fn handle_escape_from_dock() -> DockInputResult {
    let interface = EditorInterface::singleton();
    let Some(script_editor) = interface.get_script_editor() else {
        return DockInputResult::Declined;
    };
    let Some(current) = script_editor.get_current_editor() else {
        log::debug!("dock_escape: no current editor found");
        return DockInputResult::Declined;
    };

    let root = current.clone().upcast::<Node>();

    if let Some(code_edit) = find_child_of_type::<CodeEdit>(&root, MAX_DISCOVERY_DEPTH) {
        defer_grab_focus(&code_edit);
        return DockInputResult::FocusChanged;
    }
    if let Some(text_edit) =
        find_child_of_type::<godot::classes::TextEdit>(&root, MAX_DISCOVERY_DEPTH)
    {
        defer_grab_focus(&text_edit);
        return DockInputResult::FocusChanged;
    }

    let control = current.upcast::<Control>();
    defer_grab_focus(&control);
    DockInputResult::FocusChanged
}

// ─── Characterization tests (P0) ─────────────────────────────────────────
//
// Pins CURRENT behaviour of intra-dock key resolution. Must survive the
// dispatcher cutover UNMODIFIED. Tests marked `#[ignore]` are RED: they
// document known defects and are expected to fail until the phase named in
// each one fixes them. Do not delete them to make the suite green.
#[cfg(test)]
mod characterization {
    use super::*;
    use crate::actions::keys::Probes;

    fn ch(c: char) -> KeyEvent {
        KeyEvent::new(VimKey::Char(c), Modifiers::NONE)
    }
    fn named(k: VimKey) -> KeyEvent {
        KeyEvent::new(k, Modifiers::NONE)
    }
    fn probes(keys: &[KeyEvent]) -> Probes {
        Probes::from_slice(keys)
    }

    // ── The dock keyset ──────────────────────────────────────────────

    #[test]
    fn the_shipped_dock_keyset() {
        assert_eq!(
            dock_action_for(ch('j')),
            Some(DockKeyAction::Hjkl(DockHjkl::Down))
        );
        assert_eq!(
            dock_action_for(ch('k')),
            Some(DockKeyAction::Hjkl(DockHjkl::Up))
        );
        assert_eq!(
            dock_action_for(ch('h')),
            Some(DockKeyAction::Hjkl(DockHjkl::Left))
        );
        assert_eq!(
            dock_action_for(ch('l')),
            Some(DockKeyAction::Hjkl(DockHjkl::Right))
        );
        assert_eq!(dock_action_for(ch('/')), Some(DockKeyAction::Slash));
        assert_eq!(
            dock_action_for(named(VimKey::Enter)),
            Some(DockKeyAction::Enter)
        );
        assert_eq!(
            dock_action_for(named(VimKey::Escape)),
            Some(DockKeyAction::Escape)
        );
        assert_eq!(dock_action_for(ch('z')), None);
    }

    #[test]
    fn a_dock_rejects_every_modifier_including_shift() {
        // Ctrl+hjkl is claimed earlier, at input.rs Priority 1; by the time a
        // key reaches the dock resolver ANY modifier means "not ours".
        for m in [
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::META,
            Modifiers::SHIFT,
        ] {
            assert_eq!(dock_action_for(KeyEvent::new(VimKey::Char('j'), m)), None);
            assert_eq!(dock_action_for(KeyEvent::new(VimKey::Enter, m)), None);
        }
    }

    // ── Probe ordering ───────────────────────────────────────────────

    #[test]
    fn latin_layout_resolves_every_bound_key() {
        for (k, want) in [
            (ch('j'), DockKeyAction::Hjkl(DockHjkl::Down)),
            (ch('/'), DockKeyAction::Slash),
            (named(VimKey::Enter), DockKeyAction::Enter),
            (named(VimKey::Escape), DockKeyAction::Escape),
        ] {
            assert_eq!(resolve_dock_key(&probes(&[k])), want);
        }
        assert_eq!(resolve_dock_key(&probes(&[ch('z')])), DockKeyAction::None);
    }

    #[test]
    fn a_later_probe_recovers_hjkl_on_a_non_latin_layout() {
        assert_eq!(
            resolve_dock_key(&probes(&[ch('о'), ch('j')])),
            DockKeyAction::Hjkl(DockHjkl::Down)
        );
    }

    #[test]
    fn a_later_probe_also_recovers_slash() {
        assert_eq!(
            resolve_dock_key(&probes(&[ch('ю'), ch('/')])),
            DockKeyAction::Slash
        );
    }

    #[test]
    fn the_as_typed_probe_beats_a_later_one() {
        // The user typed `j`; a physical position of `/` must not win.
        assert_eq!(
            resolve_dock_key(&probes(&[ch('j'), ch('/')])),
            DockKeyAction::Hjkl(DockHjkl::Down)
        );
    }

    #[test]
    fn a_logical_slash_is_no_longer_shadowed_by_a_physical_hjkl_probe() {
        // Was `known_bug_physical_hjkl_probe_shadows_a_logical_slash`.
        //
        // The old resolver ran the hjkl arm — logical THEN physical — and
        // returned before the arm owning SLASH was reached, so on a layout
        // whose QWERTY-J position emits `/` the dock filter was unreachable.
        // Probes are now tried against the WHOLE keyset in priority order, so
        // the as-typed `/` wins.
        assert_eq!(
            resolve_dock_key(&probes(&[ch('/'), ch('j')])),
            DockKeyAction::Slash
        );
        // Same for the other keys the old shadow swallowed.
        assert_eq!(
            resolve_dock_key(&probes(&[named(VimKey::Escape), ch('j')])),
            DockKeyAction::Escape
        );
        assert_eq!(
            resolve_dock_key(&probes(&[named(VimKey::Enter), ch('l')])),
            DockKeyAction::Enter
        );
    }

    #[test]
    fn numpad_enter_activates_a_dock_item() {
        // Was `known_bug_numpad_enter_is_unbound_in_docks`.
        //
        // `bridge::input::get_named_key` folds KP_ENTER into `Key::Enter`
        // before the probe list is built, so the dock path sees a plain
        // Enter and no longer has to know the numpad exists.
        assert_eq!(
            resolve_dock_key(&probes(&[named(VimKey::Enter)])),
            DockKeyAction::Enter
        );
    }

    // ── The search box ───────────────────────────────────────────────

    #[test]
    fn a_search_box_leaves_on_enter_and_escape() {
        assert_eq!(
            resolve_search_key(&probes(&[named(VimKey::Escape)])),
            SearchKeyAction::LeaveSearch
        );
        assert_eq!(
            resolve_search_key(&probes(&[named(VimKey::Enter)])),
            SearchKeyAction::LeaveSearch
        );
    }

    #[test]
    fn a_search_box_passes_ordinary_typing_through() {
        // Everything else must reach the LineEdit or the user cannot type a
        // filter at all.
        for k in [ch('a'), ch('j'), ch('/'), named(VimKey::Backspace)] {
            assert_eq!(
                resolve_search_key(&probes(&[k])),
                SearchKeyAction::None,
                "{k} should reach the LineEdit"
            );
        }
    }

    #[test]
    fn a_search_box_tolerates_shift_where_a_dock_does_not() {
        // THE asymmetry. Shift+Enter still leaves the search box, while the
        // same chord is inert in a dock. A unified dispatcher will be tempted
        // to collapse these two regimes; this test makes that a decision
        // rather than an accident.
        let shift_enter = KeyEvent::new(VimKey::Enter, Modifiers::SHIFT);
        assert_eq!(
            resolve_search_key(&probes(&[shift_enter])),
            SearchKeyAction::LeaveSearch
        );
        assert_eq!(dock_action_for(shift_enter), None);
    }

    #[test]
    fn a_search_box_declines_ctrl_alt_and_meta() {
        for m in [Modifiers::CTRL, Modifiers::ALT, Modifiers::META] {
            assert_eq!(
                resolve_search_key(&probes(&[KeyEvent::new(VimKey::Escape, m)])),
                SearchKeyAction::None
            );
        }
    }

    // ── The tri-state ────────────────────────────────────────────────

    #[test]
    fn only_declined_is_unconsumed() {
        assert!(DockInputResult::Handled.is_consumed());
        assert!(DockInputResult::FocusChanged.is_consumed());
        assert!(!DockInputResult::Declined.is_consumed());
    }

    // Compile-time proof that `is_consumed` stays `const`-evaluable, which
    // P2's `const fn is_consumed(self)` signature depends on.
    const _: () = assert!(DockInputResult::Handled.is_consumed());
    const _: () = assert!(DockInputResult::FocusChanged.is_consumed());
    const _: () = assert!(!DockInputResult::Declined.is_consumed());
}
