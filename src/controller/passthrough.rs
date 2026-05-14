//! Key passthrough chain: a chain-of-responsibility pattern that decides
//! whether a key bypasses the Vim engine and flows through to Godot's native
//! input handling.
//!
//! Four filters compose the decision, evaluated in priority order:
//!
//! 1. **MappingPriority** — pending/startable mappings always claim the key for
//!    the engine. Terminal `SendToEngine`.
//! 2. **UserOverride** — explicit passthrough keys from EditorSettings. Terminal
//!    `Passthrough`.
//! 3. **HostPolicy** — F-keys and Meta combos always pass through (IDE/OS
//!    shortcuts). **Alt is NOT included** — Alt keys reach `EngineQuery` so the
//!    engine can claim them if it has a binding.
//! 4. **EngineQuery** — `would_handle_key()` is the final arbiter. Always
//!    terminal.
//!
//! Adding a new filter: one enum variant + one match arm + one array entry.
//! Exhaustive `match` catches missing arms at compile time.

use std::collections::HashSet;

use vim_core::execution::VimEngine;
use vim_core::keymap::{Key, KeyEvent, Modifiers};

// ═══════════════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════════════

/// Terminal or non-terminal verdict from a single filter in the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterVerdict {
    /// Terminal: key goes to Godot.
    Passthrough,
    /// Terminal: key goes to the Vim engine.
    SendToEngine,
    /// Non-terminal: this filter has no opinion; continue to the next.
    Undecided,
}

/// The four filters in the passthrough chain, evaluated in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassthroughFilter {
    /// Pending/startable mappings claim the key for the engine.
    MappingPriority,
    /// User-configured passthrough keys from EditorSettings.
    UserOverride,
    /// F-keys + Meta combos pass through unconditionally (NOT Alt).
    HostPolicy,
    /// Engine's `would_handle_key()` — always produces a terminal verdict.
    EngineQuery,
}

/// Const-ordered filter chain. The runner iterates this array in order.
pub(crate) const FILTER_CHAIN: [PassthroughFilter; 4] = [
    PassthroughFilter::MappingPriority,
    PassthroughFilter::UserOverride,
    PassthroughFilter::HostPolicy,
    PassthroughFilter::EngineQuery,
];

/// Everything a filter needs to make its decision. Zero allocation — all
/// references are borrowed from VimController.
pub(crate) struct FilterContext<'a> {
    /// The raw key event from Godot.
    pub key: KeyEvent,
    /// Latin-normalized key for non-Latin layouts (e.g. Cyrillic `о` → `j`).
    pub normalized_key: KeyEvent,
    /// Read-only access to the Vim engine for mapping/command queries.
    pub engine: &'a VimEngine,
    /// User-configured passthrough key set from EditorSettings.
    pub user_overrides: &'a HashSet<KeyEvent>,
}

// ═══════════════════════════════════════════════════════════════════════════════
// Dispatch
// ═══════════════════════════════════════════════════════════════════════════════

