//! The dock executors, and the decision tables they replaced.
//!
//! Vim-style j/k/h/l navigation within Godot's Tree, ItemList and
//! RichTextLabel dock controls, plus `/` to focus the dock's search box and
//! `ESC` to return to the code editor. Every one of these is now reached by
//! NAME through an `ActionSpec`, resolved from the `dock` surface's binding
//! trie — the dispatcher no longer knows this file exists.
//!
//! # Why the old resolvers are still here
//!
//! [`resolve_dock_key`] and [`resolve_search_key`] have no production caller
//! any more; the binding index is the live table. They stay because P0's
//! characterization suite is the acceptance gate for the cutover and pins
//! them by name — including the two asymmetries a unified dispatcher is most
//! tempted to erase: a dock rejects every modifier *including Shift*, while
//! the search box tolerates Shift and rejects only Ctrl/Alt/Meta. Deleting
//! them would delete the proof that the new table says the same thing.

use godot::classes::{CodeEdit, Control, EditorInterface, Node};
use godot::prelude::*;
use vim_core::keymap::{Key as VimKey, KeyEvent, Modifiers};

use crate::actions::keys::{Probes, CMD_MODS};

use super::dock_search::{find_sibling_nav_control, find_sibling_search_box};
use crate::scene_tree::{find_child_of_type, MAX_DISCOVERY_DEPTH};

/// Retained so every existing call site compiles unchanged.
///
/// A `type` alias DOES resolve variants (RFC 2338, Rust 1.37+), which is why
/// `DockInputResult::Declined` still works here — but it cannot *rename* one,
/// which is why the `Ignored` → `Declined` rename had to land first.
pub(crate) use crate::actions::outcome::Outcome as DockInputResult;

/// The dock widgets whose *signal contracts* differ.
///
/// Moved here when `focus.rs` was deleted rather than dying with
/// `FocusContext`, because the distinction is real at the Godot API level and
/// not merely a dispatch convenience: `Tree::item_activated` takes **no**
/// parameters while `ItemList::item_activated` takes an index
/// (godot `scene/gui/tree.cpp:7534` vs `scene/gui/item_list.cpp:2486`).
/// Emitting the wrong arity is not a no-op — it is
/// `CALL_ERROR_TOO_MANY_ARGUMENTS`, and the editor's handler does not run.
///
/// It is NOT a classification: which surface a control belongs to is the
/// binding plane's question, answered by `Caps` and the surface forest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockKind {
    Tree,
    ItemList,
    RichTextLabel,
}

/// Direction for hjkl dock navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "the live table is the binding index; this is P0's characterization oracle"
)]
enum DockHjkl {
    Down,
    Up,
    Left,
    Right,
}

/// What a plain (unmodified) keystroke resolves to in a dock, before any
/// widget is touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "the live table is the binding index; this is P0's characterization oracle"
)]
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
#[allow(
    dead_code,
    reason = "the live table is the binding index; this is P0's characterization oracle"
)]
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
#[allow(
    dead_code,
    reason = "the live table is the binding index; this is P0's characterization oracle"
)]
fn resolve_dock_key(probes: &Probes) -> DockKeyAction {
    probes
        .resolve(dock_action_for)
        .unwrap_or(DockKeyAction::None)
}

/// Classify a control into the dock kind whose signal contract it follows.
///
/// The pure half, taking the class probe rather than the control. Split out so
/// that this table and [`crate::actions::caps::Caps::of_control`] — two
/// hand-maintained lists over the same three widget classes — can be held to
/// one agreement property without a Godot runtime. A class in one and not the
/// other manufactures a silent dead key: `Caps` grants `VNAV`, so `:panelmap`
/// reports `godotvim.item.next` as eligible, and then `dock_kind_of` answers
/// `None` and the body declines.
pub(crate) fn dock_kind_for(is_class: impl Fn(&str) -> bool) -> Option<DockKind> {
    if is_class("Tree") {
        Some(DockKind::Tree)
    } else if is_class("ItemList") {
        Some(DockKind::ItemList)
    } else if is_class("RichTextLabel") {
        Some(DockKind::RichTextLabel)
    } else {
        None
    }
}

/// Classify a focused control into the dock kind whose signal contract it
/// follows. `Node::is_class` walks the inheritance chain, which is what makes
/// `FileSystemTree` answer `Tree`.
pub(crate) fn dock_kind_of(control: &Gd<Control>) -> Option<DockKind> {
    let node = control.clone().upcast::<Node>();
    dock_kind_for(|class| node.is_class(class))
}

/// Resolve a keystroke inside a dock search box.
///
/// What a keystroke resolves to while a dock's filter `LineEdit` has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "the live table is the binding index; this is P0's characterization oracle"
)]
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
#[allow(
    dead_code,
    reason = "the live table is the binding index; this is P0's characterization oracle"
)]
fn search_action_for(key: KeyEvent) -> Option<SearchKeyAction> {
    if key.modifiers().intersects(CMD_MODS) {
        return None;
    }
    match key.key() {
        VimKey::Escape | VimKey::Enter => Some(SearchKeyAction::LeaveSearch),
        _ => None,
    }
}

#[allow(
    dead_code,
    reason = "the live table is the binding index; this is P0's characterization oracle"
)]
fn resolve_search_key(probes: &Probes) -> SearchKeyAction {
    probes
        .resolve(search_action_for)
        .unwrap_or(SearchKeyAction::None)
}

/// Leave a dock's filter box, keeping the filter text.
///
/// The executor behind `godotvim.search.accept`, reached from the `searchbox`
/// surface by `<CR>` and `<Esc>` — both of which tolerate Shift, which is why
/// those two rules and only those two carry the `<shift>` flag.
///
/// Escape does **not** clear the filter; it returns focus to the sibling nav
/// control with the text intact, and falls back to the script editor when the
/// dock has no nav control to return to.
pub(crate) fn leave_search(focused: &Gd<Control>) -> DockInputResult {
    if let Some(nav) = find_sibling_nav_control(focused) {
        defer_grab_focus(&nav);
        DockInputResult::FocusChanged
    } else {
        // No sibling nav control — fall back to the script editor.
        handle_escape_from_dock()
    }
}

/// `/` — Vim-style "search": focus the dock's filter/search LineEdit.
pub(crate) fn handle_slash(focused: &Gd<Control>) -> DockInputResult {
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
pub(crate) fn handle_escape_from_dock() -> DockInputResult {
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
