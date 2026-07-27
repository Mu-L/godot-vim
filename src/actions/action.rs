//! Named actions: identity and parameters.
//!
//! Every shell-side verb gets a stable dotted name (`godotvim.item.next`) and
//! an interned id. Naming is what dissolves the asymmetry the design set out
//! to fix: inside the editor `<C-w>h` already had a name and was therefore
//! remappable, while the identical action from a dock was an anonymous match
//! arm. Once both address one `ActionSpec`, one binding table serves both.
//!
//! Ids are minted by the shell's own [`vim_core::keymap::NameRegistry`], the
//! same interner vim-core uses for `<Plug>` and `<Action>` pseudo-keys, so an
//! id can be carried inside a `KeyEvent` as `Key::Action(u32)` and flow
//! through machinery that knows nothing about Godot.

use compact_str::CompactString;
use godot::classes::Control;
use godot::prelude::*;
use vim_core::keymap::KeyEvent;

use super::caps::Caps;
use super::outcome::Outcome;

/// A registered action, identified by its interned id.
///
/// `Copy` and pointer-width so it can live inside a `Key` without making
/// `Key` allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ActionId(pub(crate) u32);

#[allow(
    dead_code,
    reason = "pseudo-key form for the shell's own use; the engine crossing is by NAME"
)]
impl ActionId {
    /// The pseudo-key carrying this id.
    ///
    /// **Not** the shell↔engine crossing. `ActionNames` wraps its own
    /// `NameRegistry`, a different instance from the engine's, so handing a
    /// raw id across would resolve against the wrong table — actions cross by
    /// NAME. This is for the shell's own internal use.
    pub(crate) const fn as_key(self) -> KeyEvent {
        KeyEvent::action(self.0)
    }
}

/// Upper bound on any repeat count.
///
/// Not a style choice. `find_navigable_target` walks up to `MAX_ATTEMPTS`
/// (1000) items per call, so an unbounded count is a frozen editor rather
/// than a slow keystroke. Always reach a count through [`Params::count`],
/// never by reading the raw integer.
pub(crate) const MAX_ACTION_COUNT: i64 = 100;

/// Scalar arguments attached to a binding.
///
/// Values are **decimal integers only**, and the grammar must not grow a
/// string or enum form. A closed integer vocabulary is what makes extending
/// the config sandbox provable: a parameter can never expand into `:!`,
/// `:source`, or a recursive mapping chain, so a binding read from a
/// project-level vimrc cannot become code execution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Params(Vec<(CompactString, i64)>);

#[allow(
    dead_code,
    reason = "set_int/iter are consumed by the panelmap parser in P5"
)]
impl Params {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Read a parameter, or `default` if absent.
    pub(crate) fn int(&self, key: &str, default: i64) -> i64 {
        self.0
            .iter()
            .find(|(k, _)| k.as_str() == key)
            .map_or(default, |(_, v)| *v)
    }

    /// Set a parameter, replacing any existing value for `key`.
    ///
    /// Last writer wins, matching how a later config line overrides an
    /// earlier one.
    pub(crate) fn set_int(&mut self, key: &str, value: i64) {
        match self.0.iter_mut().find(|(k, _)| k.as_str() == key) {
            Some((_, v)) => *v = value,
            None => self.0.push((CompactString::from(key), value)),
        }
    }

    /// The repeat count, clamped to a survivable range.
    ///
    /// **Always use this for repeat loops.** Reading `int("count", 1)`
    /// directly re-opens the freeze that [`MAX_ACTION_COUNT`] exists to close,
    /// and a zero or negative count would silently mean "do nothing" rather
    /// than "do it once".
    pub(crate) fn count(&self) -> u32 {
        self.int("count", 1).clamp(1, MAX_ACTION_COUNT) as u32
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Parameters in insertion order, for the introspector.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, i64)> + '_ {
        self.0.iter().map(|(k, v)| (k.as_str(), *v))
    }
}

