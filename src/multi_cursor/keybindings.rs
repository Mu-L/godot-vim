//! Multi-cursor keybindings using vim-core's `KeyEvent` vocabulary.
//!
//! Detection is a pure function (`classify_multi_cursor_key`) that matches
//! a translated `KeyEvent` against a binding table. Default bindings match
//! VS Code conventions:
//!
//! - Ctrl+D: add next match
//! - Ctrl+Shift+Up/Down: add cursor above/below (matches Godot's native shortcut)
//! - Ctrl+Shift+L: select all occurrences
//! - Alt+Click: add cursor at mouse position (Godot-native, handled by import sync)
//! - Escape (when multi-cursor active): clear secondary cursors
//!
//! The binding table lives in `ControllerContext::multi_cursor_bindings`,
//! enabling future user remapping through `.godot-vimrc`.

use vim_core::keymap::{Key, KeyEvent, Modifiers};

/// Actions triggered by multi-cursor keyboard shortcuts.
///
/// Each variant maps to a specific vim-core `MultiCursorCommand` that the
/// caller will execute after this detection layer returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MultiCursorAction {
    /// Ctrl+D — add next occurrence of word/selection.
    AddNextMatch,
    /// Ctrl+Shift+Up — add a cursor on the line above.
    AddCursorAbove,
    /// Ctrl+Shift+Down — add a cursor on the line below.
    AddCursorBelow,
    /// Ctrl+Shift+L — select all occurrences of word/selection.
    SelectAllOccurrences,
    /// Escape when multi-cursor active — clear all secondary cursors.
    /// Not detected by `classify_multi_cursor_key` — handled by the caller
    /// which checks `cursor_count > 1` before deciding Escape behavior.
    #[allow(dead_code)]
    ClearSecondary,
}

/// Classify a translated `KeyEvent` against a binding table.
///
/// Returns `Some(action)` if the key matches a multi-cursor shortcut.
/// The caller should execute the corresponding action and NOT pass the key
/// through to the vim engine.
///
/// Pure function — testable under `cargo test` without Godot runtime.
///
/// Note: Escape/ClearSecondary is handled by the caller separately (it must
/// check `cursor_count > 1` before deciding whether Escape clears cursors
/// or passes through to the vim engine for its normal behavior).
pub(crate) fn classify_multi_cursor_key(
    key: KeyEvent,
    bindings: &[(KeyEvent, MultiCursorAction)],
) -> Option<MultiCursorAction> {
    bindings
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, a)| *a)
}

/// Default VS Code-style multi-cursor key bindings.
///
/// These match the Godot key translations produced by `bridge::input::parse_godot_key`:
/// - Ctrl+D → `KeyEvent::ctrl('d')` (resolve_ctrl_key normalizes D to lowercase)
/// - Ctrl+Shift+Up → `KeyEvent::new(Key::Up, CTRL | SHIFT)` (named key path)
/// - Ctrl+Shift+Down → `KeyEvent::new(Key::Down, CTRL | SHIFT)` (named key path)
/// - Ctrl+Shift+L → `KeyEvent::new(Key::Char('l'), CTRL | SHIFT)` (resolve_ctrl_key keeps Shift for letters)
pub(crate) fn default_bindings() -> Vec<(KeyEvent, MultiCursorAction)> {
    vec![
        (KeyEvent::ctrl('d'), MultiCursorAction::AddNextMatch),
        (
            KeyEvent::new(Key::Up, Modifiers::CTRL | Modifiers::SHIFT),
            MultiCursorAction::AddCursorAbove,
        ),
        (
            KeyEvent::new(Key::Down, Modifiers::CTRL | Modifiers::SHIFT),
            MultiCursorAction::AddCursorBelow,
        ),
        (
            KeyEvent::new(Key::Char('l'), Modifiers::CTRL | Modifiers::SHIFT),
            MultiCursorAction::SelectAllOccurrences,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ctrl_d_matches_add_next() {
        let bindings = default_bindings();
        let key = KeyEvent::ctrl('d');
        assert_eq!(
            classify_multi_cursor_key(key, &bindings),
            Some(MultiCursorAction::AddNextMatch)
        );
    }

    #[test]
    fn default_ctrl_shift_up_matches_add_above() {
        let bindings = default_bindings();
        let key = KeyEvent::new(Key::Up, Modifiers::CTRL | Modifiers::SHIFT);
        assert_eq!(
            classify_multi_cursor_key(key, &bindings),
            Some(MultiCursorAction::AddCursorAbove)
        );
    }

    #[test]
    fn default_ctrl_shift_down_matches_add_below() {
        let bindings = default_bindings();
        let key = KeyEvent::new(Key::Down, Modifiers::CTRL | Modifiers::SHIFT);
        assert_eq!(
            classify_multi_cursor_key(key, &bindings),
            Some(MultiCursorAction::AddCursorBelow)
        );
    }

    #[test]
    fn default_ctrl_shift_l_matches_select_all() {
        let bindings = default_bindings();
        let key = KeyEvent::new(Key::Char('l'), Modifiers::CTRL | Modifiers::SHIFT);
        assert_eq!(
            classify_multi_cursor_key(key, &bindings),
            Some(MultiCursorAction::SelectAllOccurrences)
        );
    }

    #[test]
    fn non_matching_keys_return_none() {
        let bindings = default_bindings();
        // Plain 'd' without Ctrl
        assert_eq!(
            classify_multi_cursor_key(KeyEvent::char('d'), &bindings),
            None
        );
        // Ctrl+A (not bound)
        assert_eq!(
            classify_multi_cursor_key(KeyEvent::ctrl('a'), &bindings),
            None
        );
        // Escape (handled separately by caller)
        assert_eq!(
            classify_multi_cursor_key(KeyEvent::escape(), &bindings),
            None
        );
        // Ctrl+Up without Shift
        assert_eq!(
            classify_multi_cursor_key(KeyEvent::new(Key::Up, Modifiers::CTRL), &bindings),
            None
        );
        // Shift+Up without Ctrl
        assert_eq!(
            classify_multi_cursor_key(KeyEvent::new(Key::Up, Modifiers::SHIFT), &bindings),
            None
        );
        // Alt+D
        assert_eq!(
            classify_multi_cursor_key(KeyEvent::alt('d'), &bindings),
            None
        );
    }

    #[test]
    fn custom_bindings_work() {
        let bindings = vec![
            (KeyEvent::ctrl('j'), MultiCursorAction::AddCursorBelow),
            (KeyEvent::ctrl('k'), MultiCursorAction::AddCursorAbove),
        ];
        assert_eq!(
            classify_multi_cursor_key(KeyEvent::ctrl('j'), &bindings),
            Some(MultiCursorAction::AddCursorBelow)
        );
        assert_eq!(
            classify_multi_cursor_key(KeyEvent::ctrl('k'), &bindings),
            Some(MultiCursorAction::AddCursorAbove)
        );
        // Default binding not present in custom table
        assert_eq!(
            classify_multi_cursor_key(KeyEvent::ctrl('d'), &bindings),
            None
        );
    }

    #[test]
    fn empty_bindings_return_none() {
        let bindings: Vec<(KeyEvent, MultiCursorAction)> = vec![];
        assert_eq!(
            classify_multi_cursor_key(KeyEvent::ctrl('d'), &bindings),
            None
        );
        assert_eq!(
            classify_multi_cursor_key(KeyEvent::escape(), &bindings),
            None
        );
    }
}
