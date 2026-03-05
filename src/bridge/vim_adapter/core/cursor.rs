use godot::classes::CodeEdit;
use godot::prelude::*;
use strum::Display;
use vim_core::domain::position::Position;
use vim_core::state::VimState;

use crate::bridge::vim_adapter::core::column_codec;

#[derive(Display)]
pub enum CursorMoveType {
    /// Pure motion (h, j, k, l, w, b) - no jump entry
    Step,
    /// Explicit jump (gg, G, %, /) - updates jumplist + last_jump_position
    Jump,
    /// Restoration (Ctrl-O, `) - updates last_jump_position only (jumplist internal)
    JumpRestoration,
}
/// Centralized function to move the editor cursor and track state.
///
/// - Updates `VimState` (jumplist, last_jump_position) based on `move_type`.
/// - Moves the `CodeEdit` caret.
/// - Handles unfolding hidden lines.
pub fn move_cursor_with_tracking(
    editor: &mut Gd<CodeEdit>,
    state: &mut VimState,
    target: Position,
    move_type: CursorMoveType,
) {
    let current_pos = column_codec::caret_to_core_position(editor);

    match move_type {
        CursorMoveType::Jump => {
            // Push current position to jumplist unconditionally for Jump-type moves,
            // consistent with Vim's behavior for most jump commands.
            state.history.jumps.push(current_pos);
            state.visual.set_last_jump(current_pos);
        }
        CursorMoveType::JumpRestoration => {
            // Ctrl-O / Ctrl-I / `mark
            // Do not push to the jumplist while traversing or restoring.
            // Update last_jump_position so `` still works.
            state.visual.set_last_jump(current_pos);
        }
        CursorMoveType::Step => {
            // No state update
        }
    }

    column_codec::apply_core_position_to_editor(editor, target);

    state.set_cursor_pos(target);
}
