//! Where a keystroke *is*, as literal data.
//!
//! `classify_focus` answers "what has focus" by holding a `Gd<Control>` and
//! asking Godot questions at the moment of the decision. That works, and it is
//! why the dispatcher is untestable: `Gd<T>` cannot be constructed under
//! `cargo test` in a `cdylib`, so every classification rule in the plugin is
//! verified by hand in a running editor.
//!
//! The surface plane cuts that seam in half. [`FocusChain::sample`] performs
//! **all** the Godot work once per focus change and records the answers as
//! plain data — class names, instance ids, and five precomputed facts. Above
//! the seam sit the [`SurfaceSpec::probe`]s, which are pure
//! `fn(&FocusChain) -> Option<Anchor>` and therefore constructible from
//! literals: the golden fixture table in `providers::mod` is forty-odd focus
//! chains written out by hand, with no editor running.
//!
//! # The shape
//!
//! A **surface** is a named place in the editor UI where bindings live. Every
//! surface declares its parent in a forest ([`SurfaceSpec::parent`]), and depth
//! in *that* forest — never scene-tree depth, never a hand-assigned integer —
//! is the only specificity mechanism in the system. Classification is an
//! **ordered total function**: probes run in `providers::PROVIDERS` order and
//! the first `Some` wins, which is how N independent predicates keep the mutual
//! exclusivity today's one `if`-chain gets for free.
//!
//! See `docs/DESIGN-rebindable-nav.md` §3.3, §3.7, §4.4 and §5.4.

// Dead by design in P4, and that is the phase's whole claim: the plane is
// fully built and fully tested while nothing in production reads it, so the
// commit is revertable on its own. P5 builds the binding index against these
// types; P6 samples a chain in `handle_input_impl` and deletes `classify_focus`
// in the same commit.
#![allow(
    dead_code,
    reason = "the surface plane gains its production callers in P5 (binding index) and P6 (dispatcher cutover)"
)]

use bitflags::bitflags;
use compact_str::CompactString;
use godot::prelude::{Gd, InstanceId};

use super::action::ActionCtx;
use super::caps::Caps;

/// A surface's stable public name. Users type these in `panelmap` lines.
pub(crate) type SurfaceId = &'static str;

bitflags! {
    /// The `is_class` answers the surface plane is allowed to ask for.
    ///
    /// A probe runs on recorded data and therefore cannot call into Godot, so
    /// the questions have to be asked at sample time — which means the set of
    /// questions is closed and lives here. That is a real constraint and it is
    /// the same one [`Caps`] carries: a surface needing a class outside this
    /// vocabulary adds a bit, and the sampler starts asking for it.
    ///
    /// Recording *answers* rather than the concrete class name is what makes
    /// subclasses work. Godot's editor is built from them — `FileSystemTree`
    /// is a `Tree`, `FileSystemList` is an `ItemList` — so a probe comparing
    /// `node.class == "Tree"` would be silently wrong in the FileSystem dock,
    /// which is precisely where the design's sharpest behaviour lives.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub(crate) struct ClassMask: u8 {
        const TREE = 1 << 0;
        const ITEM_LIST = 1 << 1;
        const RICH_TEXT_LABEL = 1 << 2;
        const LINE_EDIT = 1 << 3;
        /// True for a `CodeEdit` too — `CodeEdit` derives from `TextEdit`.
        const TEXT_EDIT = 1 << 4;
        const CODE_EDIT = 1 << 5;
    }
}

/// The closed question set, paired with the bit each answer sets.
///
/// Iterated by [`ClassMask::of`] at sample time and by [`ClassMask::answers`]
/// when a probe asks. One array, so the two cannot disagree.
const VOCABULARY: &[(&str, ClassMask)] = &[
    ("Tree", ClassMask::TREE),
    ("ItemList", ClassMask::ITEM_LIST),
    ("RichTextLabel", ClassMask::RICH_TEXT_LABEL),
    ("LineEdit", ClassMask::LINE_EDIT),
    ("TextEdit", ClassMask::TEXT_EDIT),
    ("CodeEdit", ClassMask::CODE_EDIT),
];

