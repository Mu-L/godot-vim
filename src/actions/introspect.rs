//! `:panelmap` — the introspector.
//!
//! It ships in the same commit as the dispatcher cutover, and that is not a
//! scheduling nicety. The cutover replaces a fixed keyset with a resolution
//! *model*: forest depth, probe order, seals, a capability gate, an
//! arbitration seam and two consumption policies. Every one of those can make
//! a key silently do nothing, and a config surface with no way to ask "why is
//! my key dead?" turns each of them into an unanswerable bug report.
//!
//! Two tiers live here, both pure string builders over the plane:
//!
//! - [`list_report`] — every live rule, grouped by surface in forest order,
//!   in the exact syntax a user would type back into a vimrc.
//! - [`explain_report`] — one key, resolved: the sampled chain, the surface
//!   stack, which probe matched, which rule won and on which surface, which
//!   gate fired if none did, and what the consumption policy would do with
//!   the outcome.
//!
//! Neither takes a `Gd<T>`, so both are golden-snapshot testable with no
//! editor running. The transport is `PendingUiAction::PanelCommand`, and the
//! report goes to the Output panel through `godot_print!` rather than to the
//! one-line status bar — a resolution trace is a dozen lines and the default
//! `log` level is Off.
//!
//! See `docs/DESIGN-rebindable-nav.md` §5.9 and the `P6` block in §10.

use std::fmt::Write as _;

use vim_core::keymap::{KeyEvent, MappingOwner};

use super::action::{ActionRegistry, RuleTarget};
use super::bind::{BindingIndex, Consumption, Repeat, Rule};
use super::keys::{parse_lhs, Probes};
use super::resolve::{resolve, CandidateTarget, Resolution, ResolveInput, Stop};
use super::surface::{Anchor, FocusChain, Seal, SurfacePath};

/// Render one rule the way a user would write it.
///
/// Round-trippable on purpose: the listing is not a pretty-printer, it is the
/// text you paste into a vimrc to reproduce what you are looking at. That is
/// the same anti-drift device the provider defaults use — one grammar, one
/// parser, one spelling.
pub(crate) fn render_rule(rule: &Rule, registry: &ActionRegistry) -> String {
    let mut out = String::from("panelmap");
    if rule.physical {
        out.push_str(" <physical>");
    }
    if rule.consume == Consumption::Void {
        out.push_str(" <void>");
    }
    if rule.repeat == Repeat::Suppress {
        out.push_str(" <norepeat>");
    }
    if rule.shift_tolerant {
        out.push_str(" <shift>");
    }
    if rule.nowait {
        out.push_str(" <nowait>");
    }
    let lhs: String = rule.lhs.iter().map(KeyEvent::to_vim_notation).collect();
    let target = match &rule.target {
        RuleTarget::Action(id) => registry
            .name_of(*id)
            .unwrap_or("<unregistered>")
            .to_string(),
        RuleTarget::Native => "native".to_string(),
        RuleTarget::Shortcut(path) => format!("<Shortcut>({path})"),
    };
    let _ = write!(out, " {} {lhs} {target}", rule.surface);
    for (key, value) in rule.params.iter() {
        let _ = write!(out, " {key}={value}");
    }
    out
}

/// Who installed a rule, for the "why did this one win" column.
fn owner_label(owner: &MappingOwner) -> String {
    match owner {
        MappingOwner::Host(tag) => tag.to_string(),
        MappingOwner::User => "user".to_string(),
        other => format!("{other:?}"),
    }
}

