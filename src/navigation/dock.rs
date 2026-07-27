//! Top-level dock input dispatcher.
//!
//! Routes plain (unmodified) keystrokes to dock navigation handlers based on
//! the focused control's `DockKind`. This gives Vim-style j/k/h/l navigation
//! within Godot's Tree, ItemList, and RichTextLabel dock controls, plus `/`
//! to focus the dock's search box and `ESC` to return to the code editor.
//!
//! Modified keys (Ctrl/Alt/Meta/Shift) always pass through: Ctrl+hjkl is
//! intercepted at a higher priority in `input.rs` for cross-panel navigation.

use godot::classes::{CodeEdit, Control, EditorInterface, InputEventKey, Node};
use godot::global::Key;
use godot::prelude::*;

use super::dock_nav::{handle_hierarchy, handle_navigation, HierarchyAction, NavDirection};
use super::dock_search::{find_sibling_nav_control, find_sibling_search_box};
use super::focus::DockKind;
use crate::scene_tree::{find_child_of_type, MAX_DISCOVERY_DEPTH};

/// Tri-state outcome of a shell-side key handler.
///
/// `FocusChanged` is currently treated identically to `Handled` by every
/// caller — `is_consumed()` is the only method anyone calls. It is kept
/// distinct because moving focus is the case that will need extra
/// bookkeeping once dock keys become rebindable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(crate) enum DockInputResult {
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

impl DockInputResult {
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

/// Resolve a keystroke to a dock action, logical keycode first with a
/// physical fallback for non-Latin layouts.
///
/// **The order here is load-bearing, and it is currently wrong.** The hjkl
/// probe consults BOTH the logical and the physical keycode before the
/// slash/enter/escape match is reached. On a layout where the QWERTY-J
/// position emits a logical `/`, the physical fallback claims the key for
/// `Down` and `/` never opens the dock filter — the exact users the fallback
/// was added to serve. Characterized by an `#[ignore]`d red test below;
/// the fix belongs in the one-global-probe-order work, not here.
fn resolve_dock_key(logical: Key, physical: Key, any_modifier: bool) -> DockKeyAction {
    // Docks reject EVERY modifier, Shift included. The search box does not —
    // see `resolve_search_key`. That asymmetry is real and load-bearing.
    if any_modifier {
        return DockKeyAction::None;
    }
    if let Some(direction) = hjkl_to_dock(logical).or_else(|| hjkl_to_dock(physical)) {
        return DockKeyAction::Hjkl(direction);
    }
    match logical {
        Key::SLASH => DockKeyAction::Slash,
        Key::ENTER => DockKeyAction::Enter,
        Key::ESCAPE => DockKeyAction::Escape,
        _ if physical == Key::SLASH => DockKeyAction::Slash,
        _ => DockKeyAction::None,
    }
}

fn hjkl_to_dock(key: Key) -> Option<DockHjkl> {
    match key {
        Key::J => Some(DockHjkl::Down),
        Key::K => Some(DockHjkl::Up),
        Key::H => Some(DockHjkl::Left),
        Key::L => Some(DockHjkl::Right),
        _ => None,
    }
}

pub(crate) fn handle_dock_input(
    focused: Gd<Control>,
    key_event: &Gd<InputEventKey>,
    dock_kind: DockKind,
) -> DockInputResult {
    log::trace!(
        "dock_input: key={:?} kind={:?}",
        key_event.get_keycode(),
        dock_kind
    );
    // All modified keys pass through. Ctrl+hjkl is already intercepted at
    // Priority 1 in input.rs before this code is reached.
    let any_modifier = key_event.is_ctrl_pressed()
        || key_event.is_alt_pressed()
        || key_event.is_meta_pressed()
        || key_event.is_shift_pressed();

    // hjkl and / use logical-then-physical fallback for non-Latin layout support.
    // Enter and Esc use logical keycode only — they are special keys with
    // layout-independent keycodes.
    let action = resolve_dock_key(
        key_event.get_keycode(),
        key_event.get_physical_keycode(),
        any_modifier,
    );
    if let DockKeyAction::Hjkl(direction) = action {
        return match direction {
            DockHjkl::Down => {
                if handle_navigation(&focused, NavDirection::Next, 0) {
                    DockInputResult::Handled
                } else {
                    DockInputResult::Declined
                }
            }
            DockHjkl::Up => {
                if handle_navigation(&focused, NavDirection::Prev, 0) {
                    DockInputResult::Handled
                } else {
                    DockInputResult::Declined
                }
            }
            DockHjkl::Left => {
                if matches!(dock_kind, DockKind::Tree)
                    && handle_hierarchy(&focused, HierarchyAction::Collapse)
                {
                    DockInputResult::Handled
                } else {
                    DockInputResult::Declined
                }
            }
            DockHjkl::Right => {
                if matches!(dock_kind, DockKind::Tree)
                    && handle_hierarchy(&focused, HierarchyAction::Expand)
                {
                    DockInputResult::Handled
                } else {
                    DockInputResult::Declined
                }
            }
        };
    }

    match action {
        DockKeyAction::Slash => handle_slash(&focused),
        DockKeyAction::Enter => handle_enter(&focused, dock_kind),
        DockKeyAction::Escape => handle_escape_from_dock(),
        // Hjkl already returned above; None falls through.
        DockKeyAction::Hjkl(_) | DockKeyAction::None => DockInputResult::Declined,
    }
}

/// What a keystroke resolves to while a dock's filter `LineEdit` has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchKeyAction {
    /// Return focus to the sibling nav control, preserving the filter text.
    LeaveSearch,
    None,
}