impl ClassMask {
    /// Ask every question in the vocabulary. `is_a` is `Node::is_class` in
    /// production — the same string predicate `classify_focus` uses — and a
    /// literal set in tests.
    pub(crate) fn of(is_a: impl Fn(&str) -> bool) -> Self {
        let mut mask = Self::empty();
        for (class, bit) in VOCABULARY {
            if is_a(class) {
                mask |= *bit;
            }
        }
        mask
    }

    /// Replay a recorded answer. Outside the vocabulary the answer is `false`
    /// — deliberately, because a probe that silently got `true` for a class
    /// nobody sampled would be a lie rather than a miss.
    pub(crate) fn answers(self, class: &str) -> bool {
        VOCABULARY
            .iter()
            .find(|(name, _)| *name == class)
            .is_some_and(|(_, bit)| self.contains(*bit))
    }
}

/// One node of the sampled focus ancestor chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChainNode {
    /// The concrete class name, e.g. `"FileSystemTree"`. Used for ancestor
    /// lookups and diagnostics; **not** for widget identification, which goes
    /// through [`ChainNode::classes`] so subclasses answer correctly.
    pub(crate) class: CompactString,
    pub(crate) name: CompactString,
    pub(crate) instance: InstanceId,
    /// Recorded `is_class` answers over the closed vocabulary.
    pub(crate) classes: ClassMask,
}

impl ChainNode {
    pub(crate) fn new(class: &str, name: &str, instance: InstanceId, classes: ClassMask) -> Self {
        debug_assert!(
            !classes.contains(ClassMask::CODE_EDIT) || classes.contains(ClassMask::TEXT_EDIT),
            "a CodeEdit is a TextEdit; recording one without the other makes \
             `foreign` miss a non-attached CodeEdit"
        );
        Self {
            class: CompactString::from(class),
            name: CompactString::from(name),
            instance,
            classes,
        }
    }

    /// Whether this node answers `is_class(class)`.
    ///
    /// Falls back to an exact match on the concrete class name, which is what
    /// makes an ancestor query like `index_of_ancestor("EditorDebuggerNode")`
    /// work without every editor-internal container joining the vocabulary. A
    /// *subclass* of such an ancestor would miss; the fix is then either a
    /// vocabulary bit or a precomputed chain fact, and `in_filesystem_dock` is
    /// the second kind for exactly this reason.
    pub(crate) fn is(&self, class: &str) -> bool {
        self.classes.answers(class) || self.class == class
    }

    /// Affordances this widget contributes, derived from the recorded answers.
    ///
    /// Derived rather than stored so a fixture cannot claim to be a `Tree`
    /// while offering no `HIERARCHY`. [`Caps::of_control`] stays the single
    /// definition of the class→affordance mapping.
    pub(crate) fn widget_caps(&self) -> Caps {
        Caps::of_control(|class| self.classes.answers(class))
    }
}

/// Everything the surface plane knows, sampled once per focus change.
///
/// Contains no `Gd<T>` by construction. That is the entire point: it is what
/// makes probes, seals, the capability algebra and the partition audit
/// testable in a crate where a `Gd<T>` cannot exist under `cargo test`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FocusChain {
    /// Index 0 is `viewport.gui_get_focus_owner()`; higher indices are its
    /// ancestors, bounded by [`crate::scene_tree::MAX_DISCOVERY_DEPTH`].
    /// **Empty** when there is no focus owner at all — a real, mandatory
    /// state, and the reason [`Anchor::Rootless`] exists.
    pub(crate) nodes: Vec<ChainNode>,
    /// The `CodeEdit` this plugin is attached to, if any.
    pub(crate) attached_editor: Option<InstanceId>,
    /// The vim controller's mode. `None` means "no controller", which maps to
    /// `editor.nav` rather than to the barrier — transcribing the `is_none_or`
    /// polarity at `src/plugin/input.rs:116`.
    pub(crate) editor_mode: Option<vim_core::primitives::Mode>,
    /// `FileSystemDock::is_ancestor_of(focus_owner)`, evaluated once and
    /// unbounded (`src/navigation/filesystem_explorer.rs:394-400`). Grants
    /// [`Caps::FILEOPS`] through the `dock.filesystem` surface.
    pub(crate) in_filesystem_dock: bool,
    /// Discriminant for the `searchbox` probe ONLY, reproducing
    /// `src/navigation/focus.rs:73-82`. There is deliberately no
    /// `sibling_search_box` field: the depth-20 DFS that finds one stays
    /// inside `handle_slash`, run once per `/` press exactly as today, rather
    /// than once per focus change.
    pub(crate) sibling_nav_control: Option<InstanceId>,
    /// Instance equality against the `FileSystemExplorer` prompt `LineEdit`.
    /// Probed before `searchbox` **and** before `foreign`; see the ordering
    /// argument on `providers::PROVIDERS`.
    pub(crate) is_plugin_prompt: bool,
}