/// `:panelmap` with no arguments — every live binding, by surface.
///
/// Surfaces are listed in forest (probe) order, and rules within a surface in
/// slot-allocation order, because both are deterministic and neither is a
/// hash iteration. The introspector's golden snapshots depend on that, and so
/// does a user diffing two runs.
pub(crate) fn list_report(index: &BindingIndex, registry: &ActionRegistry) -> String {
    let mut out = String::new();
    out.push_str("--- panel bindings ---\n");
    let mut total = 0_usize;
    for surface in index.forest().ids() {
        let rules: Vec<&Rule> = index.rules_on(surface).collect();
        let spec = index.forest().get(surface);
        let seal = spec.map_or(Seal::Open, |s| s.seal);
        if rules.is_empty() {
            // Printed anyway: "this surface exists and binds nothing" is the
            // answer to half the questions the listing is asked.
            let note = match seal {
                Seal::Barrier => "  (barrier — takes no bindings)",
                _ => "  (no bindings)",
            };
            let _ = writeln!(out, "{surface}{note}");
            continue;
        }
        let parent = spec.and_then(|s| s.parent).unwrap_or("-");
        let _ = writeln!(out, "{surface}  (parent: {parent}, seal: {seal:?})");
        for rule in rules {
            total += 1;
            let _ = writeln!(
                out,
                "  {}    [{}]",
                render_rule(rule, registry),
                owner_label(&rule.owner)
            );
        }
    }
    let _ = writeln!(out, "--- {total} binding(s) ---");
    out
}

/// `:panelmap <lhs>` — resolve one key against the live chain and explain it.
///
/// The report answers the five questions the model can silently fail on, in
/// the order the model asks them: where am I, what is above me, which
/// interpretation of the key matched, which rule claimed it, and what happens
/// to the event afterwards.
pub(crate) fn explain_report(
    lhs: &str,
    chain: &FocusChain,
    path: &SurfacePath,
    index: &BindingIndex,
    registry: &ActionRegistry,
    vim_claims: &dyn Fn(KeyEvent) -> bool,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "--- panelmap {lhs} ---");

    let keys = match parse_lhs(lhs) {
        Ok(keys) => keys,
        Err(err) => {
            let _ = writeln!(out, "cannot parse '{lhs}': {err}");
            return out;
        }
    };
    let Some(&first) = keys.first() else {
        let _ = writeln!(out, "cannot parse '{lhs}': empty key sequence");
        return out;
    };
    if keys.len() > 1 {
        // The shell plane holds no multi-key rule on any editor-reachable
        // surface, and sequences arrive with the pending buffer. Explaining
        // one now would be inventing an answer.
        let _ = writeln!(
            out,
            "multi-key sequences are not resolvable yet; explaining the first key only"
        );
    }
    let key = first;

    // A synthesized probe list: exactly the as-typed interpretation. A real
    // keystroke may carry two more (a Latin collapse and a US-QWERTY
    // position), which is stated rather than faked — the plane has no way to
    // know which layout the user is on from a written `<C-h>`.
    let probes = Probes::from_key(key);

    // ── Where am I ───────────────────────────────────────────────────
    let _ = writeln!(out, "focus chain (leaf first):");
    if chain.nodes.is_empty() {
        let _ = writeln!(out, "  <no focus owner>");
    } else {
        for (i, node) in chain.nodes.iter().enumerate() {
            let _ = writeln!(out, "  [{i}] {} ({})", node.class, node.name);
        }
    }
    let _ = writeln!(
        out,
        "facts: attached_editor={:?} mode={:?} in_filesystem_dock={} sibling_nav={} prompt={}",
        chain.attached_editor,
        chain.editor_mode,
        chain.in_filesystem_dock,
        chain.sibling_nav_control.is_some(),
        chain.is_plugin_prompt
    );

    // ── What is above me ─────────────────────────────────────────────
    let _ = writeln!(
        out,
        "surface stack (deepest first): {}",
        path.ids.join(" -> ")
    );
    let anchor = match path.anchor {
        Anchor::Node(i) => format!("chain[{i}]"),
        Anchor::Rootless => "rootless (no focus owner)".to_string(),
    };
    let _ = writeln!(
        out,
        "anchor: {anchor}  caps: {:?}  seal: {:?}  yields_to_engine: {}  refuses_positional: {}",
        path.caps, path.seal, path.anchor_yields_to_engine, path.anchor_refuses_positional
    );

    // ── Candidates, surface by surface ───────────────────────────────
    let _ = writeln!(out, "candidates:");
    for &surface in &path.ids {
        let positional = !path.anchor_refuses_positional && index.has_physical_rule(surface);
        let mut any = false;
        for probe in probes.iter_scoped(positional) {
            let Some(rule) = index.rule_for(surface, &[probe]) else {
                continue;
            };
            any = true;
            let verdict = match &rule.target {
                RuleTarget::Action(id) => registry.get(*id).map_or_else(
                    || "SKIPPED (action not registered)".to_string(),
                    |spec| {
                        if path.caps.satisfies(spec.requires) {
                            format!("eligible ({})", spec.id)
                        } else {
                            format!(
                                "SKIPPED — {} needs {:?}, path offers {:?}",
                                spec.id, spec.requires, path.caps
                            )
                        }
                    },
                ),
                RuleTarget::Native => "native — hands the key back and stops the walk".to_string(),
                RuleTarget::Shortcut(p) => format!("eligible (editor shortcut {p})"),
            };
            let _ = writeln!(
                out,
                "  {surface}: {} via {} -> {verdict}",
                render_rule(rule, registry),
                probe.to_vim_notation()
            );
        }
        if !any {
            let _ = writeln!(out, "  {surface}: no rule for this key");
        }
    }

    // ── The verdict ──────────────────────────────────────────────────
    let resolution = resolve(&ResolveInput {
        probes: &probes,
        path,
        index,
        registry,
        vim_claims,
    });
    match resolution {
        Resolution::Run {
            matched,
            candidates,
        } => {
            let _ = writeln!(out, "matched: {}", matched.to_vim_notation());
            for candidate in &candidates {
                let name = match &candidate.target {
                    CandidateTarget::Action(_, spec) => spec.id.to_string(),
                    CandidateTarget::Shortcut(p) => format!("<Shortcut>({p})"),
                };
                let consumption = match candidate.consume {
                    Consumption::Void => {
                        "consumed ALWAYS (void) — even if the action declines, and the walk stops"
                    }
                    Consumption::Elastic => {
                        "consumed only if the action accepts (elastic); otherwise Godot gets the key"
                    }
                };
                let _ = writeln!(out, "runs: {name} on '{}'", candidate.surface);
                let _ = writeln!(out, "consumption: {consumption}");
                if candidate.repeat == Repeat::Suppress {
                    let _ = writeln!(
                        out,
                        "repeat: key-repeat echoes are consumed WITHOUT running the action"
                    );
                }
            }
        }
        Resolution::None(stop) => {
            let _ = writeln!(out, "nothing runs, and the key is NOT consumed.");
            let _ = writeln!(out, "reason: {}", explain_stop(stop));
        }
    }
    out
}

