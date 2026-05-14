//! Auto-brace completion for insert mode.
//!
//! Reimplements Godot's `CodeEdit::_handle_unicode_input_internal` auto-brace
//! logic (code_edit.cpp:770-807) using CodeEdit's bound query APIs. This is
//! necessary because `handle_unicode_input` is not callable from gdext (it's
//! not registered via `ClassDB::bind_method`).
//!
//! The decision tree and helper functions (`find_close_pair_at_pos`,
//! `find_open_pair_at_pos`, `is_symbol`) are direct ports of Godot's C++
//! implementation, using the same ordering and short-circuit logic.

use std::rc::Rc;

use crate::bridge::codec::{i32_to_usize, usize_to_i32, DocumentView};
use crate::bridge::{AutoBraceSnapshot, SyntaxRegion};

/// Pure decision result for auto-brace insert.
///
/// Returned by [`compute_auto_brace_insert`] to describe what the caller
/// should do, without touching the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutoBraceAction {
    /// Insert the character normally (no auto-brace behavior).
    InsertOnly,
    /// Insert the character AND a closing string after it.
    InsertWithClose { close: String },
    /// Skip over an existing closing brace (cursor move only, no text change).
    SkipOver { move_cols: i32 },
}

/// Pure decision function for auto-brace insert.
///
/// Mirrors the decision tree of Godot's `CodeEdit::_handle_unicode_input_internal`
/// but reads line text from `doc.text` (the Cow mirror) and returns an
/// [`AutoBraceAction`] instead of mutating the editor.
pub(crate) fn compute_auto_brace_insert(
    doc: &DocumentView,
    offset: usize,
    ch: char,
    auto_brace: &AutoBraceSnapshot,
    syntax: &SyntaxRegion,
) -> AutoBraceAction {
    debug_assert!(
        !ch.is_control(),
        "compute_auto_brace_insert received control char U+{:04X}",
        ch as u32,
    );
    let lc = doc.line_index.byte_to_line_col(doc.text, offset);
    let line = lc.line;
    let pairs = Rc::clone(&auto_brace.pairs);
    let mut char_buf = [0u8; 4];
    let ch_str = ch.encode_utf8(&mut char_buf);

    let line_text = doc
        .line_index
        .line_text_at(doc.text, i32_to_usize(line));
    let char_col = i32_to_usize(lc.col);
    let line_char_len = line_text.chars().count();

    let post_brace_pair = if char_col < line_char_len {
        find_close_pair_at_pos_str(&pairs, line_text, char_col)
    } else {
        None
    };

    // Branch 1: String delimiter after non-symbol char, no post_brace_pair.
    if auto_brace.has_string_delimiter(ch_str)
        && char_col > 0
        && !is_symbol(nth_char(line_text, char_col - 1).unwrap_or(' '))
        && post_brace_pair.is_none()
    {
        return AutoBraceAction::InsertOnly;
    }

    // Branch 2: Next char is not a symbol → just insert, no auto-close.
    if char_col < line_char_len && !is_symbol(nth_char(line_text, char_col).unwrap_or(' ')) {
        return AutoBraceAction::InsertOnly;
    }

    // Branch 3: Skip-over — close brace at cursor matches the typed char.
    if let Some(pair_idx) = post_brace_pair {
        let close_key = &pairs[pair_idx].1;
        if close_key.starts_with(ch) {
            let move_offset = usize_to_i32(close_key.chars().count());
            return AutoBraceAction::SkipOver {
                move_cols: move_offset,
            };
        }
    }

    // Branch 4: Inside comment, or inside string and char is string delimiter.
    if matches!(syntax, SyntaxRegion::Comment)
        || (matches!(syntax, SyntaxRegion::String) && auto_brace.has_string_delimiter(ch_str))
    {
        return AutoBraceAction::InsertOnly;
    }

    // Branch 5 (default): Check if ch (as a string) matches any open pair key.
    // The imperative version inserts first, then re-reads the line to find the
    // open pair. Since we can't insert, we check the pair table directly: if
    // `ch` (as a single-char string) matches an open pair key, return
    // InsertWithClose with the corresponding close value.
    for (open, close) in pairs.iter() {
        if open == ch_str {
            return AutoBraceAction::InsertWithClose {
                close: close.clone(),
            };
        }
    }

    AutoBraceAction::InsertOnly
}