impl FocusChain {
    /// The focus owner, or `None` when nothing has focus.
    pub(crate) fn focus(&self) -> Option<&ChainNode> {
        self.nodes.first()
    }

    /// Whether the focus owner answers `is_class(class)`.
    pub(crate) fn focus_is(&self, class: &str) -> bool {
        self.focus().is_some_and(|n| n.is(class))
    }

    /// Position of the nearest node answering `class`, focus owner included.
    pub(crate) fn index_of_ancestor(&self, class: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.is(class))
    }

    /// Whether the focus owner is the `CodeEdit` this plugin drives.
    pub(crate) fn attached_editor_focused(&self) -> bool {
        match (self.focus(), self.attached_editor) {
            (Some(node), Some(attached)) => node.instance == attached,
            _ => false,
        }
    }

    /// Affordances of the focus owner. Ancestors contribute nothing — a
    /// surface that wants to add capabilities does it through `grants`.
    pub(crate) fn widget_caps(&self) -> Caps {
        self.focus()
            .map_or_else(Caps::empty, ChainNode::widget_caps)
    }

    /// The single Godot→Rust seam of the whole subsystem.
    ///
    /// Walks the focus owner upward and records what every probe downstream
    /// will need. Runs once per focus change, never once per keystroke: the
    /// caller caches the result against `(focus owner, plugin epoch, index
    /// generation)`.
    ///
    /// `prompt` is the `FileSystemExplorer` prompt's instance id, passed in
    /// rather than looked up so this stays a leaf function with no plugin
    /// borrow — the dispatcher already holds `&mut GodotVimCore`.
    pub(crate) fn sample(
        viewport: &Gd<godot::classes::Viewport>,
        attached_editor: Option<InstanceId>,
        editor_mode: Option<vim_core::primitives::Mode>,
        prompt: Option<InstanceId>,
    ) -> Self {
        let base = Self {
            attached_editor,
            editor_mode,
            ..Self::default()
        };
        // No focus owner is not an error state: `classify_focus` returns
        // `Unknown` for it and the dispatcher still consumes Ctrl+hjkl there.
        let Some(focus_owner) = viewport.gui_get_focus_owner() else {
            return base;
        };

        let mut nodes = Vec::new();
        let mut current: Gd<godot::classes::Node> = focus_owner.clone().upcast();
        for _ in 0..crate::scene_tree::MAX_DISCOVERY_DEPTH {
            let classes = ClassMask::of(|class| current.is_class(class));
            nodes.push(ChainNode::new(
                &current.get_class().to_string(),
                &current.get_name().to_string(),
                current.instance_id(),
                classes,
            ));
            let Some(parent) = current.get_parent() else {
                break;
            };
            current = parent;
        }

        // Only a LineEdit can be a filter box, and the sibling search is the
        // expensive part of sampling (a depth-8 climb over a depth-20 DFS), so
        // it stays gated exactly as `classify_focus` gates it.
        let sibling_nav_control = if focus_owner.is_class("LineEdit") {
            crate::navigation::find_sibling_nav_control(&focus_owner).map(|c| c.instance_id())
        } else {
            None
        };

        Self {
            nodes,
            in_filesystem_dock: crate::navigation::is_in_filesystem_dock(&focus_owner),
            sibling_nav_control,
            is_plugin_prompt: prompt.is_some_and(|p| p == focus_owner.instance_id()),
            ..base
        }
    }
}