impl PassthroughFilter {
    /// Evaluate this filter against the given context.
    fn evaluate(self, ctx: &FilterContext<'_>) -> FilterVerdict {
        match self {
            Self::MappingPriority => evaluate_mapping_priority(ctx),
            Self::UserOverride => evaluate_user_override(ctx),
            Self::HostPolicy => evaluate_host_policy(ctx),
            Self::EngineQuery => evaluate_engine_query(ctx),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Chain runner
// ═══════════════════════════════════════════════════════════════════════════════

/// Run the passthrough chain. Returns `true` if the key should pass through
/// to Godot, `false` if it should go to the Vim engine.
pub(crate) fn run_passthrough_chain(ctx: &FilterContext<'_>) -> bool {
    for filter in &FILTER_CHAIN {
        match filter.evaluate(ctx) {
            FilterVerdict::Passthrough => return true,
            FilterVerdict::SendToEngine => return false,
            FilterVerdict::Undecided => continue,
        }
    }
    // Conservative default: if no filter claimed the key, pass through.
    true
}

// ═══════════════════════════════════════════════════════════════════════════════
// Filter evaluators
// ═══════════════════════════════════════════════════════════════════════════════

/// Mappings always take priority — never passthrough mid-sequence or when a
/// mapping could start from the normalized key.
fn evaluate_mapping_priority(ctx: &FilterContext<'_>) -> FilterVerdict {
    if ctx.engine.has_pending_mapping() || ctx.engine.could_start_mapping(ctx.normalized_key) {
        FilterVerdict::SendToEngine
    } else {
        FilterVerdict::Undecided
    }
}

/// User overrides: explicit passthrough keys from EditorSettings. Checks both
/// the raw key and the Latin-normalized key so that a passthrough entry for `j`
/// works on both Latin and non-Latin layouts.
fn evaluate_user_override(ctx: &FilterContext<'_>) -> FilterVerdict {
    if ctx.user_overrides.contains(&ctx.key) || ctx.user_overrides.contains(&ctx.normalized_key) {
        FilterVerdict::Passthrough
    } else {
        FilterVerdict::Undecided
    }
}

/// Host policy: F-keys and Meta combos always pass through to the IDE/OS.
///
/// **Alt is NOT included.** Alt keys reach `EngineQuery` so the engine can
/// claim them if it has a binding (`<M-j>` via user mapping or built-in).
/// If the engine doesn't handle the Alt key, it passes through naturally via
/// the `EngineQuery` filter's fallback.
fn evaluate_host_policy(ctx: &FilterContext<'_>) -> FilterVerdict {
    if matches!(ctx.key.key(), Key::F(_)) {
        return FilterVerdict::Passthrough;
    }

    if ctx.key.modifiers().contains(Modifiers::META) {
        return FilterVerdict::Passthrough;
    }

    FilterVerdict::Undecided
}

/// Final arbiter: does the engine's built-in command set handle this key?
/// Always produces a terminal verdict.
fn evaluate_engine_query(ctx: &FilterContext<'_>) -> FilterVerdict {
    if ctx.engine.would_handle_key(ctx.key) {
        FilterVerdict::SendToEngine
    } else {
        FilterVerdict::Passthrough
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ─────────────────────────────────────────────────────────

    /// Build a minimal FilterContext for unit tests.
    ///
    /// Uses a default VimEngine and empty user overrides.
    /// Both engine and overrides are leaked for `'static` lifetime — tests
    /// are short-lived so this is fine.
    fn test_ctx(key: KeyEvent) -> FilterContext<'static> {
        let engine: &'static VimEngine = Box::leak(Box::new(VimEngine::new()));
        let overrides: &'static HashSet<KeyEvent> = Box::leak(Box::new(HashSet::new()));
        FilterContext {
            key,
            normalized_key: key,
            engine,
            user_overrides: overrides,
        }
    }

    /// Build a FilterContext with a specific engine and overrides set.
    fn make_ctx<'a>(
        key: KeyEvent,
        engine: &'a VimEngine,
        overrides: &'a HashSet<KeyEvent>,
    ) -> FilterContext<'a> {
        FilterContext {
            key,
            normalized_key: key,
            engine,
            user_overrides: overrides,
        }
    }

    // ── evaluate_mapping_priority ───────────────────────────────────────

    #[test]
    fn mapping_priority_no_mapping_is_undecided() {
        let ctx = test_ctx(KeyEvent::char('j'));
        assert_eq!(evaluate_mapping_priority(&ctx), FilterVerdict::Undecided);
    }

    // ── evaluate_user_override ─────────────────────────────────────────

    #[test]
    fn user_override_matching_key_passes_through() {
        let mut overrides = HashSet::new();
        overrides.insert(KeyEvent::ctrl('s'));
        let engine = VimEngine::new();
        let ctx = make_ctx(KeyEvent::ctrl('s'), &engine, &overrides);
        assert_eq!(evaluate_user_override(&ctx), FilterVerdict::Passthrough);
    }

    #[test]
    fn user_override_no_match_is_undecided() {
        let overrides = HashSet::new();
        let engine = VimEngine::new();
        let ctx = make_ctx(KeyEvent::ctrl('s'), &engine, &overrides);
        assert_eq!(evaluate_user_override(&ctx), FilterVerdict::Undecided);
    }

    #[test]
    fn user_override_matches_normalized_key() {
        let mut overrides = HashSet::new();
        overrides.insert(KeyEvent::char('j'));
        // Simulate a non-Latin key with latin_key override.
        let raw_key = KeyEvent::new(Key::Char('\u{043E}'), Modifiers::NONE);
        let normalized = KeyEvent::char('j');
        let engine = VimEngine::new();
        let ctx = FilterContext {
            key: raw_key,
            normalized_key: normalized,
            engine: &engine,
            user_overrides: &overrides,
        };
        assert_eq!(evaluate_user_override(&ctx), FilterVerdict::Passthrough);
    }

    // ── evaluate_host_policy ───────────────────────────────────────────

    #[test]
    fn host_policy_f_keys_passthrough() {
        for n in 1..=12 {
            let key = KeyEvent::new(Key::F(n), Modifiers::NONE);
            let ctx = test_ctx(key);
            assert_eq!(
                evaluate_host_policy(&ctx),
                FilterVerdict::Passthrough,
                "F{n} should passthrough"
            );
        }
    }

    #[test]
    fn host_policy_f_keys_with_modifiers_passthrough() {
        let key = KeyEvent::new(Key::F(5), Modifiers::SHIFT);
        let ctx = test_ctx(key);
        assert_eq!(evaluate_host_policy(&ctx), FilterVerdict::Passthrough);

        let key = KeyEvent::new(Key::F(1), Modifiers::CTRL);
        let ctx = test_ctx(key);
        assert_eq!(evaluate_host_policy(&ctx), FilterVerdict::Passthrough);
    }

    #[test]
    fn host_policy_meta_combos_passthrough() {
        let key = KeyEvent::new(Key::Char('s'), Modifiers::META);
        let ctx = test_ctx(key);
        assert_eq!(evaluate_host_policy(&ctx), FilterVerdict::Passthrough);
    }

    #[test]
    fn host_policy_meta_keys_passthrough() {
        for c in 'a'..='z' {
            let key = KeyEvent::new(Key::Char(c), Modifiers::META);
            let ctx = test_ctx(key);
            assert_eq!(
                evaluate_host_policy(&ctx),
                FilterVerdict::Passthrough,
                "Meta+{c} should pass through"
            );
        }
    }

    #[test]
    fn host_policy_alt_does_not_passthrough() {
        // Alt keys must NOT be caught by HostPolicy — they reach EngineQuery.
        let key = KeyEvent::alt('x');
        let ctx = test_ctx(key);
        assert_eq!(evaluate_host_policy(&ctx), FilterVerdict::Undecided);
    }

    #[test]
    fn host_policy_alt_s_does_not_passthrough() {
        let key = KeyEvent::alt('s');
        let ctx = test_ctx(key);
        assert_eq!(evaluate_host_policy(&ctx), FilterVerdict::Undecided);
    }

    #[test]
    fn host_policy_plain_key_is_undecided() {
        let ctx = test_ctx(KeyEvent::char('j'));
        assert_eq!(evaluate_host_policy(&ctx), FilterVerdict::Undecided);
    }

    #[test]
    fn host_policy_ctrl_key_is_undecided() {
        let ctx = test_ctx(KeyEvent::ctrl('s'));
        assert_eq!(evaluate_host_policy(&ctx), FilterVerdict::Undecided);
    }

    // ── evaluate_engine_query ──────────────────────────────────────────

    #[test]
    fn engine_query_handled_key_sends_to_engine() {
        // 'j' is a built-in Normal mode motion — the engine handles it.
        let engine = VimEngine::new();
        let overrides = HashSet::new();
        let ctx = make_ctx(KeyEvent::char('j'), &engine, &overrides);
        assert_eq!(evaluate_engine_query(&ctx), FilterVerdict::SendToEngine);
    }

    #[test]
    fn engine_query_unhandled_key_passes_through() {
        // F20 is not a vim command — the engine won't handle it.
        let engine = VimEngine::new();
        let overrides = HashSet::new();
        let key = KeyEvent::new(Key::F(20), Modifiers::NONE);
        let ctx = make_ctx(key, &engine, &overrides);
        assert_eq!(evaluate_engine_query(&ctx), FilterVerdict::Passthrough);
    }

    // ── Chain integration ──────────────────────────────────────────────

    #[test]
    fn chain_f_key_stops_at_host_policy() {
        let key = KeyEvent::new(Key::F(5), Modifiers::NONE);
        let engine = VimEngine::new();
        let overrides = HashSet::new();
        let ctx = make_ctx(key, &engine, &overrides);
        assert!(run_passthrough_chain(&ctx), "F5 should passthrough");
    }

    #[test]
    fn chain_meta_stops_at_host_policy() {
        let key = KeyEvent::new(Key::Char('s'), Modifiers::META);
        let engine = VimEngine::new();
        let overrides = HashSet::new();
        let ctx = make_ctx(key, &engine, &overrides);
        assert!(run_passthrough_chain(&ctx), "Meta+s should passthrough");
    }

    #[test]
    fn chain_alt_reaches_engine_query_not_host_policy() {
        // Alt+x: HostPolicy is Undecided, so the chain continues to EngineQuery.
        // The default engine doesn't handle Alt+x, so it passes through.
        let key = KeyEvent::alt('x');
        let engine = VimEngine::new();
        let overrides = HashSet::new();
        let ctx = make_ctx(key, &engine, &overrides);
        assert!(
            run_passthrough_chain(&ctx),
            "Alt+x should passthrough via EngineQuery (not HostPolicy)"
        );
    }

    #[test]
    fn chain_user_override_intercepts_before_engine() {
        // Even though 'j' is a built-in command, a user override forces passthrough.
        let key = KeyEvent::char('j');
        let engine = VimEngine::new();
        let mut overrides = HashSet::new();
        overrides.insert(KeyEvent::char('j'));
        let ctx = make_ctx(key, &engine, &overrides);
        assert!(
            run_passthrough_chain(&ctx),
            "User override for 'j' should passthrough despite engine handling it"
        );
    }

    #[test]
    fn chain_plain_key_handled_by_engine() {
        // 'j' is a built-in Normal mode motion — engine claims it.
        let key = KeyEvent::char('j');
        let engine = VimEngine::new();
        let overrides = HashSet::new();
        let ctx = make_ctx(key, &engine, &overrides);
        assert!(
            !run_passthrough_chain(&ctx),
            "'j' should NOT passthrough — engine handles it"
        );
    }

    #[test]
    fn chain_escape_handled_by_engine() {
        let key = KeyEvent::escape();
        let engine = VimEngine::new();
        let overrides = HashSet::new();
        let ctx = make_ctx(key, &engine, &overrides);
        assert!(
            !run_passthrough_chain(&ctx),
            "Escape should NOT passthrough — engine handles it"
        );
    }
}
