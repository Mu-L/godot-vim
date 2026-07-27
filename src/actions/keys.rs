//! One key vocabulary for the whole shell-side surface.
//!
//! Every shell-side handler used to decode keystrokes for itself, and each
//! did it slightly differently — three ad-hoc "logical keycode, else physical
//! keycode" fallbacks in `navigation::{dock, window, filesystem_explorer}`.
//! Evaluating a fallback *inside a match arm* is what made `/` unreachable in
//! docks on some layouts: the hjkl arm consulted the physical keycode and
//! returned before the arm that owned `Key::SLASH` was ever reached.
//!
//! This module replaces all three with **one ordered probe list evaluated
//! against the whole keyset**. A handler tries probe 1 against every binding
//! it owns, then probe 2 against every binding, and so on — so a lower-priority
//! interpretation can never shadow a higher-priority one.
//!
//! ```text
//!   InputEventKey
//!        │
//!        ├─ parse_godot_key ──── named keys, Ctrl path, unicode path
//!        ├─ LangmapTable::remap_key_event ─── :set langmap
//!        └─ canonicalize ─────── Shift folding
//!        │
//!        ▼
//!   probe 1  as-typed          the normal case                  always
//!   probe 2  latin_key         Cyrillic / Greek                 when latin_key is Some
//!   probe 3  physical position Colemak / Dvorak / AZERTY        when it differs from 1
//! ```
//!
//! Probe 2 fires only when `parse_godot_key` recorded a Latin equivalent,
//! which it does only for non-ASCII output (`bridge::input`). Probe 3 is the
//! US-QWERTY scan-code position, and is deliberately last: it is a guess about
//! what the user *meant*, and must never outrank what they actually typed.

use godot::classes::InputEventKey;
use godot::prelude::*;
use vim_core::keymap::{Key, KeyEvent, LangmapTable, Modifiers, MAX_KEY_SEQUENCE_LEN};

use godot::global::Key as GodotKey;

use crate::bridge::input::{physical_to_ascii, translate_key};

/// Modifiers that mean "this is a command chord", not "this is a character".
///
/// Shared vocabulary: any handler asking "is this the IDE's key, not mine?"
/// must ask it the same way.
pub(crate) const CMD_MODS: Modifiers = Modifiers::CTRL.union(Modifiers::ALT).union(Modifiers::META);

/// An ordered, de-duplicated list of interpretations of one keystroke.
///
/// Fixed capacity so the input hot path never allocates: there are exactly
/// three probes and the list is built once per keystroke.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Probes {
    buf: [Option<KeyEvent>; 3],
    /// Index of the US-QWERTY positional probe, if one was added.
    ///
    /// Tracked because some callers must refuse it. It is a guess about
    /// intent, and in a context that already has a meaningful interpretation
    /// of the key — the attached editor — acting on the guess steals a
    /// binding the user did meant for something else.
    positional: Option<u8>,
}

impl Probes {
    /// Append `key` unless an earlier probe already produced it.
    ///
    /// De-duplication matters: on a US layout all three probes agree, and a
    /// handler that scored "number of probes matched" would triple-count.
    fn push(&mut self, key: KeyEvent, positional: bool) {
        if self.buf.iter().flatten().any(|&k| k == key) {
            return;
        }
        if let Some((i, slot)) = self.buf.iter_mut().enumerate().find(|(_, s)| s.is_none()) {
            *slot = Some(key);
            if positional {
                self.positional = Some(i as u8);
            }
        }
    }