/// Whether `name` is a well-formed action id.
///
/// The dot is load-bearing, not cosmetic: the host bridge splits its own
/// namespace from Godot's editor-shortcut namespace on exactly this
/// character, so an id without one would be ambiguous with a shortcut path.
pub(crate) fn is_valid_action_id(name: &str) -> bool {
    name.contains('.')
        && !name.contains('/')
        && !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

/// Interner mapping action names to [`ActionId`]s.
///
/// Wraps vim-core's `NameRegistry`, which is append-only and idempotent, so
/// an id stays stable for the life of the process even across config reloads.
#[derive(Debug, Default)]
pub(crate) struct ActionNames {
    inner: vim_core::keymap::NameRegistry,
}

#[allow(dead_code, reason = "name_of feeds the introspector in P6")]
impl ActionNames {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Intern `name`, returning its stable id.
    ///
    /// Idempotent: registering the same name twice yields the same id.
    ///
    /// # Panics (debug only)
    /// If `name` is not a valid action id. Registration is a compile-time-ish
    /// event driven by `const` provider tables, so a malformed id is a
    /// programming error, not user input.
    pub(crate) fn intern(&mut self, name: &str) -> ActionId {
        debug_assert!(
            is_valid_action_id(name),
            "action id '{name}' must be dotted, slash-free and alphanumeric"
        );
        ActionId(self.inner.register(name))
    }

    pub(crate) fn name_of(&self, id: ActionId) -> Option<&str> {
        self.inner.get_name(id.0)
    }

    pub(crate) fn id_of(&self, name: &str) -> Option<ActionId> {
        self.inner.get_id(name).map(ActionId)
    }
}

/// Where a binding sends a keystroke.
///
/// `Native` is the give-back: it means "this key is not mine", and is the one
/// target legal at every trust tier because it can only *reduce* what the
/// plugin consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code, reason = "Native/Shortcut gain their consumers in P5/P9")]
pub(crate) enum RuleTarget {
    /// Run one of the plugin's own registered actions.
    Action(ActionId),
    /// Hand the key back to Godot untouched.
    Native,
    /// Delegate to one of Godot's own registered editor shortcuts.
    Shortcut(CompactString),
}

/// A named verb: what it is called, what it needs, and what it does.
///
/// Values are `static`s. `Caps` combinators are `const fn` in bitflags 2, and
/// a non-capturing closure coerces to a `fn` pointer in a static initializer,
/// so `run: |cx| { … }` is legal there.
#[allow(
    dead_code,
    reason = "desc feeds the introspector in P6; host_invocable gates the host bridge in P3"
)]
pub(crate) struct ActionSpec {
    pub(crate) id: &'static str,
    pub(crate) desc: &'static str,
    /// Consulted only when the binding resolver ranks candidates for a
    /// keystroke. Host-originated invocation does not consult it — `:action`
    /// names the verb directly, so there is no candidate list to rank.
    pub(crate) requires: Caps,
    /// False ⇒ a host request fails loudly ("requires panel focus") rather
    /// than declining invisibly. `godotvim.fs.*` can locate their own target
    /// and are true; `godotvim.item.*` need a focused control and are false.
    pub(crate) host_invocable: bool,
    pub(crate) run: fn(&mut ActionCtx<'_>) -> Outcome,
}

impl std::fmt::Debug for ActionSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionSpec")
            .field("id", &self.id)
            .field("requires", &self.requires)
            .finish_non_exhaustive()
    }
}

/// What an action can reach while it runs.
///
/// Ships **chain-less** at this phase: the sampled focus chain arrives with
/// the surface plane, and adding it later does not change this signature.
/// `target` is `Option` because the mandatory no-focus-owner case — cross-panel
/// navigation with nothing focused, which must still consume — would otherwise
/// be unconstructible.
pub(crate) struct ActionCtx<'a> {
    /// The focused control, when there is one.
    target: Option<Gd<Control>>,
    pub(crate) params: Params,
    /// Records side effects instead of performing them, under test.
    pub(crate) recorder: Option<&'a mut Vec<Effect>>,
}

/// One observable side effect of running an action.
///
/// Exists so a test can assert what an action *did* without a Godot runtime.
/// The `ItemList` activation path is the reason: it must emit **both**
/// `item_selected` and `item_activated`, because different editor docks
/// listen to different ones, and a "verbatim move" can silently halve that.
#[allow(
    dead_code,
    reason = "GrabFocus is recorded once P4 moves the focus executors"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Effect {
    GrabFocus,
    EmitSignal {
        name: &'static str,
        arg: Option<i64>,
    },
}

#[allow(
    dead_code,
    reason = "defer_grab_focus adopts the remaining executors in P4"
)]
impl<'a> ActionCtx<'a> {
    /// Capabilities are deliberately NOT a field: the gate lives on the
    /// binding path (see [`ActionRegistry::caps_allow`]), so by the time a
    /// context exists the decision is made. Carrying them here would invite a
    /// `run` body to re-derive — or rewrite — its own gate result.
    pub(crate) fn new(target: Option<Gd<Control>>, params: Params) -> Self {
        Self {
            target,
            params,
            recorder: None,
        }
    }

    /// A context that records side effects rather than performing them.
    #[cfg(test)]
    pub(crate) fn recording(sink: &'a mut Vec<Effect>) -> Self {
        Self {
            target: None,
            params: Params::new(),
            recorder: Some(sink),
        }
    }

