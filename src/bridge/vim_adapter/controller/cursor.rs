//! Cursor extraction helpers for `VimController`.

use crate::bridge::vim_adapter::core::column_codec;
use crate::bridge::vim_wrapper::VimController;

use godot::classes::CodeEdit;
use godot::prelude::*;
use vim_core::domain::position::Position;

impl VimController {
    /// Extract logical cursor position from a `CodeEdit`.
    ///
    /// Godot caret positions can point to the exclusive end of a selection.
    /// This helper normalizes to the logical character position expected by core logic.
    #[inline]
    pub(crate) fn cursor_from_editor(editor: &Gd<CodeEdit>) -> Position {
        column_codec::read_selection_core(editor).head
    }
}