    /// Probes in priority order, highest first.
    pub(crate) fn iter(&self) -> impl Iterator<Item = KeyEvent> + '_ {
        self.iter_scoped(true)
    }

    /// Probes in priority order, admitting the US-QWERTY positional guess
    /// only when `positional` is true.
    ///
    /// The resolver asks per *surface*: probe 3 is offered only where the
    /// binding index holds a `<physical>`-flagged rule, and never at all on a
    /// surface that refuses it. One function so [`Self::iter`] and
    /// [`Self::iter_typed`] cannot drift from the scoped form the walk uses.
    pub(crate) fn iter_scoped(&self, positional: bool) -> impl Iterator<Item = KeyEvent> + '_ {
        let stop = if positional {
            self.buf.len()
        } else {
            self.positional.map_or(self.buf.len(), usize::from)
        };
        self.buf.iter().take(stop).flatten().copied()
    }

    /// Whether any probe carries a command chord (Ctrl/Alt/Meta).
    ///
    /// The `Sealed` discriminator: a bare key stops at the anchor and falls
    /// through to the control's own `gui_input`, while a modifier-bearing key
    /// continues up the forest to `panel`. Shift is deliberately not a
    /// command modifier — it is folded into the character.
    pub(crate) fn has_command_modifier(&self) -> bool {
        self.iter().any(|k| k.modifiers().intersects(CMD_MODS))
    }

    /// Probes excluding the US-QWERTY positional guess.
    ///
    /// Use this wherever the key already has a meaning worth protecting.
    /// Inside the attached editor a Dvorak `Ctrl+d` is half-page-down and a
    /// Colemak `Ctrl+n` is jump-forward; both sit at QWERTY hjkl positions,
    /// so honouring the positional probe there would silently convert core
    /// Vim chords into panel navigation. Non-Latin layouts are unaffected:
    /// `resolve_ctrl_key` already resolves those to a Latin key as probe 1,
    /// and `latin_key` covers the unmodified case as probe 2.
    #[allow(
        dead_code,
        reason = "the resolver scopes probe 3 per surface via `iter_scoped`; this is the \
                  named form the design and the probe-order tests speak in"
    )]
    pub(crate) fn iter_typed(&self) -> impl Iterator<Item = KeyEvent> + '_ {
        self.iter_scoped(false)
    }

    /// The as-typed interpretation, if the event decoded at all.
    #[allow(
        dead_code,
        reason = "the probe-pipeline tests assert against it; dispatch reads the whole list"
    )]
    pub(crate) fn primary(&self) -> Option<KeyEvent> {
        self.buf[0]
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.buf[0].is_none()
    }

    /// Resolve against a per-key table, trying each probe in turn.
    ///
    /// This is the shape every shell-side handler uses: the table covers the
    /// handler's *entire* keyset, so probe 1 is tested against all bindings
    /// before probe 2 is tested against any. That ordering is the whole point
    /// of the module.
    pub(crate) fn resolve<T>(&self, mut table: impl FnMut(KeyEvent) -> Option<T>) -> Option<T> {
        self.iter().find_map(&mut table)
    }

    /// A one-entry probe list holding exactly `key`, canonicalized.
    ///
    /// For the introspector and nothing else. `:panelmap <C-h>` is a written
    /// key, not a keystroke: there is no scan code and no layout to derive a
    /// Latin collapse or a US-QWERTY position from, so synthesizing probes 2
    /// and 3 would be inventing an answer. The report says so rather than
    /// pretending the list is complete.
    pub(crate) fn from_key(key: KeyEvent) -> Self {
        let mut p = Self::default();
        p.push(canonicalize(key), false);
        p
    }

    /// Build a probe list directly. Test-only: production probes always come
    /// from [`probes`] so the pipeline stays the single source of truth.
    #[cfg(test)]
    pub(crate) fn from_slice(keys: &[KeyEvent]) -> Self {
        let mut p = Self::default();
        for &k in keys {
            p.push(k, false);
        }
        p
    }

    /// As [`Self::from_slice`], but marking the LAST entry as the US-QWERTY
    /// positional guess — the shape `probes_from_parts` produces on Dvorak,
    /// Colemak, AZERTY and QWERTZ. Test-only, and the only way to exercise
    /// the surfaces that refuse probe 3.
    #[cfg(test)]
    pub(crate) fn from_slice_positional(keys: &[KeyEvent]) -> Self {
        let mut p = Self::default();
        let last = keys.len().saturating_sub(1);
        for (i, &k) in keys.iter().enumerate() {
            p.push(k, i == last && keys.len() > 1);
        }
        p
    }
}

/// Fold a keystroke into the one spelling a binding can be written as.
///
/// `bridge::input::translate_key` strips SHIFT for printables with no
/// Ctrl/Alt/Meta, so the runtime event for `R` is `Char('R') + NONE`. But
/// `KeyEvent::from_vim_notation("<S-r>")` yields `Char('r') + SHIFT` and
/// `("<S-R>")` yields `Char('R') + SHIFT`. All three must intern identically
/// or the binding is dead on arrival, so both alphabetic cases fold to
/// uppercase-with-SHIFT-cleared.
///
/// Shifted **non-alphabetic** keys are deliberately not folded — there is no
/// canonical form. `<S-1>` is `!` on US and `+` on DE, and `physical_to_ascii`
/// only knows US. Such an LHS is rejected by [`validate_lhs_key`] instead of
/// silently becoming a binding that can never fire.
pub(crate) fn canonicalize(k: KeyEvent) -> KeyEvent {
    match k.key() {
        Key::Char(c)
            if c.is_ascii_alphabetic()
                && k.modifiers().contains(Modifiers::SHIFT)
                && !k.modifiers().intersects(CMD_MODS) =>
        {
            // No latin_key to carry across: `bridge::input` sets it only for
            // non-ASCII output, and this arm requires `is_ascii_alphabetic`.
            KeyEvent::new(
                Key::Char(c.to_ascii_uppercase()),
                k.modifiers() & !Modifiers::SHIFT,
            )
        }
        _ => k,
    }
}

/// Collapse a `latin_key` override into the key itself.
///
/// Mirrors `controller::process::normalize_key_for_mapping`, which does the
/// same job for the editor path. Both planes must agree or a Cyrillic user
/// gets different keys inside and outside the editor.
fn collapse_latin(k: KeyEvent) -> Option<KeyEvent> {
    k.latin_key()
        .map(|latin| canonicalize(KeyEvent::new(latin, k.modifiers())))
}