/// Where on the chain a surface matched.
///
/// Not `Option<usize>`, because "no focus owner at all" has no chain index and
/// is a state the dispatcher must still act in: `classify_focus` returns
/// `Unknown` for it (`src/navigation/focus.rs:46-48`), `input.rs` maps that to
/// intercept, and then calls `set_input_as_handled()` with no target found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    /// Anchored at `chain.nodes[idx]`.
    Node(usize),
    /// Matched with NO focus owner. Only `unknown` may return this, and
    /// `ActionCtx::target` is then `None`.
    Rootless,
}

/// A pure predicate over the sampled chain. No `Gd<T>`, so it is unit-testable
/// from literals with no Godot runtime.
pub(crate) type Probe = fn(&FocusChain) -> Option<Anchor>;

/// How a surface terminates the upward walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seal {
    /// Bindings on ancestors of this surface still apply.
    Open,
    /// BARE keys (no Ctrl/Alt/Meta) stop here and fall through to the
    /// control's own `gui_input`; modifier-bearing keys continue to the forest
    /// root. One rule delivers three behaviours: `<CR>` still reaches the FS
    /// prompt's `text_submitted`, typing in a dock filter box still types, and
    /// Ctrl+hjkl still escapes both.
    Sealed,
    /// Total hard stop: dispatch returns `Ignore` before any lookup. The
    /// structural form of `FocusContext::Foreign => false` and of
    /// "never intercept in insert-like modes".
    Barrier,
}

/// A declared place in the editor UI where bindings live.
pub(crate) struct SurfaceSpec {
    pub(crate) id: SurfaceId,
    /// Declared forest parent. `None` only for roots — `panel`,
    /// `editor.insert` and `foreign`.
    pub(crate) parent: Option<SurfaceId>,
    pub(crate) seal: Seal,
    /// Capabilities this surface adds on top of the widget's own. The one
    /// non-tautological grant is `dock.filesystem`'s [`Caps::FILEOPS`]:
    /// membership of a dock is not a property of any widget class, which is
    /// why `Caps::of_control` can never contribute it.
    pub(crate) grants: fn(&FocusChain) -> Caps,
    /// Probes run in `providers::PROVIDERS` order; the first `Some` wins.
    pub(crate) probe: Probe,
    /// Runs once per keystroke for every surface on the active path, before
    /// any lookup and regardless of whether a binding matches. Must be
    /// idempotent and cheap — key-repeat echo events reach it too.
    pub(crate) on_key: Option<fn(&mut ActionCtx<'_>)>,
    /// `true` on `editor.nav` only, and not settable from config. When the
    /// ANCHOR surface declares it and the vim engine claims the matched key,
    /// dispatch is abandoned and the key flows on to `gui_input`. Living here
    /// rather than on a binding is what makes the `editor.nav`/`panel`
    /// duplication gap unrepresentable.
    pub(crate) yields_to_engine: bool,
    /// `true` on `editor.nav` only, and not settable from config. When the
    /// ANCHOR surface declares it, the US-QWERTY positional probe is withheld
    /// from every surface on the path — even from a rule that carries
    /// `<physical>`.
    ///
    /// This is the surface-plane transcription of `resolve_panel_key_typed`
    /// (`src/plugin/input.rs:151-155`), and it is a guard, not a preference.
    /// On Dvorak the QWERTY-H position emits `d`, so honouring the positional
    /// probe inside the attached editor turns `Ctrl+d` — half-page-down —
    /// into panel-left; Colemak does the same to `Ctrl+n` and `Ctrl+e`. It
    /// lives on the surface rather than on the rule because the reason is a
    /// property of *where* the key was pressed: inside the editor every chord
    /// already has a meaning worth protecting, and a positional guess is a
    /// guess about intent that must never outrank it. Non-Latin layouts lose
    /// nothing — `resolve_ctrl_key` resolves those to a Latin key as probe 1.
    pub(crate) refuses_positional: bool,
}

impl std::fmt::Debug for SurfaceSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceSpec")
            .field("id", &self.id)
            .field("parent", &self.parent)
            .field("seal", &self.seal)
            .finish_non_exhaustive()
    }
}