    pub(crate) fn target(&self) -> Option<&Gd<Control>> {
        self.target.as_ref()
    }

    /// Emit a Godot signal, or record the intent under test.
    pub(crate) fn emit(&mut self, name: &'static str, arg: Option<i64>) {
        if let Some(rec) = self.recorder.as_deref_mut() {
            rec.push(Effect::EmitSignal { name, arg });
            return;
        }
        let Some(target) = self.target.as_mut() else {
            return;
        };
        match arg {
            Some(v) => target.emit_signal(name, &[Variant::from(v as i32)]),
            None => target.emit_signal(name, &[]),
        };
    }

    /// Move focus, deferred.
    ///
    /// Deferred because an immediate `grab_focus()` during input processing is
    /// swallowed by Godot's event dispatch loop. Single home for what were
    /// four identical `call_deferred("grab_focus", &[])` copies.
    pub(crate) fn defer_grab_focus(&mut self, target: &Gd<Control>) {
        if let Some(rec) = self.recorder.as_deref_mut() {
            rec.push(Effect::GrabFocus);
            return;
        }
        target
            .clone()
            .upcast::<godot::classes::Node>()
            .call_deferred("grab_focus", &[]);
    }
}

/// Every action the shell knows about, by id.
///
/// Registration is driven by `const` provider tables rather than link-time
/// magic: a `cdylib` that Godot dlopens and hot-reloads under `lto = "fat"` is
/// the wrong place for life-before-main constructors, and a plain array is
/// compile-time checked and reviewable in a diff.
#[derive(Debug, Default)]
pub(crate) struct ActionRegistry {
    names: ActionNames,
    specs: Vec<&'static ActionSpec>,
}