/// Pure decision function for auto-brace delete.
///
/// Returns the number of extra bytes to delete after the primary deletion
/// (the close brace), or `0` for no auto-brace behavior.
///
/// `start..end` is the byte range of the text that was (or will be) deleted.
/// The function checks `doc.text` (the pre-delete snapshot) to see if the
/// deleted text matches an open pair key with the corresponding close key
/// immediately following.
pub(crate) fn compute_auto_brace_delete_extra(
    doc: &DocumentView,
    start: usize,
    end: usize,
    auto_brace: &AutoBraceSnapshot,
) -> usize {
    let pairs = &auto_brace.pairs;
    if pairs.is_empty() || end > doc.text.len() {
        return 0;
    }

    let deleted = &doc.text[start..end];
    let remainder = &doc.text[end..];

    for (open, close) in pairs.iter() {
        if deleted == open.as_str() && remainder.starts_with(close.as_str()) {
            return close.len();
        }
    }

    0
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Godot's auto-brace "symbol" predicate: ASCII punctuation (excluding `_`)
/// plus whitespace. Non-ASCII punctuation is treated as a word character,
/// matching `code_edit.cpp`'s `char_utils` behavior.
fn is_symbol(ch: char) -> bool {
    (ch.is_ascii_punctuation() && ch != '_') || ch == '\t' || ch == ' '
}

#[inline]
fn nth_char(s: &str, n: usize) -> Option<char> {
    s.chars().nth(n)
}

/// Check if `needle` matches `haystack` starting at char index `col`.
fn chars_match_at(haystack: &str, col: usize, needle: &str) -> bool {
    let mut haystack_iter = haystack.chars().skip(col);
    for expected in needle.chars() {
        match haystack_iter.next() {
            Some(c) if c == expected => {}
            _ => return false,
        }
    }
    true
}

/// Port of `CodeEdit::_get_auto_brace_pair_close_at_pos` (code_edit.cpp:3111-3133).
///
/// String-based version: checks if a close key of any pair starts at char
/// index `col` in `line_text`. Returns the pair index if found.
fn find_close_pair_at_pos_str(
    pairs: &[(String, String)],
    line_text: &str,
    col: usize,
) -> Option<usize> {
    let line_char_len = line_text.chars().count();
    for (i, (_open, close)) in pairs.iter().enumerate() {
        let close_char_len = close.chars().count();
        if col + close_char_len > line_char_len {
            continue;
        }
        if chars_match_at(line_text, col, close) {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_symbol_paren() {
        assert!(is_symbol('('));
        assert!(is_symbol(')'));
        assert!(is_symbol('{'));
        assert!(is_symbol('}'));
        assert!(is_symbol('['));
        assert!(is_symbol(']'));
        assert!(is_symbol('"'));
        assert!(is_symbol('\''));
        assert!(is_symbol(';'));
        assert!(is_symbol(':'));
        assert!(is_symbol(' '));
        assert!(is_symbol('\t'));
    }

    #[test]
    fn is_symbol_non_symbol() {
        assert!(!is_symbol('a'));
        assert!(!is_symbol('Z'));
        assert!(!is_symbol('0'));
        assert!(!is_symbol('_'));
    }

    #[test]
    fn find_close_pair_paren() {
        let pairs = vec![
            ("(".to_string(), ")".to_string()),
            ("{".to_string(), "}".to_string()),
        ];
        assert_eq!(find_close_pair_at_pos_str(&pairs, "foo()", 4), Some(0)); // ')' at col 4
        assert_eq!(find_close_pair_at_pos_str(&pairs, "foo()", 3), None); // '(' at col 3
    }

    #[test]
    fn find_close_pair_multichar() {
        let pairs = vec![("/*".to_string(), "*/".to_string())];
        assert_eq!(find_close_pair_at_pos_str(&pairs, "foo*/bar", 3), Some(0)); // "*/" at col 3
        assert_eq!(find_close_pair_at_pos_str(&pairs, "foo*/bar", 4), None);
    }

    // ── Pure decision function tests ────────────────────────────────────

    use crate::bridge::codec::LineIndex;

    /// Build an `AutoBraceSnapshot` with the given pairs and string delimiters.
    fn snap(pairs: Vec<(&str, &str)>, string_delimiters: Vec<&str>) -> AutoBraceSnapshot {
        AutoBraceSnapshot {
            enabled: true,
            pairs: Rc::new(
                pairs
                    .into_iter()
                    .map(|(o, c)| (o.to_string(), c.to_string()))
                    .collect(),
            ),
            string_delimiters: string_delimiters
                .into_iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }

    /// Build a `DocumentView` from a text string and a freshly computed `LineIndex`.
    fn doc_view<'a>(text: &'a str, index: &'a LineIndex) -> DocumentView<'a> {
        DocumentView::new(text, index)
    }

    #[test]
    fn compute_insert_skip_over() {
        // Typing `)` when `)` is at the cursor position → SkipOver.
        let text = "foo()";
        let idx = LineIndex::new(text);
        let doc = doc_view(text, &idx);
        let ab = snap(vec![("(", ")")], vec![]);
        // offset 4 = byte position of `)`, i.e. cursor is on the `)`.
        let action = compute_auto_brace_insert(&doc, 4, ')', &ab, &SyntaxRegion::Code);
        assert_eq!(action, AutoBraceAction::SkipOver { move_cols: 1 });
    }

    #[test]
    fn compute_insert_with_close() {
        // Typing `(` at the end of line → InsertWithClose.
        let text = "foo";
        let idx = LineIndex::new(text);
        let doc = doc_view(text, &idx);
        let ab = snap(vec![("(", ")")], vec![]);
        let action = compute_auto_brace_insert(&doc, 3, '(', &ab, &SyntaxRegion::Code);
        assert_eq!(
            action,
            AutoBraceAction::InsertWithClose {
                close: ")".to_string(),
            }
        );
    }

    #[test]
    fn compute_insert_only_in_comment() {
        // Typing `(` inside a comment → InsertOnly (no auto-close).
        let text = "// hello";
        let idx = LineIndex::new(text);
        let doc = doc_view(text, &idx);
        let ab = snap(vec![("(", ")")], vec![]);
        let action = compute_auto_brace_insert(&doc, 8, '(', &ab, &SyntaxRegion::Comment);
        assert_eq!(action, AutoBraceAction::InsertOnly);
    }

    #[test]
    fn compute_delete_extra_removes_close() {
        // Deleting `(` which is immediately followed by `)` → extra = 1 byte.
        let text = "foo()bar";
        let idx = LineIndex::new(text);
        let doc = doc_view(text, &idx);
        let ab = snap(vec![("(", ")")], vec![]);
        // Deleting bytes 3..4 which is `(`, close `)` follows at byte 4.
        let extra = compute_auto_brace_delete_extra(&doc, 3, 4, &ab);
        assert_eq!(extra, 1);
    }

    #[test]
    fn compute_delete_extra_no_match() {
        // Deleting `x` which is not an open pair key → extra = 0.
        let text = "fox)bar";
        let idx = LineIndex::new(text);
        let doc = doc_view(text, &idx);
        let ab = snap(vec![("(", ")")], vec![]);
        // Deleting bytes 2..3 which is `x`.
        let extra = compute_auto_brace_delete_extra(&doc, 2, 3, &ab);
        assert_eq!(extra, 0);
    }
}