/// The resolved active path, deepest surface first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SurfacePath {
    /// e.g. `["dock.filesystem", "dock", "panel"]`.
    pub(crate) ids: Vec<SurfaceId>,
    pub(crate) anchor: Anchor,
    /// Widget caps of the anchored node ∪ every `grants` on the path.
    pub(crate) caps: Caps,
    /// Seal of the DEEPEST surface.
    pub(crate) seal: Seal,
    /// `yields_to_engine` of the deepest surface only.
    pub(crate) anchor_yields_to_engine: bool,
    /// `refuses_positional` of the deepest surface only. Withholds probe 3
    /// from the whole walk, not merely from the anchor's own rules — the
    /// `<C-h>` rule lives on `panel`, which is the surface a positional guess
    /// from inside the editor would reach.
    pub(crate) anchor_refuses_positional: bool,
}

/// The declared parent relation over a set of surfaces, in probe order.
#[derive(Debug, Default)]
pub(crate) struct Forest {
    specs: Vec<&'static SurfaceSpec>,
}

impl Forest {
    /// `specs` is in probe order — that ordering is load-bearing and is
    /// audited by [`Forest::audit`] plus the ordering tests in
    /// `providers::mod`.
    pub(crate) fn new(specs: &[&'static SurfaceSpec]) -> Self {
        Self {
            specs: specs.to_vec(),
        }
    }

    pub(crate) fn get(&self, id: SurfaceId) -> Option<&'static SurfaceSpec> {
        self.specs.iter().find(|s| s.id == id).copied()
    }