#[allow(dead_code, reason = "iter/len/name_of feed the introspector in P6")]
impl ActionRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Register a spec, returning its id. Idempotent by name.
    pub(crate) fn register(&mut self, spec: &'static ActionSpec) -> ActionId {
        let id = self.names.intern(spec.id);
        let idx = id.0 as usize;
        if idx == self.specs.len() {
            self.specs.push(spec);
        } else {
            debug_assert_eq!(
                self.specs[idx].id, spec.id,
                "action id {idx} reused for a different spec"
            );
        }
        id
    }

    pub(crate) fn get(&self, id: ActionId) -> Option<&'static ActionSpec> {
        self.specs.get(id.0 as usize).copied()
    }

    pub(crate) fn id_of(&self, name: &str) -> Option<ActionId> {
        self.names.id_of(name)
    }

    pub(crate) fn name_of(&self, id: ActionId) -> Option<&str> {
        self.names.name_of(id)
    }

    pub(crate) fn len(&self) -> usize {
        self.specs.len()
    }

    /// Every registered action, for the introspector.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (ActionId, &'static ActionSpec)> + '_ {
        self.specs
            .iter()
            .enumerate()
            .map(|(i, s)| (ActionId(i as u32), *s))
    }

    /// Run an action by id.
    ///
    /// **Deliberately does not consult `requires`.** `Caps` gates *bindings*,
    /// never invocation: the resolver filters candidates during the forest
    /// walk, so by the time an action runs the decision is made. That rule is
    /// what keeps `:action godotvim.fs.refresh` working from the command
    /// line, where there is no keystroke, no surface and no sampled widget to
    /// derive capabilities from. Actions that genuinely need a widget
    /// re-assert it in their own body.
    ///
    /// Call [`Self::caps_allow`] first on the binding path.
    pub(crate) fn run(&self, id: ActionId, cx: &mut ActionCtx<'_>) -> Outcome {
        let Some(spec) = self.get(id) else {
            log::warn!("run: unknown action id {}", id.0);
            return Outcome::Declined;
        };
        (spec.run)(cx)
    }

    /// Whether `caps` satisfies what `id` requires — the binding-plane gate.
    ///
    /// A miss is a **declination**: it is how `h`/`l` go inert on a list with
    /// no hierarchy without the dispatcher naming a widget class.
    pub(crate) fn caps_allow(&self, id: ActionId, caps: Caps) -> bool {
        let Some(spec) = self.get(id) else {
            return false;
        };
        let ok = caps.satisfies(spec.requires);
        if !ok {
            log::trace!(
                "caps: {} skipped — needs {:?}, target offers {:?}",
                spec.id,
                spec.requires,
                caps
            );
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Params ───────────────────────────────────────────────────────

    #[test]
    fn an_absent_parameter_yields_its_default() {
        assert_eq!(Params::new().int("count", 7), 7);
        assert!(Params::new().is_empty());
    }

    #[test]
    fn set_then_read_round_trips() {
        let mut p = Params::new();
        p.set_int("count", 5);
        assert_eq!(p.int("count", 1), 5);
        assert!(!p.is_empty());
    }

    #[test]
    fn setting_the_same_key_twice_replaces_rather_than_appends() {
        let mut p = Params::new();
        p.set_int("count", 3);
        p.set_int("count", 9);
        assert_eq!(p.int("count", 1), 9);
        assert_eq!(p.iter().count(), 1, "last writer wins, no duplicate key");
    }

    #[test]
    fn count_defaults_to_one() {
        assert_eq!(Params::new().count(), 1);
    }

    #[test]
    fn count_is_clamped_at_both_ends() {
        // The upper clamp is the freeze guard: find_navigable_target walks up
        // to 1000 items per call, so an unbounded count hangs the editor.
        let mut p = Params::new();
        p.set_int("count", 10_000);
        assert_eq!(p.count(), MAX_ACTION_COUNT as u32);

        // Zero or negative must mean "once", not "never" and not a panic on
        // the `as u32` cast.
        for v in [0, -1, i64::MIN] {
            let mut p = Params::new();
            p.set_int("count", v);
            assert_eq!(p.count(), 1, "count={v} should clamp to 1");
        }
        let mut p = Params::new();
        p.set_int("count", i64::MAX);
        assert_eq!(p.count(), MAX_ACTION_COUNT as u32);
    }

    #[test]
    fn iter_preserves_insertion_order_for_the_introspector() {
        let mut p = Params::new();
        p.set_int("count", 1);
        p.set_int("depth", 2);
        assert_eq!(
            p.iter().collect::<Vec<_>>(),
            vec![("count", 1), ("depth", 2)]
        );
    }

    // ── Action ids ───────────────────────────────────────────────────

    #[test]
    fn well_formed_ids_are_accepted() {
        for name in [
            "godotvim.focus.left",
            "godotvim.fs.create",
            "godotvim.item.next",
            "a.b",
            "plugin.some_action",
        ] {
            assert!(is_valid_action_id(name), "{name} should be valid");
        }
    }

    #[test]
    fn ids_without_a_dot_are_rejected() {
        // The host bridge splits its namespace from Godot's shortcut paths on
        // the dot, so a bare name would be ambiguous.
        for name in ["", "focus", "godotvim"] {
            assert!(!is_valid_action_id(name), "{name} should be invalid");
        }
    }

    #[test]
    fn ids_containing_a_slash_are_rejected() {
        // `filesystem_dock/delete` is a Godot editor-shortcut path, not an
        // action id; accepting it would make the two namespaces collide.
        assert!(!is_valid_action_id("filesystem_dock/delete"));
        assert!(!is_valid_action_id("godotvim.fs/create"));
    }

    #[test]
    fn ids_with_exotic_characters_are_rejected() {
        for name in ["godotvim.fs create", "godotvim.fs-create", "godotvim.fs!"] {
            assert!(!is_valid_action_id(name), "{name} should be invalid");
        }
    }

    // ── Interning ────────────────────────────────────────────────────

    #[test]
    fn interning_is_idempotent() {
        let mut names = ActionNames::new();
        let a = names.intern("godotvim.focus.left");
        let b = names.intern("godotvim.focus.left");
        assert_eq!(a, b, "the same name must always yield the same id");
    }

    #[test]
    fn distinct_names_get_distinct_ids() {
        let mut names = ActionNames::new();
        let a = names.intern("godotvim.focus.left");
        let b = names.intern("godotvim.focus.right");
        assert_ne!(a, b);
    }

    #[test]
    fn interning_round_trips_both_ways() {
        let mut names = ActionNames::new();
        let id = names.intern("godotvim.fs.create");
        assert_eq!(names.name_of(id), Some("godotvim.fs.create"));
        assert_eq!(names.id_of("godotvim.fs.create"), Some(id));
    }

    #[test]
    fn an_unregistered_name_has_no_id() {
        let names = ActionNames::new();
        assert_eq!(names.id_of("godotvim.nope"), None);
        assert_eq!(names.name_of(ActionId(0)), None);
    }

    #[test]
    fn an_id_becomes_a_pseudo_key_carrying_itself() {
        // This is what lets an action be a mapping right-hand side:
        // `nnoremap <leader>ff <Action>(godotvim.fs.create)`.
        let id = ActionId(42);
        assert_eq!(id.as_key().key(), vim_core::keymap::Key::Action(42));
    }
}