/// Build the ordered probe list from already-extracted event fields.
///
/// The pure core. Split out for the same reason `bridge::input` splits
/// `translate_key` from `parse_godot_key`: a `Gd<InputEventKey>` cannot be
/// constructed under `cargo test` in a cdylib, so anything that takes one is
/// untestable. Every claim this module makes about probe order, langmap,
/// Shift handling and the Alt/Meta exclusion is verified against this
/// function.
#[allow(clippy::too_many_arguments, reason = "mirrors translate_key's shape")]
pub(crate) fn probes_from_parts(
    keycode: GodotKey,
    physical: GodotKey,
    unicode: u32,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
    langmap: Option<&LangmapTable>,
) -> Probes {
    let mut out = Probes::default();

    // ── Probe 1: as typed ────────────────────────────────────────────
    let parsed = translate_key(keycode, physical, unicode, ctrl, alt, shift, meta);
    if let Some(key) = parsed {
        let key = match langmap {
            Some(table) => table.remap_key_event(key),
            None => key,
        };
        out.push(canonicalize(key), false);
    }

    // Alt and Meta chords belong to the IDE, and AltGr is not a chord at all
    // — it is a level-3 shift that composes characters. Both fallbacks are
    // gated on the RAW state rather than on `parsed.modifiers()`, because
    // `translate_key` deliberately zeroes ctrl/alt for AltGr so the composed
    // character takes the printable path. Reading the post-translation
    // modifiers would see NONE and fire: a Polish `AltGr+a` produces `ą`,
    // whose `latin_key` is `a`, which the FileSystem dock binds to "create
    // file". Guessing there would open a prompt instead of typing a letter.
    let raw_chord = alt || meta;

    // ── Probe 2: the Latin equivalent of a non-Latin key ─────────────
    //
    // Derived from `parsed`, deliberately NOT from the langmap-remapped key:
    // `LangmapTable::remap_key_event` rebuilds via `KeyEvent::new`, which
    // drops `latin_key`. Reading the pre-remap event is what keeps probe 2
    // available when both features are in play.
    if !raw_chord {
        if let Some(latin) = parsed.and_then(collapse_latin) {
            out.push(latin, false);
        }
    }

    // ── Probe 3: the US-QWERTY physical position ─────────────────────
    //
    // Last on purpose. It is a guess about intent — a Dvorak user reaching
    // for where `j` sits on QWERTY — and must never outrank what the user
    // actually typed. Skipped for Alt/Meta chords: those belong to the IDE,
    // and a positional guess there would steal them.
    if let Some(key) = parsed {
        let mods = key.modifiers();
        // Only for keys that produced a character. If probe 1 decoded a NAMED
        // key (Enter, Escape, an arrow, an F-key), a positional character
        // guess is meaningless — and would let a physical position synthesize
        // a binding the user never pressed.
        let is_char = matches!(key.key(), Key::Char(_));
        if is_char && !raw_chord && !mods.intersects(Modifiers::ALT | Modifiers::META) {
            if let Some(ch) = physical_to_ascii(physical, shift) {
                // SHIFT is cleared because the shifted state is already in the
                // character (`physical_to_ascii` uppercases / picks the symbol).
                out.push(
                    canonicalize(KeyEvent::new(Key::Char(ch), mods & !Modifiers::SHIFT)),
                    true,
                );
            }
        }
    }

    out
}

/// Build the ordered probe list for one Godot key event.
///
/// Thin shim over [`probes_from_parts`]; reads each field exactly once.
pub(crate) fn probes(event: &Gd<InputEventKey>, langmap: Option<&LangmapTable>) -> Probes {
    probes_from_parts(
        event.get_keycode(),
        event.get_physical_keycode(),
        event.get_unicode(),
        event.is_ctrl_pressed(),
        event.is_alt_pressed(),
        event.is_shift_pressed(),
        event.is_meta_pressed(),
        langmap,
    )
}

/// Why a left-hand side cannot be used as a binding.
///
/// Consumed by the `panelmap` parser (P5). Defined here because LHS validity
/// is a property of the key vocabulary, not of the config syntax: the same
/// canonicalization that makes a runtime probe must make a parsed binding, or
/// the two planes disagree and the binding is dead on arrival.
#[allow(dead_code, reason = "consumed by the panelmap parser in P5")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LhsError {
    /// A shifted non-alphabetic key has no layout-independent spelling.
    UnspellableShift(char),
    /// The notation did not parse at all.
    Unparseable(String),
    /// Parsed to nothing.
    Empty,
    /// Longer than `vim_core::keymap::MAX_KEY_SEQUENCE_LEN`.
    ///
    /// Rejected rather than truncated: a `KeySequence` is an `ArrayVec` of
    /// that capacity, so a longer LHS would be silently shortened into a
    /// binding the user never wrote — and one that shadows a real prefix.
    TooLong(usize),
}

impl std::fmt::Display for LhsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnspellableShift(c) => write!(
                f,
                "<S-{c}> is layout-dependent and can never match; \
                 write the literal character instead"
            ),
            Self::Unparseable(s) => write!(f, "cannot parse key notation '{s}'"),
            Self::Empty => write!(f, "empty key sequence"),
            Self::TooLong(n) => write!(
                f,
                "key sequence is {n} keys long; at most \
                 {MAX_KEY_SEQUENCE_LEN} are supported"
            ),
        }
    }
}

/// Reject a left-hand side that could never match a runtime keystroke.
///
/// The only such case is Shift over a non-alphabetic key: the runtime never
/// produces `Char('1') + SHIFT` — it produces `!` on US and `+` on DE — so a
/// binding written `<S-1>` is silently dead. Failing at load with a message
/// telling the user to write `!` is strictly better than accepting it.
#[allow(dead_code, reason = "consumed by the panelmap parser in P5")]
pub(crate) fn validate_lhs_key(k: KeyEvent) -> Result<(), LhsError> {
    match k.key() {
        Key::Char(c)
            if !c.is_ascii_alphabetic()
                && k.modifiers().contains(Modifiers::SHIFT)
                && !k.modifiers().intersects(CMD_MODS) =>
        {
            Err(LhsError::UnspellableShift(c))
        }
        _ => Ok(()),
    }
}

/// The nav modes in which an `editor.*` surface is live.
///
/// Verbatim from `src/plugin/input.rs:118-123` — the modes in which the
/// dispatcher intercepts a panel chord from the attached editor. A grammar
/// prefix is only dangerous where the shell is allowed to consume, so these
/// are exactly the modes the guard must ask about.
const NAV_MODES: [vim_core::primitives::Mode; 3] = [
    vim_core::primitives::Mode::Normal,
    vim_core::primitives::Mode::Visual(vim_core::primitives::VisualType::Char),
    vim_core::primitives::Mode::OperatorPending(vim_core::primitives::Operator::Delete),
];