    pub(crate) fn ids(&self) -> impl Iterator<Item = SurfaceId> + '_ {
        self.specs.iter().map(|s| s.id)
    }

    pub(crate) fn len(&self) -> usize {
        self.specs.len()
    }

    /// The declared path from `leaf` to its root, deepest first.
    ///
    /// Bounded by `self.specs.len()` iterations. A declared cycle is rejected
    /// by [`Forest::audit`] at registration; the bound is defence in depth so
    /// that a malformed third-party provider cannot hang `input()`.
    pub(crate) fn path_from(&self, leaf: SurfaceId) -> Vec<SurfaceId> {
        let mut path = Vec::new();
        let mut current = Some(leaf);
        for _ in 0..self.specs.len() {
            let Some(id) = current else { break };
            let Some(spec) = self.get(id) else { break };
            path.push(spec.id);
            current = spec.parent;
        }
        path
    }

    /// Whether `maybe` is `of` or one of its declared ancestors.
    pub(crate) fn is_ancestor_or_self(&self, maybe: SurfaceId, of: SurfaceId) -> bool {
        self.path_from(of).contains(&maybe)
    }

    /// Every surface whose probe claims `chain`, in probe order.
    ///
    /// The partition audit's input: two claimants that are not forest-related
    /// mean two providers disagree about who owns a control, resolved by array
    /// position — nondeterministic to the authors and invisible to the user.
    pub(crate) fn claimants(&self, chain: &FocusChain) -> Vec<(SurfaceId, Anchor)> {
        self.specs
            .iter()
            .filter_map(|s| (s.probe)(chain).map(|a| (s.id, a)))
            .collect()
    }

    /// Classify a sampled chain into its active path.
    ///
    /// An ordered total function over the shipped forest: `unknown`'s probe
    /// returns `Some` unconditionally, so this only answers `None` for a
    /// forest that has no total probe at all.
    pub(crate) fn classify(&self, chain: &FocusChain) -> Option<SurfacePath> {
        let (spec, anchor) = self
            .specs
            .iter()
            .find_map(|s| (s.probe)(chain).map(|anchor| (*s, anchor)))?;

        let ids = self.path_from(spec.id);
        let mut caps = match anchor {
            Anchor::Node(idx) => chain
                .nodes
                .get(idx)
                .map_or_else(Caps::empty, ChainNode::widget_caps),
            Anchor::Rootless => Caps::empty(),
        };
        for id in &ids {
            if let Some(surface) = self.get(id) {
                caps |= (surface.grants)(chain);
            }
        }

        Some(SurfacePath {
            ids,
            anchor,
            caps,
            seal: spec.seal,
            anchor_yields_to_engine: spec.yields_to_engine,
            anchor_refuses_positional: spec.refuses_positional,
        })
    }

    /// Structural validation, as human-readable errors.
    ///
    /// Covers V1 (a surface id is declared once), V3 (every parent is declared
    /// and the graph is acyclic) and the ordering half of V4 (probe order is a
    /// linear extension of descendant-before-ancestor). The totality half of
    /// V4 — that exactly one surface probes unconditionally and that it is the
    /// last probing entry — cannot be decided from a fn pointer and is
    /// asserted over the golden fixture table instead.
    pub(crate) fn audit(&self) -> Vec<String> {
        let mut errors = Vec::new();

        for (i, spec) in self.specs.iter().enumerate() {
            // V1 — declared once. A second declaration is an error, never an
            // overwrite, or a third party could silently redefine `dock`.
            if self.specs[..i].iter().any(|s| s.id == spec.id) {
                errors.push(format!("surface '{}' is declared twice", spec.id));
            }
            let Some(parent) = spec.parent else { continue };
            // V3 — a typo'd parent yields a one-element path with no `panel`,
            // i.e. a surface that has silently lost Ctrl+hjkl.
            let Some(parent_spec) = self.get(parent) else {
                errors.push(format!(
                    "surface '{}' names undeclared parent '{parent}'",
                    spec.id
                ));
                continue;
            };
            // V3 — acyclic. `path_from` is bounded, so a cycle shows up as a
            // path that never reaches a root.
            let path = self.path_from(spec.id);
            if path.len() == self.specs.len()
                && self
                    .get(path[path.len() - 1])
                    .is_some_and(|s| s.parent.is_some())
            {
                errors.push(format!("surface '{}' lies on a parent cycle", spec.id));
            }
            // V4 — descendant before ancestor, or the child is unreachable and
            // its bindings are silently dead.
            let parent_pos = self.specs.iter().position(|s| s.id == parent_spec.id);
            if parent_pos.is_some_and(|p| p < i) {
                errors.push(format!(
                    "surface '{}' is probed after its parent '{parent}'; the child would never match",
                    spec.id
                ));
            }
        }

        errors
    }
}