/// One sentence per stop reason, each naming the fix.
fn explain_stop(stop: Stop) -> String {
    match stop {
        Stop::Barrier => {
            "the anchor surface is a barrier (a foreign text input, or the editor in an \
             insert-like mode). Nothing is ever intercepted there."
                .to_string()
        }
        Stop::Sealed(surface) => format!(
            "'{surface}' is sealed and this key carries no Ctrl/Alt/Meta, so it stops at the \
             anchor and reaches the control's own input handling. A modifier-bearing key would \
             have continued to the forest root."
        ),
        Stop::Native(surface) => format!(
            "'{surface}' binds this key to `native`, which hands it back to Godot and terminates \
             the walk. `panelunmap` is the other verb: it removes the rule and lets the walk \
             continue to the parent surface."
        ),
        Stop::Yielded(key) => format!(
            "the script editor has focus and the Vim engine claims {} — your own `:map` wins. \
             Remove the mapping, or bind the panel action to a different chord.",
            key.to_vim_notation()
        ),
        Stop::Exhausted => "no surface on the stack binds this key, or every candidate was \
             gated out by a missing capability (see above)."
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::bind::builtin_index;
    use crate::actions::caps::Caps;
    use crate::actions::providers;
    use crate::actions::specs;
    use crate::actions::surface::fixtures::{code_edit, id, line_edit, plain, tree};
    use vim_core::keymap::{Key as VimKey, Modifiers};

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        for spec in specs::SHIPPED {
            r.register(spec);
        }
        r
    }

    const NEVER: &dyn Fn(KeyEvent) -> bool = &|_| false;
    const ALWAYS: &dyn Fn(KeyEvent) -> bool = &|_| true;

    fn explain(lhs: &str, chain: &FocusChain, claims: &dyn Fn(KeyEvent) -> bool) -> String {
        let reg = registry();
        let index = builtin_index(&reg);
        let path = providers::forest().classify(chain).expect("total probe");
        explain_report(lhs, chain, &path, &index, &reg, claims)
    }

    fn fs_chain() -> FocusChain {
        FocusChain {
            nodes: vec![
                tree("FileSystemTree", 1),
                plain("VBoxContainer", 2),
                plain("FileSystemDock", 3),
            ],
            in_filesystem_dock: true,
            ..Default::default()
        }
    }

    fn editor_chain(mode: Option<vim_core::primitives::Mode>) -> FocusChain {
        FocusChain {
            nodes: vec![code_edit(7), plain("CodeTextEditor", 8)],
            attached_editor: Some(id(7)),
            editor_mode: mode,
            ..Default::default()
        }
    }

    // ── The listing ──────────────────────────────────────────────────

    #[test]
    fn the_listing_is_a_golden_snapshot_of_the_shipped_keyset() {
        let reg = registry();
        let report = list_report(&builtin_index(&reg), &reg);
        insta_like(&report);
    }

    /// Asserted literally rather than through a snapshot crate: the shipped
    /// keyset is sixteen rules and a hand-written expectation is reviewable
    /// in a diff, which is the same argument the provider array makes.
    fn insta_like(report: &str) {
        let expected = "\
--- panel bindings ---
editor.nav  (no bindings)
editor.insert  (barrier — takes no bindings)
prompt  (no bindings)
searchbox  (parent: panel, seal: Sealed)
  panelmap <shift> searchbox <CR> godotvim.search.accept    [godotvim.searchbox]
  panelmap <shift> searchbox <Esc> godotvim.search.accept    [godotvim.searchbox]
dock.filesystem  (parent: dock, seal: Open)
  panelmap <physical> dock.filesystem a godotvim.fs.create    [godotvim.filesystem]
  panelmap <physical> dock.filesystem d godotvim.fs.delete    [godotvim.filesystem]
  panelmap <physical> dock.filesystem r godotvim.fs.rename    [godotvim.filesystem]
  panelmap <physical> dock.filesystem y godotvim.fs.yank_path    [godotvim.filesystem]
  panelmap <physical> dock.filesystem R godotvim.fs.refresh    [godotvim.filesystem]
dock  (parent: panel, seal: Open)
  panelmap <physical> dock h godotvim.item.collapse    [godotvim.dock]
  panelmap <physical> dock j godotvim.item.next    [godotvim.dock]
  panelmap <physical> dock k godotvim.item.prev    [godotvim.dock]
  panelmap <physical> dock l godotvim.item.expand    [godotvim.dock]
  panelmap <physical> dock / godotvim.dock.search    [godotvim.dock]
  panelmap dock <CR> godotvim.item.activate    [godotvim.dock]
  panelmap dock <Esc> godotvim.focus.editor    [godotvim.dock]
foreign  (barrier — takes no bindings)
unknown  (no bindings)
panel  (parent: -, seal: Open)
  panelmap <physical> <void> <norepeat> panel <C-h> godotvim.focus.left    [godotvim.panel]
  panelmap <physical> <void> <norepeat> panel <C-j> godotvim.focus.down    [godotvim.panel]
  panelmap <physical> <void> <norepeat> panel <C-k> godotvim.focus.up    [godotvim.panel]
  panelmap <physical> <void> <norepeat> panel <C-l> godotvim.focus.right    [godotvim.panel]
--- 18 binding(s) ---
";
        assert_eq!(report, expected);
    }

    #[test]
    fn every_listed_line_parses_back() {
        // The listing is the config syntax, not a pretty-printer. If this
        // ever fails, `:panelmap` is telling users to write something the
        // parser rejects.
        let reg = registry();
        let index = builtin_index(&reg);
        for rule in index.rules() {
            let line = render_rule(rule, &reg);
            let parsed = crate::config::panelmap::parse_panel_line(&line);
            assert!(parsed.is_ok(), "'{line}' does not parse back: {parsed:?}");
        }
    }

    // ── The explainer ────────────────────────────────────────────────

    #[test]
    fn it_explains_a_key_that_wins_on_the_deepest_surface() {
        let report = explain("d", &fs_chain(), NEVER);
        assert!(
            report.contains("dock.filesystem -> dock -> panel"),
            "{report}"
        );
        assert!(
            report.contains("runs: godotvim.fs.delete on 'dock.filesystem'"),
            "{report}"
        );
        assert!(report.contains("elastic"), "{report}");
    }

    #[test]
    fn it_explains_a_key_that_falls_through_to_a_parent() {
        let report = explain("j", &fs_chain(), NEVER);
        assert!(
            report.contains("dock.filesystem: no rule for this key"),
            "{report}"
        );
        assert!(
            report.contains("runs: godotvim.item.next on 'dock'"),
            "{report}"
        );
    }

    #[test]
    fn it_names_the_capability_that_gated_a_candidate_out() {
        // The single most useful line in the whole report: `l` on an ItemList
        // looks identical to `l` being unbound unless the gate is printed.
        let chain = FocusChain {
            nodes: vec![
                crate::actions::surface::fixtures::item_list("ItemList", 1),
                plain("VBoxContainer", 2),
            ],
            ..Default::default()
        };
        let report = explain("l", &chain, NEVER);
        assert!(report.contains("SKIPPED"), "{report}");
        assert!(report.contains("HIERARCHY"), "{report}");
        assert!(report.contains("NOT consumed"), "{report}");
    }

    #[test]
    fn it_names_the_arbitration_seam_when_the_engine_wins() {
        let report = explain(
            "<C-h>",
            &editor_chain(Some(vim_core::primitives::Mode::Normal)),
            ALWAYS,
        );
        assert!(report.contains("your own `:map` wins"), "{report}");
    }

    #[test]
    fn it_names_the_barrier_in_an_insert_like_mode() {
        for mode in [
            vim_core::primitives::Mode::Insert,
            vim_core::primitives::Mode::Replace,
            vim_core::primitives::Mode::CommandLine,
        ] {
            let report = explain("<C-h>", &editor_chain(Some(mode)), NEVER);
            assert!(report.contains("barrier"), "{mode:?}: {report}");
        }
    }

    #[test]
    fn it_names_the_seal_for_a_bare_key_in_a_filter_box() {
        let chain = FocusChain {
            nodes: vec![line_edit(1), plain("VBoxContainer", 2)],
            sibling_nav_control: Some(id(9)),
            ..Default::default()
        };
        let report = explain("x", &chain, NEVER);
        assert!(report.contains("sealed"), "{report}");
        // …and the same surface passes a chord straight through to `panel`.
        let chord = explain("<C-l>", &chain, NEVER);
        assert!(
            chord.contains("runs: godotvim.focus.right on 'panel'"),
            "{chord}"
        );
    }

    #[test]
    fn it_reports_the_void_policy_on_the_panel_chords() {
        let chain = FocusChain::default();
        let report = explain("<C-h>", &chain, NEVER);
        assert!(report.contains("<no focus owner>"), "{report}");
        assert!(report.contains("rootless"), "{report}");
        assert!(report.contains("consumed ALWAYS (void)"), "{report}");
        assert!(
            report.contains("echoes are consumed WITHOUT running"),
            "{report}"
        );
    }

    #[test]
    fn it_refuses_an_unparseable_key_instead_of_guessing() {
        // Nine keys: over `MAX_KEY_SEQUENCE_LEN`, so `parse_lhs` rejects it
        // rather than silently truncating into a binding nobody wrote.
        let report = explain("abcdefghi", &fs_chain(), NEVER);
        assert!(report.contains("cannot parse"), "{report}");
        assert!(
            !report.contains("surface stack"),
            "a report that could not parse its key must not pretend to resolve: {report}"
        );
    }

    #[test]
    fn it_says_so_rather_than_guessing_at_a_sequence() {
        // `<C-` is not an error to vim-core's parser — it is four literal
        // keys. Explaining a sequence needs the pending buffer, which the
        // shell plane does not have yet, so the report says which key it
        // answered for instead of inventing a walk.
        let report = explain("<C-", &fs_chain(), NEVER);
        assert!(report.contains("first key only"), "{report}");
    }

    #[test]
    fn it_reports_an_unbound_key_as_unbound() {
        let report = explain("q", &fs_chain(), NEVER);
        assert!(
            report.contains("no surface on the stack binds this key"),
            "{report}"
        );
    }

    #[test]
    fn caps_are_printed_so_a_gate_is_diagnosable() {
        let report = explain("d", &fs_chain(), NEVER);
        // The path's caps must be printed in full: a gate the user cannot see
        // is indistinguishable from an unbound key.
        for bit in ["VNAV", "HIERARCHY", "ACTIVATE", "FILEOPS"] {
            assert!(report.contains(bit), "{bit} missing from: {report}");
        }
        // …and the action's own requirement, so the two sets can be compared
        // side by side rather than inferred.
        let tree_in_fs_dock = Caps::VNAV | Caps::HIERARCHY | Caps::ACTIVATE | Caps::FILEOPS;
        assert!(
            report.contains(&format!("{tree_in_fs_dock:?}")),
            "the resolved path caps must appear verbatim: {report}"
        );
    }

    #[test]
    fn a_named_key_round_trips_through_the_explainer() {
        let chain = fs_chain();
        let report = explain("<CR>", &chain, NEVER);
        assert!(report.contains("runs: godotvim.item.activate"), "{report}");
    }

    #[test]
    fn shift_folding_is_visible_in_the_listing() {
        // `R` and `r` are two keys, not one key plus a modifier. If the
        // listing rendered `<S-r>` the user would copy back a binding that
        // can never fire.
        let reg = registry();
        let report = list_report(&builtin_index(&reg), &reg);
        assert!(report.contains("dock.filesystem R godotvim.fs.refresh"));
        assert!(!report.contains("<S-r>"));
        assert!(!report.contains("<S-R>"));
    }

    #[test]
    fn an_action_id_never_renders_as_an_action_pseudo_key() {
        // `Key::Action(7).to_vim_notation()` is literally `<Action>(7)`, so
        // the trie can never be the listing's source of truth. This is the
        // regression guard for reading the RHS instead of the arena.
        let reg = registry();
        let report = list_report(&builtin_index(&reg), &reg);
        assert!(!report.contains("<Action>("), "{report}");
    }

    #[test]
    fn the_key_notation_helper_agrees_with_the_parser() {
        for notation in ["<C-h>", "<CR>", "<Esc>", "a", "/", "R"] {
            let keys = parse_lhs(notation).expect("shipped notation parses");
            assert_eq!(keys.len(), 1);
            let round = parse_lhs(&keys[0].to_vim_notation()).expect("renders back");
            assert_eq!(round, keys, "{notation} does not round-trip");
        }
    }

    #[test]
    fn the_bare_char_key_events_are_what_the_runtime_produces() {
        assert_eq!(
            parse_lhs("R").expect("parses")[0],
            KeyEvent::new(VimKey::Char('R'), Modifiers::NONE)
        );
    }
}