/// Whether `key` puts vim-core's grammar into an `Awaiting*` state.
///
/// This is the `<C-w>` guard, and it asks vim-core's own state machine rather
/// than a hand-written denylist. The reason a cheaper test cannot work:
/// `Keymap::lookup` merges only the *user* mapping tables and never consults
/// `CORE_KEYMAP`, so `could_start_mapping` structurally cannot see `<C-w>`;
/// and `<C-w>` is `KeyClass::Action` in `CORE_KEYMAP`, not `Prefix`, so a
/// class-based test misses it too. `<C-\>` is worse still — the parser
/// intercepts it before classification and it appears in no table at all.
///
/// Consuming such a key at `_input()` destroys the follow-up key, which turns
/// `<C-w>s` into a bare `s`: a destructive edit, silently, from a binding the
/// user thought only moved focus. So a rule carrying one is rejected on any
/// editor-reachable surface.
///
/// Conservative in the safe direction: bare digits answer `true`, which
/// correctly forbids `panelmap panel 3 …` from breaking `3j`.
#[allow(dead_code, reason = "consumed by the binding index in P5")]
pub(crate) fn starts_vim_grammar_sequence(key: KeyEvent) -> bool {
    // Core defaults only. User mappings are `could_start_mapping`'s job at
    // dispatch time, not this one's at registration time.
    let keymap = vim_core::keymap::Keymap::new();
    NAV_MODES.iter().any(|&mode| {
        // `:set sneak` makes `s`/`S` two-key operators, so the answer is
        // genuinely setting-dependent and both must be asked.
        [false, true].into_iter().any(|sneak| {
            let mut parser = vim_core::grammar::Parser::new();
            parser.set_sneak_mode(sneak);
            parser.process(key, &keymap, mode).is_pending()
        })
    })
}