/// Resolve a keystroke inside a dock search box.
///
/// Two deliberate asymmetries against [`resolve_dock_key`], both pinned by
/// tests because a unified dispatcher will be tempted to erase them:
///
/// 1. **Shift is tolerated.** Only Ctrl/Alt/Meta suppress handling, so
///    `Shift+Enter` and `Shift+Esc` still leave the search box. In a dock,
///    Shift suppresses everything. Shift is not even a parameter here — it
///    is never consulted.
/// 2. **No physical fallback.** Only the logical keycode is read. Enter and
///    Escape carry layout-independent keycodes, so the fallback that hjkl
///    and `/` need would buy nothing and could only misfire.
fn resolve_search_key(logical: Key, ctrl_alt_or_meta: bool) -> SearchKeyAction {
    if ctrl_alt_or_meta {
        return SearchKeyAction::None;
    }
    match logical {
        Key::ESCAPE | Key::ENTER => SearchKeyAction::LeaveSearch,
        _ => SearchKeyAction::None,
    }
}

/// Only intercepts ESC and Enter from dock search boxes — all other keys
/// pass through for normal typing. Both keys return focus to the sibling
/// nav control (Tree/ItemList), preserving the search filter text.
pub(crate) fn handle_search_input(
    line_edit: &Gd<godot::classes::LineEdit>,
    key_event: &Gd<InputEventKey>,
) -> DockInputResult {
    let action = resolve_search_key(
        key_event.get_keycode(),
        key_event.is_ctrl_pressed() || key_event.is_alt_pressed() || key_event.is_meta_pressed(),
    );

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
fn handle_enter(focused: &Gd<Control>, dock_kind: DockKind) -> DockInputResult {
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

    // ── The hjkl table ───────────────────────────────────────────────

    #[test]
    fn hjkl_maps_to_dock_directions() {
        assert_eq!(hjkl_to_dock(Key::J), Some(DockHjkl::Down));
        assert_eq!(hjkl_to_dock(Key::K), Some(DockHjkl::Up));
        assert_eq!(hjkl_to_dock(Key::H), Some(DockHjkl::Left));
        assert_eq!(hjkl_to_dock(Key::L), Some(DockHjkl::Right));
        assert_eq!(hjkl_to_dock(Key::A), None);
    }

    // ── Resolution order ─────────────────────────────────────────────

    #[test]
    fn latin_layout_resolves_every_bound_key() {
        let same = |k: Key| resolve_dock_key(k, k, false);
        assert_eq!(same(Key::J), DockKeyAction::Hjkl(DockHjkl::Down));
        assert_eq!(same(Key::K), DockKeyAction::Hjkl(DockHjkl::Up));
        assert_eq!(same(Key::H), DockKeyAction::Hjkl(DockHjkl::Left));
        assert_eq!(same(Key::L), DockKeyAction::Hjkl(DockHjkl::Right));
        assert_eq!(same(Key::SLASH), DockKeyAction::Slash);
        assert_eq!(same(Key::ENTER), DockKeyAction::Enter);
        assert_eq!(same(Key::ESCAPE), DockKeyAction::Escape);
        assert_eq!(same(Key::Z), DockKeyAction::None);
    }

    #[test]
    fn physical_fallback_recovers_hjkl_on_a_cyrillic_layout() {
        // Models a non-Latin layout: the logical keycode is not one we bind,
        // the physical position is J. (Key::O merely stands in for "some
        // unbound logical keycode" — Godot reports a layout-specific value.)
        assert_eq!(
            resolve_dock_key(Key::O, Key::J, false),
            DockKeyAction::Hjkl(DockHjkl::Down)
        );
    }

    #[test]
    fn physical_fallback_also_recovers_slash() {
        // Logical is unbound, physical is SLASH: the trailing guard arm.
        assert_eq!(
            resolve_dock_key(Key::Z, Key::SLASH, false),
            DockKeyAction::Slash
        );
    }

    #[test]
    fn logical_hjkl_beats_a_physical_slash() {
        // Pins precedence: the hjkl probe runs first, so a logical `j` wins
        // even when the physical position is `/`. This one is CORRECT — the
        // user typed `j`.
        assert_eq!(
            resolve_dock_key(Key::J, Key::SLASH, false),
            DockKeyAction::Hjkl(DockHjkl::Down)
        );
    }

    #[test]
    fn enter_and_escape_do_not_take_the_physical_fallback() {
        // Only hjkl and slash consult the physical keycode. Enter/Escape are
        // layout-independent, so a physical ENTER under some other logical
        // key must NOT activate an item.
        assert_eq!(
            resolve_dock_key(Key::Z, Key::ENTER, false),
            DockKeyAction::None
        );
        assert_eq!(
            resolve_dock_key(Key::Z, Key::ESCAPE, false),
            DockKeyAction::None
        );
    }

    // ── Known bugs, pinned at their WRONG value ──────────────────────
    //
    // These assert what the code does TODAY, which is not what it should do.
    // They run by default and are green right now. When the phase named in
    // each one fixes the bug, the assertion FAILS — which is the point: it
    // forces the fix to be acknowledged here rather than landing silently.
    // Flip the expected value (and drop the `known_bug_` prefix) as part of
    // that phase. Do not delete them to keep the suite quiet.
    //
    // `#[ignore]` was deliberately NOT used: an ignored test stays silent
    // both while the bug exists and after it is fixed, so nothing ever
    // forces the revisit.

    #[test]
    fn known_bug_physical_hjkl_probe_shadows_a_logical_slash() {
        // On a layout whose QWERTY-J position emits `/`, the user pressed a
        // key that produces `/` and should get the dock filter. Instead the
        // hjkl probe consults the physical keycode first and moves the
        // selection down, so `/` is unreachable — on exactly the non-Latin
        // layouts the physical fallback exists to serve.
        //
        // WRONG. P1 (one global probe order) must flip this to
        // `DockKeyAction::Slash`.
        assert_eq!(
            resolve_dock_key(Key::SLASH, Key::J, false),
            DockKeyAction::Hjkl(DockHjkl::Down)
        );

        // The shadow is not specific to `/` — any logical key whose physical
        // position is hjkl is swallowed the same way.
        assert_eq!(
            resolve_dock_key(Key::ESCAPE, Key::J, false),
            DockKeyAction::Hjkl(DockHjkl::Down)
        );
        assert_eq!(
            resolve_dock_key(Key::ENTER, Key::L, false),
            DockKeyAction::Hjkl(DockHjkl::Right)
        );
    }

    #[test]
    fn known_bug_numpad_enter_is_unbound_in_docks() {
        // `bridge::input::get_named_key` folds KP_ENTER into Enter for the
        // editor path (`src/bridge/input.rs:23`); the dock path matches the
        // raw keycode and misses it.
        //
        // WRONG. P1 (one key vocabulary) must flip this to
        // `DockKeyAction::Enter`.
        assert_eq!(
            resolve_dock_key(Key::KP_ENTER, Key::KP_ENTER, false),
            DockKeyAction::None
        );
    }

    // ── The tri-state ────────────────────────────────────────────────

    #[test]
    fn only_declined_is_unconsumed() {
        assert!(DockInputResult::Handled.is_consumed());
        assert!(DockInputResult::FocusChanged.is_consumed());
        assert!(!DockInputResult::Declined.is_consumed());
    }

    // Compile-time proof that `is_consumed` stays `const`-evaluable, which
    // P2's `const fn is_consumed(self)` signature depends on. These are
    // assertions the compiler checks: if const-ness regresses, the crate
    // stops building rather than a test going red at runtime.
    const _: () = assert!(DockInputResult::Handled.is_consumed());
    const _: () = assert!(DockInputResult::FocusChanged.is_consumed());
    const _: () = assert!(!DockInputResult::Declined.is_consumed());

    // ── Modifier handling: dock vs. search box ───────────────────────

    #[test]
    fn a_dock_rejects_every_modifier_including_shift() {
        // Ctrl+hjkl is claimed earlier, at input.rs Priority 1; by the time a
        // key reaches the dock resolver ANY modifier means "not ours".
        for key in [
            Key::J,
            Key::K,
            Key::H,
            Key::L,
            Key::SLASH,
            Key::ENTER,
            Key::ESCAPE,
        ] {
            assert_eq!(
                resolve_dock_key(key, key, true),
                DockKeyAction::None,
                "{key:?} with a modifier should not be a dock action"
            );
        }
    }

    #[test]
    fn a_search_box_leaves_on_enter_and_escape() {
        assert_eq!(
            resolve_search_key(Key::ESCAPE, false),
            SearchKeyAction::LeaveSearch
        );
        assert_eq!(
            resolve_search_key(Key::ENTER, false),
            SearchKeyAction::LeaveSearch
        );
    }

    #[test]
    fn a_search_box_passes_ordinary_typing_through() {
        // Everything else must reach the LineEdit or the user cannot type a
        // filter at all.
        for key in [Key::A, Key::J, Key::SLASH, Key::BACKSPACE, Key::SPACE] {
            assert_eq!(
                resolve_search_key(key, false),
                SearchKeyAction::None,
                "{key:?} should reach the LineEdit"
            );
        }
    }

    #[test]
    fn a_search_box_tolerates_shift_where_a_dock_does_not() {
        // THE asymmetry. Shift is not a parameter of resolve_search_key at
        // all, so Shift+Enter and Shift+Esc still leave the search box —
        // while the same chords are inert in a dock. A unified dispatcher
        // will be tempted to collapse these two regimes into one; this test
        // is what makes that a deliberate decision rather than an accident.
        assert_eq!(
            resolve_search_key(Key::ENTER, false),
            SearchKeyAction::LeaveSearch
        );
        assert_eq!(
            resolve_dock_key(Key::ENTER, Key::ENTER, true),
            DockKeyAction::None
        );
    }

    #[test]
    fn a_search_box_declines_ctrl_alt_and_meta() {
        assert_eq!(resolve_search_key(Key::ESCAPE, true), SearchKeyAction::None);
        assert_eq!(resolve_search_key(Key::ENTER, true), SearchKeyAction::None);
    }
}