/// Literal chains, for tests that need a focus context without an editor.
///
/// Lives here rather than in the test module of `providers` because every
/// provider's own probe tests want them too.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::{ChainNode, ClassMask, FocusChain, InstanceId};

    pub(crate) fn id(n: i64) -> InstanceId {
        InstanceId::from_i64(n)
    }

    fn node(class: &str, n: i64, classes: ClassMask) -> ChainNode {
        ChainNode::new(class, class, id(n), classes)
    }

    /// A `Tree` subclass, e.g. Godot's `FileSystemTree`.
    pub(crate) fn tree(class: &str, n: i64) -> ChainNode {
        node(class, n, ClassMask::TREE)
    }

    pub(crate) fn item_list(class: &str, n: i64) -> ChainNode {
        node(class, n, ClassMask::ITEM_LIST)
    }

    pub(crate) fn rich_text(class: &str, n: i64) -> ChainNode {
        node(class, n, ClassMask::RICH_TEXT_LABEL)
    }

    pub(crate) fn line_edit(n: i64) -> ChainNode {
        node("LineEdit", n, ClassMask::LINE_EDIT)
    }

    /// A plain `TextEdit` — not a `CodeEdit`, so not ours even in principle.
    pub(crate) fn text_edit(n: i64) -> ChainNode {
        node("TextEdit", n, ClassMask::TEXT_EDIT)
    }

    /// A `CodeEdit`, which answers `is_class("TextEdit")` too.
    pub(crate) fn code_edit(n: i64) -> ChainNode {
        node(
            "CodeEdit",
            n,
            ClassMask::CODE_EDIT.union(ClassMask::TEXT_EDIT),
        )
    }

    /// Anything the vocabulary does not recognize: a container, a Button, a
    /// GraphEdit.
    pub(crate) fn plain(class: &str, n: i64) -> ChainNode {
        node(class, n, ClassMask::empty())
    }

    /// The empty chain: no focus owner at all.
    pub(crate) fn no_focus_owner() -> FocusChain {
        FocusChain::default()
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    // ── The recorded class vocabulary ────────────────────────────────

    #[test]
    fn a_subclass_answers_its_base_class() {
        // The reason answers are recorded rather than the class name: Godot's
        // editor is built from Tree/ItemList subclasses, and the FileSystem
        // dock — where the design's sharpest behaviour lives — is all of them.
        let fs_tree = tree("FileSystemTree", 1);
        assert!(fs_tree.is("Tree"));
        assert!(fs_tree.is("FileSystemTree"), "concrete name still matches");
        assert!(!fs_tree.is("ItemList"));
    }

    #[test]
    fn an_unsampled_class_answers_false_rather_than_guessing() {
        let button = plain("Button", 1);
        assert!(!button.is("Tree"));
        assert!(button.is("Button"), "the concrete name is always available");
    }

    #[test]
    fn a_code_edit_answers_text_edit() {
        // `foreign` claims "a TextEdit that is not ours", which must catch a
        // foreign CodeEdit — the arm at focus.rs:50-58.
        let ce = code_edit(1);
        assert!(ce.is("CodeEdit"));
        assert!(ce.is("TextEdit"));
    }

    #[test]
    fn widget_caps_come_from_caps_of_control() {
        // Derived, not stored: a fixture cannot claim to be a Tree while
        // offering no HIERARCHY.
        assert_eq!(
            tree("Tree", 1).widget_caps(),
            Caps::VNAV | Caps::HIERARCHY | Caps::ACTIVATE
        );
        assert_eq!(
            item_list("ItemList", 1).widget_caps(),
            Caps::VNAV | Caps::ACTIVATE
        );
        assert_eq!(rich_text("RichTextLabel", 1).widget_caps(), Caps::VNAV);
        assert_eq!(line_edit(1).widget_caps(), Caps::TEXTENTRY);
        assert_eq!(code_edit(1).widget_caps(), Caps::TEXTENTRY);
        assert_eq!(plain("GraphEdit", 1).widget_caps(), Caps::empty());
    }

    #[test]
    fn class_mask_of_asks_every_question_once() {
        let mask = ClassMask::of(|c| ["CodeEdit", "TextEdit", "Control"].contains(&c));
        assert_eq!(mask, ClassMask::CODE_EDIT | ClassMask::TEXT_EDIT);
        assert!(mask.answers("TextEdit"));
        assert!(!mask.answers("Control"), "outside the vocabulary");
    }

    // ── Chain accessors ──────────────────────────────────────────────

    #[test]
    fn an_empty_chain_has_no_focus_and_no_caps() {
        let chain = no_focus_owner();
        assert!(chain.focus().is_none());
        assert!(!chain.focus_is("Tree"));
        assert!(!chain.attached_editor_focused());
        assert_eq!(chain.widget_caps(), Caps::empty());
    }

    #[test]
    fn ancestors_are_searchable_by_concrete_class() {
        let chain = FocusChain {
            nodes: vec![
                tree("FileSystemTree", 1),
                plain("SplitContainer", 2),
                plain("FileSystemDock", 3),
            ],
            ..Default::default()
        };
        assert_eq!(chain.index_of_ancestor("FileSystemDock"), Some(2));
        assert_eq!(chain.index_of_ancestor("EditorDebuggerNode"), None);
        // The focus owner counts as its own ancestor for this query.
        assert_eq!(chain.index_of_ancestor("Tree"), Some(0));
    }

    #[test]
    fn attachment_is_instance_identity_not_class_identity() {
        // Two CodeEdits with the same class; only the one we attached to is
        // ours. This is the whole `Editor` vs `Foreign` split at focus.rs:50.
        let ours = FocusChain {
            nodes: vec![code_edit(7)],
            attached_editor: Some(id(7)),
            ..Default::default()
        };
        let theirs = FocusChain {
            nodes: vec![code_edit(9)],
            attached_editor: Some(id(7)),
            ..Default::default()
        };
        assert!(ours.attached_editor_focused());
        assert!(!theirs.attached_editor_focused());
    }

    // ── The forest ───────────────────────────────────────────────────

    static ROOT: SurfaceSpec = SurfaceSpec {
        id: "t.root",
        parent: None,
        seal: Seal::Open,
        grants: |_| Caps::empty(),
        probe: |_| None,
        on_key: None,
        yields_to_engine: false,
        refuses_positional: false,
    };
    static CHILD: SurfaceSpec = SurfaceSpec {
        id: "t.child",
        parent: Some("t.root"),
        seal: Seal::Open,
        grants: |_| Caps::VNAV,
        probe: |chain| chain.focus().map(|_| Anchor::Node(0)),
        on_key: None,
        yields_to_engine: false,
        refuses_positional: false,
    };
    static ORPHAN: SurfaceSpec = SurfaceSpec {
        id: "t.orphan",
        parent: Some("t.nowhere"),
        seal: Seal::Open,
        grants: |_| Caps::empty(),
        probe: |_| None,
        on_key: None,
        yields_to_engine: false,
        refuses_positional: false,
    };

    #[test]
    fn a_path_is_deepest_first_and_ends_at_a_root() {
        let forest = Forest::new(&[&CHILD, &ROOT]);
        assert_eq!(forest.path_from("t.child"), vec!["t.child", "t.root"]);
        assert_eq!(forest.path_from("t.root"), vec!["t.root"]);
        assert!(forest.is_ancestor_or_self("t.root", "t.child"));
        assert!(forest.is_ancestor_or_self("t.child", "t.child"));
        assert!(!forest.is_ancestor_or_self("t.child", "t.root"));
    }

    #[test]
    fn an_unknown_leaf_yields_an_empty_path_rather_than_panicking() {
        let forest = Forest::new(&[&CHILD, &ROOT]);
        assert!(forest.path_from("t.nope").is_empty());
        assert!(forest.get("t.nope").is_none());
    }

    #[test]
    fn classification_unions_widget_caps_with_every_grant_on_the_path() {
        let forest = Forest::new(&[&CHILD, &ROOT]);
        let chain = FocusChain {
            nodes: vec![line_edit(1)],
            ..Default::default()
        };
        let path = forest.classify(&chain).expect("child claims any focus");
        assert_eq!(path.ids, vec!["t.child", "t.root"]);
        // TEXTENTRY from the widget, VNAV from the surface's grant.
        assert_eq!(path.caps, Caps::TEXTENTRY | Caps::VNAV);
    }

    #[test]
    fn a_forest_with_no_total_probe_can_fail_to_classify() {
        let forest = Forest::new(&[&ROOT]);
        assert!(forest.classify(&no_focus_owner()).is_none());
    }

    #[test]
    fn the_audit_names_an_undeclared_parent() {
        let errors = Forest::new(&[&ORPHAN, &ROOT]).audit();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("undeclared parent"), "{errors:?}");
    }

    #[test]
    fn the_audit_names_a_duplicate_declaration() {
        let errors = Forest::new(&[&CHILD, &CHILD, &ROOT]).audit();
        assert!(
            errors.iter().any(|e| e.contains("declared twice")),
            "{errors:?}"
        );
    }

    #[test]
    fn the_audit_rejects_an_ancestor_probed_before_its_descendant() {
        // V4. `dock` probing before `dock.filesystem` would make the child
        // unreachable and every FileSystem binding silently dead.
        let errors = Forest::new(&[&ROOT, &CHILD]).audit();
        assert!(
            errors.iter().any(|e| e.contains("probed after its parent")),
            "{errors:?}"
        );
    }
}