/// Parse a Vim-notation left-hand side into canonicalized key events.
///
/// Uses vim-core's own multi-key parser so the shell plane and the editor
/// plane cannot disagree about what `<Space>ff` or `<C-w>` means.
#[allow(dead_code, reason = "consumed by the panelmap parser in P5")]
pub(crate) fn parse_lhs(notation: &str) -> Result<Vec<KeyEvent>, LhsError> {
    if notation.is_empty() {
        return Err(LhsError::Empty);
    }
    let keys = vim_core::execution::parse_keys_from_string(notation);
    if keys.is_empty() {
        return Err(LhsError::Unparseable(notation.to_string()));
    }
    if keys.len() > MAX_KEY_SEQUENCE_LEN {
        return Err(LhsError::TooLong(keys.len()));
    }
    let keys: Vec<KeyEvent> = keys.into_iter().map(canonicalize).collect();
    for k in &keys {
        validate_lhs_key(*k)?;
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: char) -> KeyEvent {
        KeyEvent::new(Key::Char(c), Modifiers::NONE)
    }
    fn named(k: Key) -> KeyEvent {
        KeyEvent::new(k, Modifiers::NONE)
    }

    // ── canonicalize ─────────────────────────────────────────────────

    #[test]
    fn shifted_lowercase_folds_to_uppercase() {
        // `<S-r>` as parsed from notation.
        let from_notation = KeyEvent::new(Key::Char('r'), Modifiers::SHIFT);
        assert_eq!(canonicalize(from_notation), ch('R'));
    }

    #[test]
    fn shifted_uppercase_also_folds() {
        // `<S-R>`: Modifiers::from_vim_prefix strips the S- and takes the
        // remainder literally, so this is Char('R') + SHIFT. The original
        // fold missed this case and the binding could never fire.
        let from_notation = KeyEvent::new(Key::Char('R'), Modifiers::SHIFT);
        assert_eq!(canonicalize(from_notation), ch('R'));
    }

    #[test]
    fn a_bare_uppercase_char_is_already_canonical() {
        // What the runtime actually delivers for Shift+r.
        assert_eq!(canonicalize(ch('R')), ch('R'));
    }

    #[test]
    fn all_three_spellings_of_shift_r_intern_identically() {
        let runtime = ch('R');
        let s_lower = KeyEvent::new(Key::Char('r'), Modifiers::SHIFT);
        let s_upper = KeyEvent::new(Key::Char('R'), Modifiers::SHIFT);
        assert_eq!(canonicalize(runtime), canonicalize(s_lower));
        assert_eq!(canonicalize(s_lower), canonicalize(s_upper));
    }

    #[test]
    fn ctrl_shift_is_not_folded() {
        // `<C-S-f>` is a distinct chord from `<C-F>`; folding would merge them.
        let k = KeyEvent::new(Key::Char('f'), Modifiers::CTRL | Modifiers::SHIFT);
        assert_eq!(canonicalize(k), k);
    }

    #[test]
    fn shifted_digits_and_symbols_are_not_folded() {
        // There is no canonical form — `<S-1>` is `!` on US, `+` on DE.
        for c in ['1', '/', '-', '='] {
            let k = KeyEvent::new(Key::Char(c), Modifiers::SHIFT);
            assert_eq!(canonicalize(k), k, "{c} should not fold");
        }
    }

    #[test]
    fn named_keys_keep_their_shift() {
        // <S-Tab> is a real, distinct key.
        let k = KeyEvent::new(Key::Tab, Modifiers::SHIFT);
        assert_eq!(canonicalize(k), k);
    }

    #[test]
    fn a_non_ascii_char_is_left_alone_and_keeps_its_latin_override() {
        // The fold arm requires `is_ascii_alphabetic`, so a Cyrillic char
        // falls through untouched — which is what preserves the override that
        // probe 2 later reads.
        let k = KeyEvent::new(Key::Char('ф'), Modifiers::SHIFT).with_latin(Key::Char('a'));
        assert_eq!(canonicalize(k), k);
        assert_eq!(canonicalize(k).latin_key(), Some(Key::Char('a')));
    }

    #[test]
    fn canonicalize_is_idempotent() {
        for k in [
            ch('R'),
            KeyEvent::new(Key::Char('r'), Modifiers::SHIFT),
            KeyEvent::new(Key::Char('1'), Modifiers::SHIFT),
            named(Key::Enter),
        ] {
            assert_eq!(canonicalize(canonicalize(k)), canonicalize(k));
        }
    }

    // ── validate_lhs_key ─────────────────────────────────────────────

    #[test]
    fn a_shifted_symbol_lhs_is_rejected_with_advice() {
        let err = validate_lhs_key(KeyEvent::new(Key::Char('1'), Modifiers::SHIFT)).unwrap_err();
        assert_eq!(err, LhsError::UnspellableShift('1'));
        assert!(err.to_string().contains("write the literal character"));
    }

    #[test]
    fn ordinary_left_hand_sides_are_accepted() {
        for k in [
            ch('a'),
            ch('R'),
            ch('/'),
            named(Key::Enter),
            named(Key::Escape),
            KeyEvent::new(Key::Char('h'), Modifiers::CTRL),
            KeyEvent::new(Key::Tab, Modifiers::SHIFT),
            KeyEvent::new(Key::Char('1'), Modifiers::CTRL | Modifiers::SHIFT),
        ] {
            assert!(validate_lhs_key(k).is_ok(), "{k} should be a valid LHS");
        }
    }

    // ── parse_lhs ────────────────────────────────────────────────────

    #[test]
    fn parse_lhs_handles_single_and_multi_key_notation() {
        assert_eq!(parse_lhs("a").unwrap(), vec![ch('a')]);
        assert_eq!(parse_lhs("dd").unwrap(), vec![ch('d'), ch('d')]);
        assert_eq!(
            parse_lhs("<C-h>").unwrap(),
            vec![KeyEvent::new(Key::Char('h'), Modifiers::CTRL)]
        );
    }

    #[test]
    fn parse_lhs_canonicalizes_what_it_parses() {
        // The two planes must not disagree about how `<S-r>` is spelled.
        assert_eq!(parse_lhs("<S-r>").unwrap(), vec![ch('R')]);
    }

    #[test]
    fn parse_lhs_caps_the_sequence_length() {
        // A `KeySequence` is an ArrayVec of MAX_KEY_SEQUENCE_LEN, so a longer
        // LHS would be silently truncated into a binding nobody wrote — and
        // one that then shadows the real prefix.
        assert_eq!(parse_lhs("abcdefgh").map(|k| k.len()), Ok(8));
        assert_eq!(parse_lhs("abcdefghi"), Err(LhsError::TooLong(9)));
        assert!(parse_lhs("abcdefghi")
            .unwrap_err()
            .to_string()
            .contains("at most 8"));
    }

    #[test]
    fn parse_lhs_rejects_empty_and_unspellable() {
        assert_eq!(parse_lhs(""), Err(LhsError::Empty));
        assert_eq!(
            parse_lhs("<S-1>"),
            Err(LhsError::UnspellableShift('1')),
            "a binding that can never fire must not load"
        );
    }

    // ── starts_vim_grammar_sequence ──────────────────────────────────

    #[test]
    fn the_vim_core_grammar_prefix_canary() {
        // THE version canary. These six answers are what the `<C-w>` guard is
        // built on, and all six come from vim-core's own state machine rather
        // than from anything in this repo. A vim-core bump that changes one of
        // them silently changes which panel bindings are legal — most sharply,
        // `<C-w>` answering `false` would let `panelmap panel <C-w>s` load,
        // and the next `<C-w>s` would delete a word instead of splitting.
        //
        // Written as one table so the failure message names the key that
        // moved, not merely that "a" assertion failed.
        let rows: &[(&str, KeyEvent, bool)] = &[
            // Starts a grammar sequence: consuming it destroys the next key.
            (
                "<C-w>",
                KeyEvent::new(Key::Char('w'), Modifiers::CTRL),
                true,
            ),
            (
                "<C-\\>",
                KeyEvent::new(Key::Char('\\'), Modifiers::CTRL),
                true,
            ),
            // The shipped panel keyset. All four MUST be false, or the plugin
            // cannot ship its own defaults.
            (
                "<C-h>",
                KeyEvent::new(Key::Char('h'), Modifiers::CTRL),
                false,
            ),
            (
                "<C-j>",
                KeyEvent::new(Key::Char('j'), Modifiers::CTRL),
                false,
            ),
            (
                "<C-k>",
                KeyEvent::new(Key::Char('k'), Modifiers::CTRL),
                false,
            ),
            (
                "<C-l>",
                KeyEvent::new(Key::Char('l'), Modifiers::CTRL),
                false,
            ),
        ];
        for (name, key, want) in rows {
            assert_eq!(
                starts_vim_grammar_sequence(*key),
                *want,
                "vim-core changed its answer for {name}"
            );
        }
    }

    #[test]
    fn the_guard_is_reached_through_the_same_notation_users_type() {
        // The canary above builds `KeyEvent`s by hand. This closes the gap
        // between that and the parser the config path actually uses: a
        // `panelmap` line spelling `<C-w>` must reach the same answer.
        let lhs = parse_lhs("<C-w>").expect("valid notation");
        assert!(starts_vim_grammar_sequence(lhs[0]));
        let lhs = parse_lhs("<C-h>").expect("valid notation");
        assert!(!starts_vim_grammar_sequence(lhs[0]));
    }

    #[test]
    fn a_bare_digit_starts_a_count_and_is_therefore_a_prefix() {
        // Conservative in the safe direction, and deliberately so: binding a
        // digit on an editor-reachable surface would break `3j`.
        for c in ['1', '9'] {
            assert!(
                starts_vim_grammar_sequence(ch(c)),
                "{c} begins a count and must be refused"
            );
        }
    }

    // ── Probes ───────────────────────────────────────────────────────

    #[test]
    fn probes_deduplicate() {
        // On a US layout all three probes agree; the list must hold one entry.
        let p = Probes::from_slice(&[ch('j'), ch('j'), ch('j')]);
        assert_eq!(p.iter().count(), 1);
    }

    #[test]
    fn probes_preserve_priority_order() {
        let p = Probes::from_slice(&[ch('/'), ch('j')]);
        assert_eq!(p.iter().collect::<Vec<_>>(), vec![ch('/'), ch('j')]);
        assert_eq!(p.primary(), Some(ch('/')));
    }

    #[test]
    fn probes_saturate_at_three() {
        let p = Probes::from_slice(&[ch('a'), ch('b'), ch('c'), ch('d')]);
        assert_eq!(p.iter().count(), 3);
    }

    #[test]
    fn resolve_tries_the_whole_table_at_each_probe_before_the_next() {
        // THE fix for `/`-shadowing. A table owning both `/` and `j`, given
        // probes [`/`, `j`], must answer Slash — the higher-priority probe
        // wins even though a lower-priority one would also have matched.
        let table = |k: KeyEvent| match k.key() {
            Key::Char('/') => Some("slash"),
            Key::Char('j') => Some("down"),
            _ => None,
        };
        let p = Probes::from_slice(&[ch('/'), ch('j')]);
        assert_eq!(p.resolve(table), Some("slash"));

        // ...and the fallback still works when probe 1 misses entirely.
        let p = Probes::from_slice(&[ch('ц'), ch('j')]);
        assert_eq!(p.resolve(table), Some("down"));
    }

    #[test]
    fn resolve_yields_none_when_no_probe_matches() {
        let p = Probes::from_slice(&[ch('z')]);
        assert_eq!(p.resolve(|_| None::<()>), None);
        assert_eq!(Probes::default().resolve(|_| Some(())), None);
        assert!(Probes::default().is_empty());
    }

    #[test]
    fn collapse_latin_only_fires_when_there_is_an_override() {
        assert_eq!(collapse_latin(ch('j')), None);
        let cyrillic = KeyEvent::new(Key::Char('о'), Modifiers::NONE).with_latin(Key::Char('j'));
        assert_eq!(collapse_latin(cyrillic), Some(ch('j')));
    }

    #[test]
    fn collapse_latin_keeps_modifiers() {
        let k = KeyEvent::new(Key::Char('о'), Modifiers::CTRL).with_latin(Key::Char('j'));
        assert_eq!(
            collapse_latin(k),
            Some(KeyEvent::new(Key::Char('j'), Modifiers::CTRL))
        );
    }

    // ── probes_from_parts: the real construction ─────────────────────
    //
    // Every claim the module makes about probe ORDER, langmap, Shift and the
    // Alt/Meta exclusion is verified here rather than by hand-feeding a list.

    use godot::global::Key as GK;

    /// Decode as if typed on a layout where `keycode`/`unicode` is what the
    /// OS reported and `physical` is the US-QWERTY position of the same key.
    fn typed(keycode: GK, physical: GK, unicode: char, shift: bool) -> Probes {
        probes_from_parts(
            keycode,
            physical,
            unicode as u32,
            false,
            false,
            shift,
            false,
            None,
        )
    }
    fn typed_ctrl(keycode: GK, physical: GK) -> Probes {
        probes_from_parts(keycode, physical, 0, true, false, false, false, None)
    }

    #[test]
    fn on_a_us_layout_every_probe_agrees_and_collapses_to_one() {
        let p = typed(GK::J, GK::J, 'j', false);
        assert_eq!(p.iter().collect::<Vec<_>>(), vec![ch('j')]);
    }

    #[test]
    fn a_cyrillic_key_yields_as_typed_then_its_latin_equivalent() {
        // Cyrillic о sits at the QWERTY-J position. Probe 1 is the char the
        // user produced; probe 2 is the latin_key `bridge::input` recorded;
        // probe 3 is the same position again and de-duplicates away.
        let p = typed(GK::NONE, GK::J, 'о', false);
        let got: Vec<_> = p.iter().collect();
        assert_eq!(got[0].key(), Key::Char('о'), "probe 1 is what was typed");
        assert!(got.contains(&ch('j')), "a later probe must recover `j`");
        assert_eq!(p.primary(), Some(got[0]));
    }

    #[test]
    fn a_qwertz_z_yields_z_first_and_the_physical_y_second() {
        // THE ordering property. `z` is what the user typed and must lead;
        // the QWERTY-Y position is only a fallback guess.
        let p = typed(GK::Z, GK::Y, 'z', false);
        assert_eq!(p.iter().collect::<Vec<_>>(), vec![ch('z'), ch('y')]);
    }

    #[test]
    fn a_logical_slash_over_a_physical_j_leads_with_the_slash() {
        // The `/`-shadowing bug, at the level that actually produces it.
        let p = typed(GK::SLASH, GK::J, '/', false);
        assert_eq!(p.iter().collect::<Vec<_>>(), vec![ch('/'), ch('j')]);
        assert_eq!(p.primary(), Some(ch('/')));
    }

    #[test]
    fn numpad_enter_decodes_as_a_plain_enter() {
        // `get_named_key` folds KP_ENTER, so no handler has to know the
        // numpad exists. Named keys get no physical probe: `physical_to_ascii`
        // returns None for them.
        let p = probes_from_parts(
            GK::KP_ENTER,
            GK::KP_ENTER,
            0,
            false,
            false,
            false,
            false,
            None,
        );
        assert_eq!(p.iter().collect::<Vec<_>>(), vec![named(Key::Enter)]);
    }

    #[test]
    fn enter_and_escape_never_gain_a_physical_probe() {
        // Guards the behaviour the old `enter_and_escape_do_not_take_the
        // _physical_fallback` test pinned: a physical position must never
        // synthesize a named key.
        for (kc, want) in [(GK::ENTER, Key::Enter), (GK::ESCAPE, Key::Escape)] {
            let p = probes_from_parts(kc, GK::J, 0, false, false, false, false, None);
            assert_eq!(
                p.iter().collect::<Vec<_>>(),
                vec![named(want)],
                "{want:?} must not gain a physical probe"
            );
        }
    }

    #[test]
    fn shift_is_folded_into_the_character_on_every_probe() {
        // Shift+J on QWERTY: both probes must be `J`, never `j`, or a dock
        // would treat a shifted key as its unshifted binding.
        let p = typed(GK::J, GK::J, 'J', true);
        for k in p.iter() {
            assert_eq!(k, ch('J'), "probe must be uppercase with SHIFT cleared");
        }
    }

    #[test]
    fn a_shifted_digit_uses_the_us_symbol_for_its_physical_probe() {
        // Shift+1 on a DE layout produces `!` on US. Probe 1 is what was
        // typed; probe 3 is the US symbol. Neither carries a SHIFT bit.
        let p = typed(GK::KEY_1, GK::KEY_1, '"', true);
        let got: Vec<_> = p.iter().collect();
        assert_eq!(got[0], ch('"'));
        assert!(got.contains(&ch('!')), "US symbol should be the fallback");
        assert!(got
            .iter()
            .all(|k| !k.modifiers().contains(Modifiers::SHIFT)));
    }

    #[test]
    fn ctrl_survives_into_the_physical_probe() {
        // This is what gives a Cyrillic user Ctrl+hjkl from inside the editor,
        // which the old logical-only check denied them.
        let p = typed_ctrl(GK::NONE, GK::J);
        assert!(
            p.iter()
                .any(|k| k == KeyEvent::new(Key::Char('j'), Modifiers::CTRL)),
            "got {:?}",
            p.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn alt_and_meta_chords_gain_no_extra_positional_probe() {
        // Alt/Meta chords belong to the IDE, so probe 3 is skipped for them.
        // Note `translate_key` has its own zero-unicode recovery that already
        // derives a character from the physical position, so probe 1 may
        // still be `j`; what must not happen is a SECOND, positional probe
        // widening what the chord can match.
        for (alt, meta) in [(true, false), (false, true)] {
            let p = probes_from_parts(GK::NONE, GK::J, 0, false, alt, false, meta, None);
            assert_eq!(
                p.iter().count(),
                1,
                "Alt/Meta must not add a positional probe: {:?}",
                p.iter().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn an_undecodable_event_yields_no_probes() {
        // Bare modifier keys and media keys: nothing to resolve, and the
        // dispatcher returns early rather than guessing from position.
        let p = probes_from_parts(GK::NONE, GK::NONE, 0, false, false, false, false, None);
        assert!(p.is_empty());
        assert_eq!(p.primary(), None);
    }

    #[test]
    fn langmap_rewrites_the_as_typed_probe() {
        // `:set langmap=jk,kj` swaps j and k. Probe 1 must be the remapped
        // key; the physical position is untouched and still trails.
        let table = LangmapTable::parse("jk,kj").expect("valid langmap");
        let p = probes_from_parts(
            GK::J,
            GK::J,
            'j' as u32,
            false,
            false,
            false,
            false,
            Some(&table),
        );
        assert_eq!(p.primary(), Some(ch('k')), "langmap must apply to probe 1");
    }

    #[test]
    fn no_langmap_leaves_the_key_alone() {
        let p = typed(GK::J, GK::J, 'j', false);
        assert_eq!(p.primary(), Some(ch('j')));
    }

    // ── The layout matrix ────────────────────────────────────────────

    #[test]
    fn the_shipped_keyset_is_reachable_on_every_supported_layout() {
        // For each layout, `dock` navigation must still resolve `j`, and the
        // key the user typed must always lead. Rows are
        // (name, logical keycode, unicode, physical position).
        let rows: &[(&str, GK, char, GK)] = &[
            ("QWERTY j", GK::J, 'j', GK::J),
            ("QWERTZ j", GK::J, 'j', GK::J),
            ("AZERTY j", GK::J, 'j', GK::J),
            // Dvorak: the QWERTY-J position emits `h`.
            ("Dvorak  h@J", GK::H, 'h', GK::J),
            // Colemak: the QWERTY-J position emits `y`.
            ("Colemak y@J", GK::Y, 'y', GK::J),
            // Cyrillic: no usable logical keycode, latin_key carries it.
            ("Cyrillic о@J", GK::NONE, 'о', GK::J),
        ];
        for (name, kc, uni, phys) in rows {
            let p = typed(*kc, *phys, *uni, false);
            assert!(!p.is_empty(), "{name}: decoded to nothing");
            // What the user typed always leads.
            assert_eq!(
                p.primary().map(|k| k.key()),
                Some(Key::Char(*uni)),
                "{name}: probe 1 must be the typed character"
            );
            // ...and `j` stays reachable via the positional probe.
            assert!(
                p.iter().any(|k| k.key() == Key::Char('j')),
                "{name}: lost access to the QWERTY-J binding"
            );
        }
    }

    // ── Properties ───────────────────────────────────────────────────

    proptest::proptest! {
        /// Canonicalization must be a fixpoint: applying it to a parsed LHS
        /// and to a runtime probe has to land on the same value, or a binding
        /// is dead on arrival.
        #[test]
        fn canonicalize_is_idempotent_over_ascii(
            c in proptest::char::range('!', '~'),
            shift in proptest::bool::ANY,
            ctrl in proptest::bool::ANY,
        ) {
            let mut mods = Modifiers::NONE;
            if shift { mods |= Modifiers::SHIFT; }
            if ctrl { mods |= Modifiers::CTRL; }
            let once = canonicalize(KeyEvent::new(Key::Char(c), mods));
            proptest::prop_assert_eq!(canonicalize(once), once);
        }

        /// A probe list never exceeds its capacity and never repeats itself —
        /// a duplicate would make a handler score the same key twice.
        #[test]
        fn probes_are_bounded_and_unique(
            kc in 0u32..0x80,
            phys in 0u32..0x80,
            shift in proptest::bool::ANY,
        ) {
            let p = probes_from_parts(
                GK::from_ord(kc as i32), GK::from_ord(phys as i32),
                kc, false, false, shift, false, None,
            );
            let got: Vec<_> = p.iter().collect();
            proptest::prop_assert!(got.len() <= 3);
            for (i, a) in got.iter().enumerate() {
                for b in &got[i + 1..] {
                    proptest::prop_assert_ne!(a, b, "duplicate probe");
                }
            }
        }
    }

    // ── Regressions caught in review ─────────────────────────────────

    #[test]
    fn a_positional_probe_is_marked_and_can_be_refused() {
        // QWERTZ `z` at the QWERTY-Y position: `y` is a guess, `z` is not.
        let p = typed(GK::Z, GK::Y, 'z', false);
        assert_eq!(p.iter().collect::<Vec<_>>(), vec![ch('z'), ch('y')]);
        assert_eq!(
            p.iter_typed().collect::<Vec<_>>(),
            vec![ch('z')],
            "iter_typed must drop the positional guess"
        );
    }

    #[test]
    fn iter_typed_equals_iter_when_no_probe_was_positional() {
        let p = probes_from_parts(GK::ENTER, GK::ENTER, 0, false, false, false, false, None);
        assert_eq!(
            p.iter().collect::<Vec<_>>(),
            p.iter_typed().collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_dvorak_ctrl_chord_keeps_its_typed_meaning() {
        // REGRESSION GUARD. On Dvorak the QWERTY-H position emits `d`, so
        // Ctrl+d — half-page-down, a core Vim binding — sits where panel-left
        // lives on QWERTY. The positional probe still offers `h` for contexts
        // that want it, but `iter_typed` (which the editor uses) must not.
        let p = typed_ctrl(GK::D, GK::H);
        let typed_only: Vec<_> = p.iter_typed().collect();
        assert!(
            !typed_only.iter().any(|k| k.key() == Key::Char('h')),
            "editor must not see a positional `h`: {typed_only:?}"
        );
        assert!(
            p.iter().any(|k| k.key() == Key::Char('h')),
            "a dock should still get the positional fallback"
        );
    }

    #[test]
    fn a_colemak_ctrl_chord_keeps_its_typed_meaning() {
        // Colemak puts `n` at the QWERTY-J position: Ctrl+n is jump-forward.
        let p = typed_ctrl(GK::N, GK::J);
        assert!(!p.iter_typed().any(|k| k.key() == Key::Char('j')));
    }

    #[test]
    fn altgr_composed_characters_get_no_fallback_probes() {
        // REGRESSION GUARD. `translate_key` zeroes ctrl/alt for AltGr so the
        // composed character takes the printable path — which means the
        // post-translation modifiers are NONE and cannot be used to detect it.
        //
        // Polish AltGr+a produces `ą`, whose latin_key is `a`. Without the raw
        // gate that becomes probe 2, and `a` is bound to "create file" in the
        // FileSystem dock: typing a letter would open a prompt.
        //
        // Linux AltGr: alt only.
        let p = probes_from_parts(GK::A, GK::A, 'ą' as u32, false, true, false, false, None);
        assert!(
            !p.iter().any(|k| k.key() == Key::Char('a')),
            "AltGr must not surface a Latin fallback: {:?}",
            p.iter().collect::<Vec<_>>()
        );
        // Windows AltGr: ctrl+alt together.
        let p = probes_from_parts(GK::A, GK::A, 'ą' as u32, true, true, false, false, None);
        assert!(!p.iter().any(|k| k.key() == Key::Char('a')));
    }

    #[test]
    fn a_genuine_alt_chord_still_gets_no_fallback_probes() {
        let p = probes_from_parts(GK::J, GK::J, 0, false, true, false, false, None);
        assert!(p.iter().count() <= 1);
    }

    #[test]
    fn an_unmodified_non_latin_key_still_gets_its_latin_fallback() {
        // The raw alt/meta gate must not cost the case probe 2 exists for.
        let p = typed(GK::NONE, GK::J, 'о', false);
        assert!(p.iter().any(|k| k.key() == Key::Char('j')));
    }
}
