# Rebindable Shell-Side Navigation — Design

> **Status:** design proposal, not yet implemented. Produced by a 38-agent competitive
> design review (5 architectures → adversarial critique → rebuttal → judge panel vote →
> synthesis → verification → remediation → re-verification).
>
> **Target:** godot-vim v1.6.1+ against vim-core **v0.7.1** (pinned, not forked) and
> Godot 4.5+/4.8-dev.

## Contents

1. [Problem & Constraints](#1-problem--constraints)
2. [The Core Insight](#2-the-core-insight)
3. [Architecture](#3-architecture)
4. [Types & APIs](#4-types--apis)
5. [Dispatch Model](#5-dispatch-model)
6. [Config Syntax](#6-config-syntax)
7. [Extensibility](#7-extensibility)
8. [vim-core Decision](#8-vim-core-decision)
9. [Rejected Alternatives](#9-rejected-alternatives)
10. [Implementation Phases](#10-implementation-phases)
11. [Test Strategy](#11-test-strategy)
12. [Migration & Compatibility](#12-migration--compatibility)
13. [Known Limitations & Open Questions](#13-known-limitations--open-questions)

---

## 1. Problem & Constraints

### 1.1 The keyset is fixed, and the code says so

`src/navigation/mod.rs:9-11` states the situation without euphemism:

```rust
//! Entirely shell-side — vim-core has no knowledge of Godot's dock layout.
//! Uses a fixed keyset (not user-customizable via `:map`) to keep the
//! focus-management boundary simple and predictable.
```

Everything the plugin does outside an attached `CodeEdit` — every panel jump, every dock motion, every FileSystem operation — is a `Key::` literal compiled into a `match` arm. A user who wants `<C-w>h` instead of `<C-h>`, or `n` instead of `a` for "new file", has no config line to write and no command to run.

The inventory below is exhaustive. It was derived by grepping every `Key::[A-Z]` occurrence in `src/` outside `src/bridge/input.rs` (the Godot→vim-core scancode translation table, which is not a binding site) and outside `#[cfg(test)]` modules; the only other survivors are `src/controller/passthrough.rs:139-147` and doc comments in `src/bridge/godot_calls.rs:52,56`. There is no eighth site.

| # | Keys | Where the key is decided | Where the behaviour lives | Transport |
|---|---|---|---|---|
| 1 | `Ctrl+h/j/k/l` (cross-panel focus) | `plugin/input.rs:84-135`; scancode→direction at `navigation/window.rs:27-39` | `window::handle_window_nav`, `window.rs:48-127` | `input()` |
| 2 | `h/j/k/l` (intra-dock) | `navigation/dock.rs:71-86` (identity), `:87-122` (dispatch) | `dock_nav::handle_navigation` `:96-125`, `handle_hierarchy` `:127-136` | `input()` |
| 3 | `/`, `Enter`, `Esc` (intra-dock) | `navigation/dock.rs:148-156` | `handle_slash` `:162-171`, `handle_enter` `:178-202`, `handle_escape_from_dock` `:217-243` | `input()` |
| 4 | `Esc`, `Enter` (dock filter box) | `navigation/dock.rs:162-181` | same function; falls back to `handle_escape_from_dock()` at `:153` | `input()` |
| 5 | `a`, `d`, `r`, `y`, `R` (FileSystem) | `navigation/filesystem_explorer.rs:87-97`; identity at `:363-378` | `begin_create` `:125-134`, `begin_delete`, `begin_rename`, `yank_path` `:109-115`, `refresh` `:117-123` | `input()` |
| 6 | `Esc` (FS create/rename prompt) | `plugin/mod.rs:781-801` (`on_fs_prompt_gui_input`) | `FileSystemExplorer::dismiss_prompt` | the prompt LineEdit's own `gui_input` |
| 7 | `Ctrl+@`, `Ctrl+N`, `Ctrl+P`, `Up`, `Down`, `Tab`, `Enter`, `Esc`, `Backspace` (completion popup) | `controller/completion.rs:32-125`, `:139-165` | inline; calls `CodeEdit` completion API directly | the attached editor's `gui_input` |

Two entries need qualification.

**Focus cycling has no shell-side key at all.** `navigation/cycle.rs:52-100` implements spatial next/previous panel cycling, but nothing in the `input()` path can reach it. It is reachable only from inside the editor, through the engine: `Effect::WindowNext` → `CompoundAction::WindowNav { action: WindowNavAction::CycleNext }` (`effects/dispatch.rs:967-971`) → `navigation::handle_window_nav_action` (`cycle.rs:19-45`), driven from `controller/process.rs:135` and `controller/mod.rs:1140`. So `<C-w>w` cycles panels when a script is open and nothing cycles panels when one is not. That asymmetry is a symptom of the same defect, and §2 returns to it.

**Site 7 is in scope for naming but not for transport.** Completion routing is real hardcoded key handling, but it lives on `gui_input` for two structural reasons: `_input` is registered per-viewport (`input_group = "_vp_input" + id`, `scene/main/viewport.cpp:5578`), so it never fires for a script editor floated into its own `Window`; and `try_handle_completion` returns `Option<bool>` — `Some(false)` at `completion.rs:88` means *handled but deliberately not consumed*, so Godot's own arrow-key list navigation still runs. Moving it onto the `input()` registry would break both. It may become rebindable later, on its existing transport; it is not part of the shell-side dispatcher.

Also out of scope: `controller/passthrough.rs:139-147`, the host policy that hands F-keys and Meta combos back to the IDE from *inside* the editor. That seam is already user-configurable through the `passthrough_keys` EditorSetting (`settings/reader.rs:240-264`) and is a different question — which keys the attached `CodeEdit` declines, not which keys the shell claims.

### 1.2 The architectural knot

The obvious diagnosis is wrong, and it is worth killing before it costs anyone a week.

**It is not true that "no vim session exists when a dock has focus."** `VimController` is a two-phase enum:

```rust
// src/controller/mod.rs:93-95
pub(crate) enum ControllerPhase {
    Attached { session: VimSession<GodotHost> },
    Detached { engine: VimEngine, state: ShellState },
}
```

`engine()` and `engine_mut()` (`:157-169`) return a live `&VimEngine` in *both* arms — the comment at `:155` says so in the source: "Engine accessors (work in both attached and detached state)". `mode()` (`:484-486`) and `could_start_mapping()` (`:677-679`) are thin delegations to `engine()`, and `plugin/input.rs:110-117` already calls `could_start_mapping` from the global `input()` handler, on a keystroke that may have been typed with a dock focused. `self.controller` is `Some(..)` from `enter_tree` (`plugin/mod.rs:128`) to `exit_tree` (`:173`) and is never nulled in between, not even by `recover_controller_from_panic` (`:1015-1050`). The mapping trie is one field dereference away at every keystroke the plugin sees.

The real knot is that **five independent decisions are fused into a single match arm**, and the fusion is what makes the keyset unaddressable. `src/navigation/dock.rs:113-119`:

```rust
DockHjkl::Down => {
    if handle_navigation(&focused, NavDirection::Next, 0) {
        DockInputResult::Handled
    } else {
        DockInputResult::Ignored
    }
}
```

That expression decides which key, which widget kinds are eligible, what behaviour runs, whether the behaviour succeeded, and whether the event is consumed — all at once, in one place, with no intermediate term. §2 takes it apart. Three consequences of the fusion are visible in today's code:

- **There is no name to bind to.** `Key::H => WindowNavDirection::Left` (`window.rs:35`) is a wire from a scancode to a function argument. Nothing between the two could appear on the right-hand side of a config line.
- **Key identity is re-derived per site, three times, incompatibly.** `dock_hjkl` (`dock.rs:71-76`), `direction_from_hjkl` (`window.rs:23-29`) and `resolve_key` (`filesystem_explorer.rs:363-374`) each implement "logical keycode first, physical fallback" over a different key subset. The consequence is a live bug: for a keyboard layout where the QWERTY-J position emits a logical `/`, `dock_hjkl` matches on the *physical* fallback and returns at `dock.rs:112-145`, so the `Key::SLASH => handle_slash` arm at `:127` is never reached. `/` is unreachable, silently, on exactly the layouts the fallback was added to support.
- **Ordering is a hardcoded branch.** `plugin/input.rs:140-150` is a literal `if navigation::is_in_filesystem_dock(&control) { … fs first … } else { … }`. Adding a second specialised dock means adding a second `if`.

**Why a naive `HashMap<Key, Action>` fails: it cuts at the wrong joint.** It separates decision (1) from decisions (2)-(5) and leaves (2)-(5) welded together. Every one of the following then survives the refactor:

- Actions still know about widgets — `godotvim.item.collapse` must itself test `is_class("Tree")`, so widget taxonomy simply migrates from the dispatcher into the action bodies.
- Actions still decide consumption — the map has no way to say "this key is bound, the action ran, and the event should still reach Godot", so either the tri-state at `dock.rs:26-49` is flattened to `bool` (regression) or every action returns a consumption verdict the user cannot influence.
- A registered key is consumed whether or not the action did anything. `Tree::allow_search` defaults to `true` (`godot/scene/gui/tree.h:792`), and `_input` runs strictly before `gui_input` (`godot/scene/main/viewport.cpp:3544-3546`, with the in-source comment "must happen before GUI, order is `_input` -> gui input -> `_unhandled input`"). There is no replay channel out of `_input()`: a consumed key is destroyed. A table that consumes on every registered key therefore deletes Tree type-to-search and arrow navigation (both `Tree::gui_input`) and the docks' F2/Delete `ED_SHORTCUT` accelerators (`shortcut_input`) for every key it names.
- A new dock still needs new arms, because "which widgets does `godotvim.item.expand` apply to" has no representation. This is Helix #5505 reproduced: a flat keymap plus per-widget conditionals inside the commands.

The map solves the *addressing* problem and leaves the *composition* problem exactly where it was.

### 1.3 Hard constraints

Every row is a MUST. Each is transcribed from behaviour that exists today; the design is not permitted to change any of them, and several of them individually kill otherwise-attractive architectures.

| # | Constraint | Evidence | What it forces |
|---|---|---|---|
| C1 | `Ctrl+hjkl` is consumed **unconditionally** from Dock, SearchBox and Unknown contexts — including when `gui_get_focus_owner()` returns `None`, where the navigation call is skipped and `set_input_as_handled()` still fires | `plugin/input.rs:120-122`, `:126-134` | A consumption policy decoupled from the action's verdict, and a target type that can be absent |
| C2 | The `could_start_mapping` escape hatch survives verbatim: `:nnoremap <C-h> x` beats panel navigation, and *no controller* still means *intercept* (`is_none_or`) | `plugin/input.rs:106-118`; `controller/mod.rs:677-679` → `vim-core/src/execution/engine/mapping.rs:78-82` | The predicate is exactly `could_start_mapping` — **not** `could_start_mapping ∨ KeyInterestSet`. `KeyEvent::ctrl('h')` and `ctrl('j')` are `KeyClass::Motion` in `vim-core/src/keymap/core.rs:121-122` and land in the normal, visual and operator-pending interest sets; `ctrl('k')`/`ctrl('l')` appear nowhere in `CORE_KEYMAP`. The disjunct silently kills two of four keys |
| C3 | Mode awareness in the editor, with **Select excluded** — Normal, Visual, OperatorPending intercept; Insert, Replace, VirtualReplace, CommandLine and Select do not | `plugin/input.rs:91-105`, with the rationale comment at `:100-102` | An editor-mode partition that is total by construction, since `vim_core::primitives::Mode` is `#[non_exhaustive]` |
| C4 | **Foreign is a hard stop.** A non-attached `CodeEdit`, a plain `TextEdit`, or a `LineEdit` with no sibling nav control: nothing is intercepted, ever | `plugin/input.rs:90`; `navigation/focus.rs:50-57`, `:73-87` | A terminating verdict evaluated before any lookup — never an emergent property of an empty binding set |
| C5 | **FileSystem-first ordering.** In the FS dock, `a/d/r/y/R` get first refusal; if the FS handler declines, generic dock handling still runs | `plugin/input.rs:140-150` | Specificity must be a declared, total, machine-checkable relation. Godot's scene depth **inverts** it: `dock` is derived from the focus owner, `dock.filesystem` from an ancestor via `is_ancestor_of` (`filesystem_explorer.rs:380-386`) |
| C6 | **Prompt exemption.** While the plugin's own FS create/rename `LineEdit` has focus, the `input()` path declines everything so that typing and `text_submitted` work; `Esc` is handled on the prompt's own `gui_input` | `plugin/input.rs:156-159`; `plugin/mod.rs:781-801` | Bare keys must be able to stop at a surface and fall through to the control, while modifier-bearing keys keep climbing. Both transports stay live: a floating script editor reparents into a separate `Window` (`godot/editor/gui/window_wrapper.cpp:74-79`) where per-viewport `_input` never fires |
| C7 | Logical keycode first, **physical fallback second**, for non-Latin layouts | `window.rs:23-29`, `dock.rs:71-76`, `filesystem_explorer.rs:363-374` | Exactly one ordered probe applied to the whole key, once — not per match arm. Per-arm evaluation is what produces the `/`-shadowing bug in §1.2 and, generalised naively, fires `yank_path` when a QWERTZ user presses `z` |
| C8 | `set_input_as_handled()` on the **transport's own viewport** | `plugin/input.rs:57-62` (EditorInterface base control) vs `:279-280` (`editor.get_viewport()`) | Viewport selection is a transport property, never a dispatcher one. They differ for floating script editors |
| C9 | Focus changes are **deferred** — immediate `grab_focus()` during input processing is swallowed by Godot's dispatch loop | `dock.rs:228-235`; also `window.rs:121`, `cycle.rs:99`, `filesystem_explorer.rs:267` | One helper, four verbatim call sites preserved |
| C10 | The **tri-state** result survives: `Handled` / `FocusChanged` / `Declined`, where `Declined` means "not consumed, Godot proceeds" | `dock.rs:20-61`; `:90-101` (end of list), `:220-222` (ItemList with nothing selected), `:200` (Enter on RichTextLabel), `:168-170` (no search box), `:219-225` (no script editor) | Declination is a primitive of the model, not an error path (§2) |
| C11 | Failures degrade, never crash: every Godot entry point runs inside `panic_guard`, and malformed config warns-and-skips per token | `plugin/mod.rs:111-112`; `settings/reader.rs:238-264` | Registration and parse errors reject one line and keep the rest. Note `panic_guard` protects against panics only — it cannot detect a livelock |
| C12 | **Zero-config guarantee.** With no `.godot-vimrc` present, and with `ProjectVimrc::Disabled` set, today's keyset works byte-identically | `config/path.rs:23-59`; `config/sandbox.rs:378-381` | Defaults live in provider code, never in the user's file, and never behind the config-load path |
| C13 | vim-core stays pinned at `tag = "v0.7.1"` | `Cargo.toml:19` (`godot` is `v0.4.5` at `:18`) | Whatever is consumed must already be `pub` on the checked-out tag |
| C14 | Multi-key **grammar** prefixes stay unconsumable. `<C-w>` is `KeyClass::Action` in `CORE_KEYMAP` (`vim-core/src/keymap/core.rs:208-213`) and drives a 19-command family; `<C-\>` is intercepted before classification and is absent from `CORE_KEYMAP` entirely | `vim-core/src/grammar/parser.rs:129-133`; `grammar/handlers/ready.rs:141-150`; `Keymap::lookup` never consults `CORE_KEYMAP` (`keymap/keymap.rs:605-618`) | `could_start_mapping` provably cannot see this, so it is answered at **registration** time by `vim_core::grammar::Parser::process(key, &Keymap::new(), mode).is_pending()` — vim-core's own state machine, in a layer forbidden from importing `execution` (`grammar/mod.rs:9-11`) |
| C15 | The dispatch core must be testable headless. `src/navigation/` contains **zero** `#[cfg(test)]` modules today | `grep -rn "cfg(test)" src/navigation/` → 0; contrast `src/testing/mock_text_edit.rs` and the pure `translate_key` suites in `src/bridge/input.rs` | Exactly one Godot↔Rust sampling seam; everything above it is a pure function of plain data |

**A foreign-`CodeEdit` trap looks real from a static read of `classify_focus`, and is not.** `is_navigable_control` admits any `CodeEdit` (`scene_tree.rs:30-36`) and `classify_focus` maps a non-attached one to `Foreign` (`focus.rs:56-57`), from which `input.rs:90` refuses to intercept — so the static reading says "navigate in, never out." It self-heals. The plugin auto-attaches to **any** `CodeEdit` that takes focus: `lifecycle.rs:91` connects the base viewport's `gui_focus_changed` to `on_focus_changed` (`plugin/mod.rs:235-251`), which calls `discovery::find_code_edit_from_control` (`discovery.rs:21-23`) → `find_descendant::<CodeEdit>`, and `find_descendant` **tests the root node itself before recursing** (`scene_tree.rs`), so a focused `CodeEdit` matches itself and is attached. One deferred hop later it classifies as `Editor`, not `Foreign`, and `Ctrl+hjkl` works. This is deliberate, not accidental: `discovery.rs:11-12` says the match is intentionally broad, and `README.md:159` documents Vim-in-shader/addon-editors as *"Intentional."* Excluding non-attached `CodeEdit`s would therefore be a **regression**, not a fix — it would make Godot's shader editor unreachable by `Ctrl+hjkl` (`ShaderTextEditor : CodeEditorBase` and `CodeTextEditor` owns a real `CodeEdit *text_editor`, `godot/editor/gui/code_editor.h:93`).

The one genuine one-way trap is elsewhere and is **not** fixed here: `handle_escape_from_dock` (`dock.rs:255-266`) falls back to focusing a plain `TextEdit`, which is unconditionally `Foreign` (`focus.rs:85-87`) and which `find_code_edit_from_control` can never attach to — so there is no auto-attach rescue and no keyboard escape. It requires a `ScriptEditorBase` exposing a `TextEdit` but no `CodeEdit`, which stock Godot 4 does not produce, so it is carried as a low-probability known issue with a named red test in P0.

Correcting one claim that circulated during design: a focused `GraphEdit` is **not** a one-way trap. It classifies as `FocusContext::Unknown`, and `Unknown => true` at `input.rs:120-122` already lets `Ctrl+hjkl` escape. What is missing there is intra-widget navigation, which is a feature request, not a bug.

## 2. The Core Insight

> **Declination is not an error path. It is the composition operator — and it is what a keymap for a host UI fundamentally *is*.**

### 2.1 The wall every "keys map to actions" model hits

`src/navigation/dock.rs` returns `DockInputResult::Ignored` in five places that are not failures:

| Site | Situation | Why `Ignored` is correct |
|---|---|---|
| `:90-95` | `j` at the end of a list | The `Tree` should keep the key — incremental type-to-search is live (`allow_search` defaults `true`) |
| `:196-198` | `Enter` on an `ItemList` with nothing selected | There is nothing to activate; Godot may still want it |
| `:200` | `Enter` on a `RichTextLabel` | Not an activatable surface |
| `:168-170` | `/` where the dock has no filter box | `handle_slash` has nothing to focus |
| `:219-225` | `Esc` with no script editor open | There is nowhere to return focus to |

These are the plugin's entire vocabulary for coexisting with an editor that already has behaviour on the same keys. Because `_input` precedes `gui_input` (`viewport.cpp:3544-3546`) and precedes `shortcut_input` and Godot's whole `shortcut_context` machinery, a matching rule *always* wins and there is no replay channel. "I decline" is the only word in the language that lets Godot's own behaviour survive. A registry that consumes on every registered key is not a keymap; it is a wall.

Make declination first-class and the rest of the architecture is forced.

### 2.2 Cutting the fused arm

Here is the arm again, with the key identity that feeds it:

```rust
// src/navigation/dock.rs:78-85
fn hjkl_to_dock(key: Key) -> Option<DockHjkl> {
    match key {
        Key::J => Some(DockHjkl::Down),
        ...
```
```rust
// src/navigation/dock.rs:111-126
if let Some(direction) = dock_hjkl(key_event) {
    return match direction {
        DockHjkl::Down => {
            if handle_navigation(&focused, NavDirection::Next, 0) {
                DockInputResult::Handled
            } else {
                DockInputResult::Ignored
            }
        }
        ...
```

Five decisions, one expression. Only the first is the user's business:

| | Decision | Fused location today | New home |
|---|---|---|---|
| 1 | **which key** | `hjkl_to_dock`, `dock.rs:78-85` | a `vim_core::keymap::MappingTrie` the user owns, one per surface — key → *name* |
| 2 | **which widgets are eligible** | `matches!(dock_kind, DockKind::Tree)`, `dock.rs:128,112` | `ActionSpec::requires: Caps`, a subset test evaluated in `resolve.rs` |
| 3 | **what behaviour runs** | `handle_navigation(&focused, …)` | `run: fn(&mut ActionCtx<'_>) -> Outcome` — one signature for every verb in the plugin |
| 4 | **did it succeed** | the `bool` return, collapsed inline | `Outcome::{Handled, FocusChanged, Declined}` — a value, with a right of declination |
| 5 | **is the event consumed** | `Handled` vs `Ignored`, chosen by the executor | a declared `Consumption` policy applied *downstream* of (4), on the transport's own viewport |

Two properties of decision (2) matter enough to state here, because getting either wrong re-imports the widget knowledge one abstraction layer down.

**The capability must name an affordance, not a widget class.** Today `j`/`k` have *no* widget gate at all: `dock.rs:111-126` calls `handle_navigation` unconditionally, and `handle_navigation` has exactly three arms — `Tree` (move selection), `ItemList` (move selection), `RichTextLabel` (scroll 50px and return `true`) — at `dock_nav.rs:105-119` and `:284-298`. The tri-state return is the only gate there is. So the capability `godotvim.item.next` actually needs is *"answers vertical next/prev"*, one bit, held by all three classes: `Caps::VNAV`. Name it `LIST` after the widget taxonomy instead and a plain subset test cannot express "list **or** scrollable", and `j`/`k` silently stop working on the docs panel and the Output log — both focusable `RichTextLabel`s (`godot/editor/doc/editor_help.cpp:3490,3521`; `godot/editor/editor_log.cpp:519`), and the Output log is reachable through a shipped ex-command (`host/custom_commands.rs:70-84`). The surviving vocabulary is five affordances — `VNAV`, `HIERARCHY`, `ACTIVATE`, `TEXTENTRY`, `FILEOPS` — each traceable to one line of today's dispatch. `HIERARCHY` is what replaces `dock.rs:128,112`, and it is a strictly cleaner statement of a gate that is already redundant: `handle_hierarchy` returns `false` for every non-`Tree` class (`dock_nav.rs:127-136`).

**The gate lives in resolution, not in execution.** `Caps` filters *candidate bindings* during the forest walk; `registry.run(id, &mut ctx)` never consults `requires`. That single rule is what keeps `:action godotvim.fs.refresh` working from the command line, where there is no keystroke, no surface and no sampled widget. Actions that genuinely need a widget guard it themselves — `godotvim.fs.*` opens with the same `is_in_filesystem_dock` predicate the `dock.filesystem` probe uses (`filesystem_explorer.rs:380-386`), which is a no-op on the key path and the real guard on the invocation path.

### 2.3 Three special cases become one mechanism

| Today | Mechanism today | Under this design |
|---|---|---|
| FileSystem-first refusal | `if navigation::is_in_filesystem_dock(&control) { … } else { … }`, `plugin/input.rs:140-150` | `dock.filesystem` is a declared child of `dock`. `a` resolves at the child; `j` finds nothing there and the walk continues to the parent. No branch |
| `DockKind::Tree` gating `h`/`l` | `matches!(dock_kind, DockKind::Tree) && handle_hierarchy(…)`, `dock.rs:128,112` | the binding requires `Caps::HIERARCHY`; an `ItemList` does not contribute it; the candidate is skipped and the walk continues. No widget name in the dispatcher |
| `j` at the end of a list | `if handle_navigation(…) { Handled } else { Ignored }`, `dock.rs:114-119` | the action returns `Declined`; the walk continues; nothing else matches; nothing is consumed. Godot's `Tree` sees the key |

All three now say the same sentence — *this candidate did not take the key; continue* — and the dispatcher stops being a decision tree. It becomes **a fold over an ordered candidate list, terminated by the first non-declination.** That is why there are no priority integers, no `is_in_filesystem_dock` branch and no `DockKind` anywhere in the dispatch path, and why adding a debugger panel edits no dispatcher file.

Declination is the composition operator, not a universal solvent, and the two exceptions are load-bearing because today's code is unconditional in exactly two places:

- **A barrier terminates before any lookup.** `Foreign` and insert-like editor modes return `Ignore` immediately (C4, C3). Treating "no capability match, therefore fall through to the parent surface" as the mechanism for the Foreign hard-stop is what eats `Ctrl+H` in a Project Settings `LineEdit`.
- **A `Void` consumption terminates after execution regardless of verdict.** `plugin/input.rs:126-134` discards `handle_window_nav`'s result and calls `set_input_as_handled()` even when there was no focus owner and no target. That is C1, and it is a property of the rule, not of the action.

A third distinction belongs to the same family: handing a key *back* to Godot is not the same as declining it. Declination continues the walk to ancestor surfaces; a give-back must stop the walk and consume nothing. They are different verbs and the type system has to say so.

### 2.4 The corollary — the editor path was remappable because the verb had a name

Both halves of cross-panel navigation end in the same function. Only one of them is bindable.

**From the editor**, `<C-w>h`:
`MappingTrie` → `vim_core::grammar::Parser` (`<C-w>` → `Continue(AwaitingWindowCommand)`, `grammar/handlers/ready.rs:141-150`) → `Effect::WindowMoveLeft` → `CompoundAction::WindowNav { action: WindowNavAction::MoveLeft }` (`effects/dispatch.rs:948-952`) → `navigation::handle_window_nav_action` (`cycle.rs:19-34`) → `window::handle_window_nav`.

**From a dock**, `Ctrl+H`:
`Key::H => Some(WindowNavDirection::Left)` (`window.rs:35`) → `navigation::handle_window_nav(&control, direction)` (`plugin/input.rs:126-129`).

Same executor. Same observable behaviour. The first is remappable and the second is not — and the reason is **not** that a session exists in one place and not the other. `ControllerPhase::Detached { engine: VimEngine, .. }` proves the engine is one field away in both (`controller/mod.rs:93-95`, `:157-169`), and `input.rs:110-117` already queries it on a keystroke typed with a dock focused. The reason is that the editor path passes through `Effect::WindowMoveLeft` — a *named* intermediate term that a trie payload, an ex-command, and a config line can all address — while the dock path is a direct wire from a scancode to a function argument, with no term in between for anything to name.

The knot was never about where the session lives. It was about whether the verb has a name.

That is the whole change. Once `godotvim.focus.left` exists as a name, decision (1) becomes data the user owns, decision (2) becomes an affordance the widget contributes, decision (3) becomes one uniform signature, decision (4) becomes a value with a right of refusal, and decision (5) becomes a policy the transport applies. Three of the five were never the user's business. Naming the verb is what lets us hand over the one that is.

---

## 3. Architecture

### 3.1 The five fused decisions, and where each one goes

One line of shipped code carries the whole problem. `src/navigation/dock.rs:111-126`:

```rust
if let Some(direction) = dock_hjkl(key_event) {
    return match direction {
        DockHjkl::Down => {
            if handle_navigation(&focused, NavDirection::Next, 0) {
                DockInputResult::Handled
            } else {
                DockInputResult::Ignored
            }
        }
        // …
        DockHjkl::Left => {
            if matches!(dock_kind, DockKind::Tree)
                && handle_hierarchy(&focused, HierarchyAction::Collapse)
            { DockInputResult::Handled } else { DockInputResult::Ignored }
        }
```

That expression decides five independent things at once: **(1)** which physical key, **(2)** which widget kind is eligible (`dock_kind`), **(3)** which behaviour runs, **(4)** whether the behaviour succeeded, **(5)** whether the event is consumed. Only (1) is the user's business, and a `HashMap<Key, Action>` separates (1) from (2..5) while leaving (2..5) welded together — which is why a new dock still needs new match arms.

The architecture is one mechanism per decision, and each mechanism lives in exactly one module:

| # | Decision | Mechanism | Module |
|---|---|---|---|
| 1 | which key | `MappingTrie` per surface, keyed by canonicalized `KeyEvent` sequences, owned by the user's `.godot-vimrc` | `src/actions/bind.rs`, `src/actions/keys.rs` |
| 2 | eligibility | `Caps` bitset — the widget contributes affordances, the surface path augments them, the rule's `requires` is a subset test | `src/actions/caps.rs` |
| 3 | behaviour | `ActionSpec { id, requires, run: fn(&mut ActionCtx<'_>) -> Outcome }` — one signature for every verb in the plugin | `src/actions/action.rs` + `src/actions/providers/` |
| 4 | success | `Outcome::{Handled, FocusChanged, Declined}`, a first-class value with a right of declination | `src/actions/outcome.rs` |
| 5 | consumption | `Consumption::{Elastic, Void}`, computed downstream of (4), applied by the transport on the transport's own viewport | `src/actions/resolve.rs` → `src/actions/dispatch.rs` |

Decision (4) is the load-bearing one. `dock.rs:120-126` returns `Declined` when `j` is pressed at the end of an `ItemList`, and `dock.rs:220-222` returns `Declined` when Enter is pressed with nothing selected (the guard is `get_selected_items()`, so a populated list with no selection declines too). Those are not failures — they are how the plugin composes with Godot, whose `Tree` already implements incremental type-to-search (`allow_search` defaults true) and arrow keys, and above which the docks layer F2/Delete via `ED_SHORTCUT` (`godot/editor/docks/filesystem_dock.cpp:4466`) — F2 is not `Tree` behaviour; `scene/gui/tree.cpp` never mentions it. `Declined` is the vocabulary word that keeps that behaviour alive, and it is what turns the dispatcher from a decision tree into a fold (§3.6).

### 3.2 Module map

```
src/actions/
├── mod.rs           re-exports; `pub(crate) fn resolve`
├── plane.rs         ActionPlane: registry + index + forest + diagnostics + generation
├── outcome.rs       Outcome, Disposition
├── caps.rs          Caps bitflags, widget_caps()
├── surface.rs       ChainNode, FocusChain, Anchor, Seal, SurfaceSpec, SurfacePath
├── action.rs        ActionId, Params, ActionCtx, ActionSpec, ActionRegistry
├── keys.rs          KeyProbes, canonicalize, validate_lhs_key, parse_lhs,
│                    starts_vim_grammar_sequence
├── bind.rs          Rule, RuleTarget, SlotId, BindingIndex, Registrar, Provenance,
│                    RuleReject
├── resolve.rs       ResolveInput, Resolution, resolve()  — zero Gd<T>
├── dispatch.rs      the execution fold + Consumption; the only file that holds
│                    `&mut GodotVimCore` and a viewport at the same time
├── inject.rs        ShortcutInjector (re-entrancy guard for delegated shortcuts)
├── introspect.rs    :panelmap / :panelmap <lhs> / :checkhealth godotvim
└── providers/
    ├── mod.rs       const PROVIDERS: &[fn(&mut Registrar<'_>)] — listed below in
    │                array order, which IS probe order (§3.3)
    ├── editor.rs    editor.nav, editor.insert
    ├── prompt.rs    prompt + godotvim.prompt.dismiss
    ├── searchbox.rs searchbox + godotvim.search.{accept,cancel}
    ├── filesystem.rs dock.filesystem + godotvim.fs.*
    ├── dock.rs      dock + godotvim.item.* + godotvim.dock.search + focus.editor
    ├── foreign.rs   foreign
    ├── unknown.rs   unknown
    └── panel.rs     panel + focus.* + cycle.*
src/config/panelmap.rs   the `panelmap` / `panelunmap` line parser (§6)
```

`src/actions/` replaces the *binding* logic of `src/navigation/`. It does not replace the executors. `window::handle_window_nav`, `cycle::handle_cycle_focus`, `dock_nav::{handle_navigation, handle_hierarchy}`, `dock::{handle_slash, handle_enter, handle_escape_from_dock}`, `filesystem_explorer::{begin_create, begin_delete, begin_rename, yank_path, refresh}` and every `call_deferred("grab_focus")` site survive verbatim as `ActionSpec::run` bodies. Two mechanical consequences: `dock_nav::{handle_navigation, handle_hierarchy, NavDirection, HierarchyAction}` are `pub(super)` today and must be widened to `pub(crate)` and re-exported from `src/navigation/mod.rs:21-25`; `src/navigation/focus.rs::classify_focus` and `FocusContext` are deleted in the same commit that removes their last caller at `src/plugin/input.rs:76`, while `DockKind` **and its classifier `dock_kind_of`** move into `dock.rs` — `filesystem_explorer.rs` uses `DockKind` independently, and `ActionCtx::target_or` still needs a `Gd<Control>` → `DockKind` route.

### 3.3 The declared surface forest

A **surface** is a named place in the editor UI where bindings live. Surfaces are *declared* — each one names its parent in a forest that exists only in this design, is authored in provider code, and is validated at registration. Depth in that forest is the **only** specificity mechanism in the system.

```
FOREST  (parent-linked; probe order is descendant-before-ancestor)

panel                    seal=Open     grants —         probe: never — ancestor-only
├─ dock                  seal=Open     grants —         probe: focus.is_class(Tree|ItemList|RichTextLabel)
│  └─ dock.filesystem    seal=Open     grants FILEOPS   probe: chain.in_filesystem_dock
├─ searchbox             seal=Sealed   grants —         probe: focus.is_class(LineEdit)
│                                                              && sibling_nav_control.is_some()
├─ prompt                seal=Sealed   grants —         probe: chain.is_plugin_prompt
├─ editor.nav †          seal=Open     grants —         probe: attached CodeEdit && mode ∈ {None,
│                                                              Normal, Visual(_), OperatorPending(_)}
└─ unknown               seal=Open     grants —         probe: TOTAL — Node(0) if a focus owner
                                                              exists, else Rootless

editor.insert  (root)    seal=Barrier  grants —         probe: attached CodeEdit
                                                              && mode.is_some_and(|m| !is_nav_mode(m))
foreign        (root)    seal=Barrier  grants —         probe: CodeEdit-not-ours | TextEdit-not-CodeEdit
                                                              | LineEdit with no sibling nav control

† `editor.nav` declares `yields_to_engine = true` and carries ZERO rules of its own.
```

**Classification is an ordered total function.** The `PROVIDERS` array order *is* the probe order; the first probe that returns `Some(anchor)` wins. The order is `[editor, prompt, searchbox, filesystem, dock, foreign, unknown, panel]`, and two constraints fix it. `unknown`'s probe is **total** — it returns `Some` unconditionally — so it must be the last *probing* entry, and `foreign` must therefore be probed **before** it or `foreign` becomes unreachable and a Project Settings `LineEdit` falls through `unknown` to `panel`, where Ctrl+hjkl would be consumed in violation of `src/plugin/input.rs:90`. Equally, `foreign` must **not** come first: `prompt`, `searchbox`, `filesystem`, `dock` and `editor` all need first refusal, and `foreign`'s predicate ("`LineEdit` with no sibling nav control") would otherwise claim a dock filter box or the FS prompt before their own probes ran. `panel` has `probe: |_| None` — it is never classified directly, sits last in the array, and is reached only by following `parent` links. This preserves structurally the mutual exclusivity that today's `classify_focus` (`src/navigation/focus.rs:42-93`) gets from being one `if`-chain, which N independent predicates would silently lose. The resulting `SurfacePath` is the winner's parent chain, deepest first: a focused `Tree` inside the FileSystem dock yields `[dock.filesystem, dock, panel]`.

```rust
// src/actions/surface.rs
pub(crate) type SurfaceId = &'static str;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    /// Anchored at `chain.nodes[idx]`.
    Node(usize),
    /// Matched with NO focus owner at all. Only `unknown` may return this;
    /// `ActionCtx::target` is then `None`. Reproduces the early return at
    /// src/navigation/focus.rs:46-48 and `FocusContext::Unknown => true`
    /// at src/plugin/input.rs:120-122.
    Rootless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seal {
    /// Bindings on ancestors of this surface still apply.
    Open,
    /// BARE keys (no CTRL/ALT/META) stop here and fall to the control's own
    /// `gui_input`; modifier-bearing keys continue to the forest root.
    Sealed,
    /// Total hard stop: dispatch returns `Ignore` before any lookup.
    Barrier,
}

pub(crate) struct SurfaceSpec {
    pub(crate) id: SurfaceId,
    /// Declared forest parent. `None` only for `panel`, `editor.insert`
    /// and `foreign`. Specificity == depth in THIS forest, never scene depth.
    pub(crate) parent: Option<SurfaceId>,
    pub(crate) seal: Seal,
    /// Capabilities this surface adds on top of the widget's own.
    pub(crate) grants: fn(&FocusChain) -> Caps,
    /// Pure predicate over the sampled chain — no `Gd<T>`, so it is
    /// constructible from literals and unit-testable with no Godot runtime.
    pub(crate) probe: fn(&FocusChain) -> Option<Anchor>,
    /// Runs once per keystroke for every surface on the active path, BEFORE
    /// any lookup and regardless of whether a binding matches.
    pub(crate) on_key: Option<fn(&mut ActionCtx<'_>)>,
    /// `true` on `editor.nav` ONLY, and not settable from config. When the
    /// ANCHOR surface declares it and the vim engine claims the matched key,
    /// dispatch is abandoned and the key flows on to `gui_input`.
    pub(crate) yields_to_engine: bool,
}

pub(crate) struct SurfacePath {
    pub(crate) ids: Vec<SurfaceId>,       // deepest first
    pub(crate) anchor: Anchor,
    pub(crate) caps: Caps,                // widget caps ∪ every `grants` on the path
    pub(crate) seal: Seal,                // seal of the DEEPEST surface
    pub(crate) anchor_yields_to_engine: bool,
}
```

**Why not scene geometry.** Godot's focus chain is *generic at the leaf and specific at the ancestor*, so deepest-scene-node-wins is provably inverted here. `dock` and the widget kind come from the focus owner itself (`focus_owner.is_class("Tree")`, `focus.rs:60-68`), while `dock.filesystem` comes from an *ancestor*: `is_in_filesystem_dock` is `FileSystemDock::is_ancestor_of(control)` (`src/navigation/filesystem_explorer.rs:382-386`). A scene-depth ladder therefore ranks `dock` above `dock.filesystem` and destroys the FileSystem-first refusal that `src/plugin/input.rs:139-151` hardcodes today — the single behaviour the whole design must preserve. In the declared forest the same fact is trivially true: `dock.filesystem` names `dock` as its parent, so it is deeper, and `a` resolves there while `j` finds nothing there and falls through to `dock`. The hardcoded `if fs_result.is_consumed() { … } else { handle_dock_input(…) }` branch disappears with no replacement.

**Why not magic integers.** A hand-assigned `rank: u16` is a global z-index namespace with no allocator. Two third-party providers — a debugger surface and a signals surface — will both pick a plausible number, collide, and misroute by array position, and neither author can discover it. A parent link is local, reviewable in a diff, and makes the relation checkable: `is_ancestor_or_self` is a graph query, not a comparison of two numbers whose meaning nobody owns.

**Two escape semantics, deliberately distinct.** `Barrier` is a total hard stop — dispatch returns `Ignore` immediately and no ancestor is consulted; this makes "never intercept in Foreign" (`input.rs:90`) and "never intercept in insert-like modes" (`input.rs:91-105`, with `Select` excluded because it is insert-like) structural rather than conditional. `Sealed` is key-class-aware: *bare* keys stop at the surface and fall to the control's own `gui_input`, while modifier-bearing keys continue up to `panel`. One rule delivers three constraints at once — `<CR>` still reaches the FS prompt's `text_submitted`, typing in a dock filter box still works, and Ctrl+hjkl still escapes both. `is_prompt_active` (`input.rs:156-159`) is deleted, not replaced.

**The editor pair is a complement, not an enumeration.** `editor.insert`'s probe is written as the negation of `editor.nav`'s within "focus is the attached CodeEdit", never as `matches!(m, Insert | Replace | VirtualReplace | CommandLine | Select)`. `vim_core::primitives::Mode` is `#[non_exhaustive]` (`vim-core/vim-core/src/primitives/mode.rs:90-112`); two positive enumerations would let a future variant match neither, fall through to `foreign`, and leak Ctrl+hjkl to Godot. Written as a complement, totality is a tautology (`A ∧ P` xor `A ∧ ¬P`) rather than a test — and the test that asserts it is a cheap regression guard rather than the proof. `editor_mode == None` maps to `editor.nav`, transcribing the `is_none_or` polarity at `input.rs:92` so that "no controller" still means intercept.

### 3.4 The action registry and the name namespace

```rust
// src/actions/action.rs
pub(crate) struct ActionSpec {
    /// Dotted, globally unique, and the public name of this verb.
    pub(crate) id: &'static str,
    pub(crate) desc: &'static str,
    /// Consulted ONLY by the binding resolver when building candidates for a
    /// keystroke. Host-originated invocation does not consult it.
    pub(crate) requires: Caps,
    /// False => `:action`/`<Action>()` fails loudly ("requires panel focus")
    /// instead of declining invisibly with the CodeEdit as focus owner.
    pub(crate) host_invocable: bool,
    /// Some(path) iff `run` delegates through `ActionCtx::run_editor_shortcut`.
    /// Statically known so the injection-cycle audit is total (§3.7).
    pub(crate) delegates: Option<&'static str>,
    pub(crate) run: fn(&mut ActionCtx<'_>) -> Outcome,
}

pub(crate) struct ActionCtx<'a> {
    /// OWNED: `get_viewport()` yields an owned handle, so no borrow of a plugin
    /// field survives it (src/plugin/input.rs:279-281).
    pub(crate) viewport: Gd<godot::classes::Viewport>,
    /// `None` iff the surface anchored `Rootless`, or the action was invoked
    /// from `:action` / `<Action>()` with no usable focus owner.
    target: Option<Gd<godot::classes::Control>>,
    /// `Rc`, NOT `&FocusChain`: the chain cache is a field on the plugin, so a
    /// borrow of it and `plugin: &mut GodotVimCore` would be the same borrow
    /// twice. Cloning the `Rc` is a refcount bump (§4.5).
    pub(crate) chain: std::rc::Rc<FocusChain>,
    pub(crate) params: Params,
    pub(crate) plugin: &'a mut crate::plugin::GodotVimCore,
}
```

Every verb in the plugin has that one signature. The `plugin` field is how `godotvim.fs.create` reaches `self.fs_explorer` (`src/plugin/mod.rs:79`) and how `godotvim.prompt.dismiss` calls `dismiss_prompt()`.

**One string namespace.** Action ids are interned through `vim_core::keymap::NameRegistry` (`register(&mut self, name: &str) -> u32` at `vim-core/vim-core/src/keymap/name_registry.rs:35`, with `get_id` at `:58` and `get_name` at `:52`; idempotent and append-only). The same id string is what the dock key resolves to, what `:action godotvim.fs.refresh` names, what `nnoremap <leader>ff <Action>(godotvim.fs.create)` compiles to, and what a `panelmap` line's target token must match. That is the unification: not a shared dispatcher, a shared *namespace*.

**Three id spaces share one representation, and only names cross the boundary.** `VimEngine` mints its own action ids through its own registry and resolves `Key::Action(id)` back with `self.keymap.action_name(id)` before emitting `HostRequest::RunAction { name, .. }` (`vim-core/vim-core/src/execution/engine.rs:827-856`); the shell's `ActionId` is minted by a *second, disjoint* `NameRegistry`; and `SlotId` — the trie payload — is a third space in the same `KeyEvent::action(u32)` representation (`vim-core/vim-core/src/keymap/key_event.rs:184`). The invariant is enforced by newtypes that do not implement `Into<u32>`, and by the fact that `HostRequest::RunAction` carries a `CompactString` name. `PendingUiAction::RunRegistryAction` therefore carries a name too — never an id — which also keeps `src/bridge/` from depending on `crate::actions`.

**Where the registry lives.** Both the registry and the binding index live in one `ActionPlane` — pure data, zero `Gd<T>` — held as a **plain field on `GodotVimCore`**, never behind `Rc<RefCell<…>>` in `ControllerContext`. The reason is mechanical: a `RefCell` guard must never span `(spec.run)(&mut cx)`, because an action that re-enters the plugin would panic on a second borrow, and there is no consumer of the plane on the controller side. Both the introspector and the executor therefore hop to the plugin the same way. `:panelmap`, `:panelmap <lhs>` and `:checkhealth godotvim` are intercepted on the cmdline and relayed as `PendingUiAction::PanelCommand`, whose multi-line report goes to Godot's Output panel rather than the one-line status bar; *running* a verb needs `&mut GodotVimCore` and relays as `PendingUiAction::RunRegistryAction`. Interior mutability survives only where the design already needs it — `Rc<BindingIndex>` gives hot-reload an atomic index swap (§4.5), and `Rc` is established in-repo (`src/bridge/godot_host.rs:93-94`).

```rust
// src/actions/plane.rs — pure data, no Gd<T> anywhere.
pub(crate) struct ActionPlane {
    pub(crate) registry: std::rc::Rc<ActionRegistry>,
    pub(crate) index: std::rc::Rc<BindingIndex>,
    pub(crate) forest: Forest,
    pub(crate) diagnostics: Vec<PanelDiagnostic>,
    pub(crate) generation: u64,
}
```

The sampled `FocusChain` is **not** a field of the plane: it is cached on `GodotVimCore` alongside its validity key (§4.10), which is what lets the dispatcher clone the `Rc<FocusChain>` out before taking `&mut self`.

The registry stores `specs: Vec<&'static ActionSpec>` indexed by `ActionId`, so the resolver copies a `&'static ActionSpec` out of the borrow before `&mut self` is taken and execution holds no borrow of the plane at all.

### 3.5 The capability model

`Caps` names **affordances**, never widget taxonomy. That distinction is the whole correctness argument, because the natural taxonomy name (`LIST`) silently excludes a widget that answers the same key.

```rust
// src/actions/caps.rs
bitflags::bitflags! {
    /// Affordances the focused widget + its surface path offer. Every flag
    /// traces to a real dispatch decision in shipped code. Bits 5..15 reserved.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) struct Caps: u16 {
        /// Answers vertical next/prev. Tree, ItemList AND RichTextLabel —
        /// exactly the three arms of `handle_navigation`
        /// (src/navigation/dock_nav.rs:105-119).
        const VNAV      = 1 << 0;
        /// Expand/collapse. Tree only: `handle_hierarchy` returns false for
        /// every other class (dock_nav.rs:127-136), which is why the
        /// `DockKind::Tree` gates at dock.rs:128,113 are already redundant.
        const HIERARCHY = 1 << 1;
        /// Activate selection. Tree + ItemList; reproduces
        /// `DockKind::RichTextLabel => Ignored` at dock.rs:224.
        const ACTIVATE  = 1 << 2;
        /// Focus owner is a text input (LineEdit). Reproduces focus.rs:73-82.
        const TEXTENTRY = 1 << 3;
        /// Granted by the `dock.filesystem` SURFACE, not by a widget.
        /// Reproduces the `is_in_filesystem_dock` gate at input.rs:140.
        const FILEOPS   = 1 << 4;
    }
}

/// Pure. `is_a` is `Node::is_class` in production — the same predicate
/// `classify_focus` uses at focus.rs:50-88 — and a literal set in tests.
/// Probe order matches `classify_focus` (focus.rs:60-68) and
/// `handle_navigation` (dock_nav.rs:105-119) so the three cannot disagree.
pub(crate) fn widget_caps(is_a: &dyn Fn(&str) -> bool) -> Caps {
    if is_a("Tree") {
        // `|` is not const in bitflags 2; `.union()` is.
        Caps::VNAV.union(Caps::HIERARCHY).union(Caps::ACTIVATE)
    } else if is_a("ItemList") {
        Caps::VNAV.union(Caps::ACTIVATE)
    } else if is_a("RichTextLabel") {
        Caps::VNAV
    } else if is_a("LineEdit") {
        Caps::TEXTENTRY
    } else {
        Caps::empty()
    }
}
```

**How RichTextLabel scroll survives.** Today `j`/`k` in a dock has *no* widget gate: `handle_dock_input` calls `handle_navigation` unconditionally (`dock.rs:111-126`), and `handle_navigation`'s third arm scrolls a `RichTextLabel` by `RICHTEXTLABEL_SCROLL_STEP = 50.0` px and returns `true` (`dock_nav.rs:284-298`). `godotvim.item.next` / `godotvim.item.prev` therefore declare `requires: Caps::VNAV`, and `RichTextLabel` contributes `VNAV`. The gate stays a plain subset test — no `requires_any`, no second action, no OR — and both live surfaces keep working: the docs panel (`EditorHelp`'s `class_desc`) and the Output panel (`EditorLog`'s `log`), the latter reachable by the shipped `:Output` ex-command through `grab_focus_on_dock`, whose focusable filter explicitly admits `RichTextLabel` (`src/host/custom_commands.rs:74-76`, `:193-205`). `LIST` is recoverable as `VNAV ∧ ACTIVATE` and `SCROLL` as `VNAV ∧ ¬ACTIVATE`, so nothing is lost; if a future action genuinely means "scrolls but is not a list", adding a `SCROLL` bit back is additive.

The vocabulary is five flags, not nine. `PANEL` was a tautology (the forest root grants it to every non-Barrier path, so `requires: PANEL` can never fail — the `focus.*` actions carry `Caps::empty()`). `ESCAPE` had no possible grantor: returning focus to the script editor is a property of the script editor existing, and `handle_escape_from_dock` already declines when `get_script_editor()` or `get_current_editor()` is `None` (`dock.rs:241-267`) — declination is the correct gate. `SEARCHBOX` was redundant with `handle_slash`'s own decline when `find_sibling_search_box` returns `None` (`dock.rs:186-195`), and deleting it removes the depth-8-climb × depth-20-DFS (`src/navigation/dock_search.rs:15`, `:37`) from the per-focus-change path entirely — `handle_slash` runs it once per `/` press, exactly as today.

**The gate is a binding-plane gate.** `requires` is a field of `ActionSpec`, not of `Rule`: the resolver reads it by resolving the rule's `RuleTarget::Action(id)` through `registry.spec(id)`, which is why `native` and `<Shortcut>` targets — which have no `ActionSpec` — are never capability-gated. It decides whether a *rule* is a candidate for a resolved keystroke on a surface path. `registry.spec(id).run(&mut cx)` never consults it. That one sentence is what makes `:action godotvim.fs.refresh` work from the command line, where there is no keystroke, no surface and no sampled widget. The safety `FILEOPS` provides on the key path is provided on the invocation path by the executor itself, with the same predicate: every `godotvim.fs.*` body opens with `if !crate::navigation::is_in_filesystem_dock(&t) { return Outcome::Declined; }`. On the key path that guard is unconditionally true — the `dock.filesystem` probe *is* `in_filesystem_dock` — so it is a strict no-op there and does not disturb the verbatim move. `FILEOPS` is the one genuinely non-tautological surface-granted cap, and it earns its place: without it, `panelmap dock a godotvim.fs.create` would create files at `res://` root from a focused Scene tree, because `get_selected_path` returns `None` for a non-FS `Tree` and `begin_create` falls back to `"res://"` (`filesystem_explorer.rs:126-130`).

### 3.6 The resolver: a fold over ordered candidates

```
  ┌─ once per FOCUS CHANGE ────────────────────────────────────────────────┐
  │ FocusChain::sample()  →  FocusChain { nodes[], attached_editor,        │
  │                                       editor_mode, in_filesystem_dock, │
  │                                       sibling_nav_control,             │
  │                                       is_plugin_prompt }               │
  └────────────────────────────────────────────────────────────────────────┘
                │                                       │ once per KEY
                ▼                                       ▼
      classify (PROVIDERS order, first hit)     KeyProbes { primary, latin,
                │                                          physical(opt-in) }
                ▼
      SurfacePath [dock.filesystem, dock, panel]
        anchor = Node(0)
        caps   = VNAV|HIERARCHY|ACTIVATE  ∪  FILEOPS
        seal   = Open      anchor_yields_to_engine = false

  ── CANDIDATE CONSTRUCTION (resolve.rs — pure, zero Gd<T>) ───────────────
     for surface in path (DEEPEST → ROOT):
         for probe in ordered_probes(surface):        # primary, latin, [physical]
             index.lookup(surface, &[probe])          # ExactOnly ⇒ ONE slot
             ├ NoMatch                  → next probe
             └ rule ← index.rule_at(slot):
                 ├ Native               → STOP the walk, return Ignore
                 ├ Action(id) and
                 │   registry.spec(id).requires ⊄ caps
                 │                      → SKIP this surface (as if NoMatch)
                 └ hit                  → push (spec, params, consume); remember
                                          the matched KeyEvent; next surface
         if seal == Sealed && key is bare  → STOP the walk
     if anchor_yields_to_engine && vim_claims(matched) → return Ignore

  ── EXECUTION FOLD (dispatch.rs — the only &mut GodotVimCore site) ───────
     disposition ← Ignore
     for (spec, params, consume) in candidates:            # deepest first
         out ← (spec.run)(&mut cx)
         if consume == Void      { disposition ← Consume; BREAK }
         if out.accepted()       { disposition ← Consume; BREAK }
         else                    { /* Declined */          CONTINUE }
     exhausted → Ignore
```

Three properties of that shape are the design, not an implementation detail.

**Plurality comes from the walk, never from one LHS.** `MappingTrie::insert` writes `node.entry = Some(entry)` (`vim-core/vim-core/src/keymap/trie.rs:334-349`) and `TrieLookup::ExactOnly` yields exactly one `&MappingEntry` (`:266-279`), so one `(surface, lhs)` can only ever surface one rule. `BindingIndex.slots` is therefore `Vec<u32>` — one arena index per slot — with a `slot_of: HashMap<(SurfaceId, Vec<KeyEvent>), SlotId>` so that re-inserting the same `(surface, lhs)` *reuses* the slot instead of orphaning the previous rule in the arena where `:panelmap` would still list it. That is what makes last-writer-wins, `panelunmap` and rebinding work. The candidate list is plural because the *path* is plural: at most one candidate per surface on it.

**Declination is the composition operator.** Three things that look like three special cases are one mechanism. FileSystem-first refusal is `dock.filesystem` being deeper than `dock`. `h`/`l` inertness on an `ItemList` is `HIERARCHY ⊄ caps`, skipping the candidate with no widget name anywhere in the dispatcher. `j` at end-of-list is `handle_navigation` returning `false` → `Outcome::Declined` → next candidate → none → nothing consumed → Godot's `Tree` type-to-search still sees the key. Same fold, three answers.

**`Void` and `Native` are the two terminators that are not declination.** `Consumption::Void` consumes whether the action returned `Handled`, `Declined`, or short-circuited on `target == None`, and it terminates the walk — a verbatim transcription of `src/plugin/input.rs:124-135`, where `handle_window_nav`'s result is discarded with `let _ =` and `set_input_as_handled()` fires even with no focus owner. `RuleTarget::Native` is a *walk terminator* returning `Disposition::Ignore` immediately; modelling it as an action that returns `Declined` would be wrong, because declination continues the walk to `panel`, whose Ctrl+hjkl rules are `Void`, and the key would be consumed anyway — silently defeating the documented give-back.

```rust
// src/actions/bind.rs
#[derive(Debug, Clone)]
pub(crate) enum RuleTarget {
    /// A registered ActionSpec. Declining continues the walk.
    Action(ActionId),
    /// Give the key back to Godot AT THIS SURFACE. Terminates the walk.
    /// Permitted at every trust tier: it can only REDUCE what we consume.
    Native,
    /// Delegate to one of Godot's registered editor shortcuts. Not an
    /// ActionId, because there is no registered id per shortcut path.
    Shortcut(compact_str::CompactString),
}
```

**Arbitration is a property of the surface, not of a binding.** `vim_claims` is exactly `VimController::could_start_mapping` — the verbatim transcription of `src/plugin/input.rs:107-117`, including the `is_none_or` polarity, which becomes `is_some_and` on the inverted predicate so that "no controller" still means intercept. It already covers user mapping *prefixes*, because `could_start_mapping` is `!matches!(keymap.lookup(mm, &[key]), TrieLookup::NoMatch)`. The gate runs once, on the anchor surface, after resolution has produced a winner and before any candidate executes, and it is evaluated on the *same* `KeyEvent` that produced the winner — which is why `resolve` returns the matched key. Because `yields_to_engine` lives on `editor.nav` and that surface carries zero rules, the `<C-h>` rule exists exactly once, on `panel`; the editor/panel duplication gap is unrepresentable rather than assertion-checked, and a user who writes `panelmap panel <M-h> godotvim.focus.left` inherits the `:map <M-h>` escape hatch for free.

The complementary hole — vim-core's `<C-w>` grammar family, which `could_start_mapping` provably cannot see because `Keymap::lookup` never consults `CORE_KEYMAP` — is closed at **registration**, by asking vim-core's own parser:

```rust
// src/actions/keys.rs
use vim_core::grammar::Parser;
use vim_core::keymap::{KeyEvent, Keymap};
use vim_core::primitives::{Mode, Operator, VisualType};

const NAV_MODES: [Mode; 3] = [
    Mode::Normal, Mode::Visual(VisualType::Char), Mode::OperatorPending(Operator::Delete),
];

/// True when `key` puts vim-core's grammar into an `Awaiting*` state — i.e.
/// consuming it at `_input()` destroys the follow-up key. `vim_core::grammar`
/// is architecturally forbidden from importing `execution`
/// (vim-core/src/grammar/mod.rs:9-11), so this is session-free.
pub(crate) fn starts_vim_grammar_sequence(key: KeyEvent) -> bool {
    let keymap = Keymap::new();          // core defaults only
    NAV_MODES.iter().any(|&mode| {
        [false, true].into_iter().any(|sneak| {
            let mut parser = Parser::new();
            parser.set_sneak_mode(sneak);                 // parser.rs:404
            parser.process(key, &keymap, mode).is_pending() // result.rs:60
        })
    })
}
```

A rule whose surface is an ancestor-or-self of any `editor.*` surface — which includes `panel`, because `panel` is `editor.nav`'s declared parent — is rejected at registration if its LHS is multi-key or if its first key starts a grammar sequence. The predicate cannot rot when vim-core adds a Ctrl-prefixed grammar entry, and it catches `<C-\>`, which is intercepted at `vim-core/vim-core/src/grammar/parser.rs:129-133` and appears nowhere in `CORE_KEYMAP` at all.

### 3.7 The `FocusChain` seam

`FocusChain::sample()` is the **single Godot↔Rust boundary** in the whole subsystem. It walks `viewport.gui_get_focus_owner()` upward, bounded by `crate::scene_tree::MAX_DISCOVERY_DEPTH` (`src/scene_tree.rs:41`, value 20), and enriches the result once per focus change:

```rust
// src/actions/surface.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChainNode {
    pub(crate) class: compact_str::CompactString,
    pub(crate) instance: godot::prelude::InstanceId,
    pub(crate) name: compact_str::CompactString,
    /// Precomputed by `widget_caps` at sample time.
    pub(crate) widget_caps: Caps,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FocusChain {
    /// Index 0 is `gui_get_focus_owner()`; higher indices are ancestors.
    pub(crate) nodes: Vec<ChainNode>,
    pub(crate) attached_editor: Option<godot::prelude::InstanceId>,
    pub(crate) editor_mode: Option<vim_core::primitives::Mode>,
    /// `FileSystemDock::is_ancestor_of(focus_owner)`; grants Caps::FILEOPS.
    /// src/navigation/filesystem_explorer.rs:382-386.
    pub(crate) in_filesystem_dock: bool,
    /// Discriminant for the `searchbox` probe ONLY — reproduces
    /// focus.rs:73-82. There is NO `sibling_search_box` field: `handle_slash`
    /// runs `find_sibling_search_box` itself, once per `/` press, as today.
    pub(crate) sibling_nav_control: Option<godot::prelude::InstanceId>,
    /// Instance equality against `FileSystemExplorer`'s prompt LineEdit.
    /// Probed BEFORE `searchbox` AND BEFORE `foreign` — `foreign`'s predicate
    /// includes "LineEdit with no sibling nav control", so a later `prompt`
    /// probe could be pre-empted and `Esc` would never dismiss the prompt.
    pub(crate) is_plugin_prompt: bool,
}
```

Below the seam: `is_class`, `is_ancestor_of`, the sibling DFS in `src/navigation/dock_search.rs:63-94`, and instance-id comparison. Above it: everything. Probes, classification, seals, barriers, the capability algebra, prefix reservation and the consumption policy are all pure functions of a `FocusChain` you can write out as a literal. That matters because this repo's entire test strategy is Godot-free — `src/testing/mock_text_edit.rs`, and roughly 1200 lines of pure `translate_key` tests in `src/bridge/input.rs` — while `src/navigation/` has **zero** `cfg(test)` today. The seam is what makes the refactor verifiable rather than manually QA'd.

**Ownership during a keystroke.** The chain is cached on `GodotVimCore` as an `Rc<FocusChain>` beside its validity key `(focus owner InstanceId, plugin_epoch, index_generation)` — the epoch bumping on prompt open/close, the generation on every index rebuild (§4.10). The dispatcher clones the `Rc` into a local before constructing any `ActionCtx`, because `ActionCtx` needs `plugin: &mut GodotVimCore` at the same time and the two borrows cannot both come from `self`. It is a small `Vec` of small structs built at most once per focus change, so the clone is not on any hot path. The `viewport` is likewise a local: in the `input()` transport it comes from `EditorInterface::singleton().get_base_control()` (`src/plugin/input.rs:57-62`), not from a field.

**Transports differ only in their viewport, and that is deliberate.** `_input` is registered per-viewport in Godot (`scene/main/viewport.cpp:5578`, `input_group = "_vp_input" + id`), which is exactly why the FS prompt is reachable through *both* `GodotVimCore::input()` and the prompt LineEdit's own `gui_input` without either being dead code: Godot 4 can float a dock, which reparents its control subtree into a separate `Window` and therefore a separate `Viewport` (`editor/gui/window_wrapper.cpp:74-79`), where `input()` never fires and `gui_input` is the only live path. Exactly one of the two delivers any given event, so no deduplication is needed. Getting the viewport wrong is what silently drops consumption in floating editors, so it is a transport property and never a dispatcher one. Everything runs inside the existing `panic_guard` envelope (`src/plugin/mod.rs:112`).

**Injection is guarded at this seam too.** `ActionCtx::run_editor_shortcut(path)` clones and re-injects an `InputEventKey` through `Input::parse_input_event`. Because `Input::flush_buffered_events` pops from the same list `parse_input_event` appends to, an injected event whose key equals the binding's own LHS is re-dispatched inside the same flush call in the same frame — an editor hang that `panic_guard` cannot catch, because a livelock is not a panic. `src/actions/inject.rs` carries three layers: a registration-time cycle audit (which is total only because `ActionSpec::delegates` makes delegation statically known), a keyed per-frame fingerprint on `(keycode_with_modifiers, physical_keycode_with_modifiers, process_frame)` that drops our own synthesized events *without consuming them*, and a hard per-frame injection budget as a backstop.

**Named uncertainty.** Two things in this section were verified by reading source rather than by compiling, because no Rust toolchain was available while this document was written. (a) `widget_caps` is specified against `Node::is_class`, the string-based predicate `classify_focus` already uses; the memoized `ClassDB::is_parent_class` route is a permitted optimization only if the invariant test — for every class the `dock` probe admits, `widget_caps(c).contains(VNAV)` iff `handle_navigation` has an arm for `c` — passes with it. (b) `starts_vim_grammar_sequence`'s six expected verdicts (`true` for `<C-w>` and `<C-\>`; `false` for `<C-h>`, `<C-j>`, `<C-k>`, `<C-l>`) are traced through `vim-core/src/grammar/handlers/ready.rs:20-75`, `:141-150`, `:358-386` and `parser.rs:107-133`. The very first implementation task is the unit test asserting those six; if any disagrees, the guard's shape holds but its exclusion set needs re-derivation.

---

## 4. Types & APIs

Everything in this section lives under a new module `src/actions/`. Signatures are verified against `vim-core` at tag **v0.7.1** (checked out at `/home/firda/projects/vim-core`, crate root `vim-core/vim-core/src/`) and against godot-rust **v0.4.5** as it is actually used in `/home/firda/projects/godot-vim` today. Where a godot-rust API could not be verified from vendored source, it is flagged as such rather than asserted.

### 4.0 Module map

```
src/actions/
  mod.rs        re-exports; `pub(crate) fn resolve`
  outcome.rs    Outcome, Disposition
  caps.rs       Caps, widget_caps, dock_kind_of (transitional — see §4.3)
  surface.rs    ChainNode, FocusChain, Anchor, Seal, SurfaceSpec, SurfacePath, Forest
  action.rs     ActionId, Params, ActionSpec, ActionCtx, ActionRegistry
  keys.rs       KeyProbes, canonicalize, parse_lhs, starts_vim_grammar_sequence
  bind.rs       Rule, RuleTarget, Consumption, Repeat, SlotId, SurfaceBindings, BindingIndex
  resolve.rs    ResolveInput, Candidate, ResolvedTarget, Resolution, resolve()
  registrar.rs  Registrar, Provenance, RuleReject, PanelDiagnostic
  plane.rs      ActionPlane
  inject.rs     ShortcutInjector
  introspect.rs render(), emit_report()
  providers/    one file per surface + `const PROVIDERS`
```

`src/navigation/` keeps every executor body (`window::handle_window_nav`, `cycle::handle_cycle_focus`, `dock_nav::{handle_navigation, handle_hierarchy}`, `dock::{handle_slash, handle_enter, handle_escape_from_dock}`, `filesystem_explorer::*`). `src/actions/` replaces only the binding and dispatch logic.

### 4.1 What is consumed from vim-core v0.7.1

Every item below was checked for existence and `pub` visibility at the tag. All of it lives in `keymap` or `grammar`, both of which are architecturally forbidden from importing `execution` (`vim-core/vim-core/src/keymap/mod.rs:15-20`, `grammar/mod.rs:5-13`), which is what makes them usable inside Godot's global `input()` where no `VimSession` exists.

| Item | Real signature at v0.7.1 | Source |
|---|---|---|
| `MappingTrie` | `#[derive(Debug, Clone, Default)] pub struct MappingTrie` | `keymap/trie.rs:316-317` |
| `MappingTrie::insert` | `pub fn insert(&mut self, lhs: &[KeyEvent], entry: MappingEntry)` | `trie.rs:334` |
| `MappingTrie::lookup` | `pub fn lookup(&self, prefix: &[KeyEvent]) -> TrieLookup<'_>` | `trie.rs:427` |
| `MappingTrie::{remove, clear, entries, remove_by_owner, get_exact, get_single, len, is_empty}` | all `pub` | `trie.rs:363,408,572,517,480,470,499,493` |
| `TrieLookup<'a>` | **`#[non_exhaustive]`** `enum { NoMatch, ExactOnly(&'a MappingEntry), Prefix { exact: Option<&'a MappingEntry> } }` | `trie.rs:264-279` |
| `MappingEntry::new` | `pub const fn new(sequence: Vec<KeyEvent>, kind: MappingKind) -> Self` | `trie.rs:44` |
| `MappingEntry::{with_owner, with_description}` | `pub fn with_owner(self, MappingOwner) -> Self`, `pub fn with_description(self, Option<CompactString>) -> Self` | `trie.rs:154,161` |
| `MappingEntry::{new_nowait, nowait, owner, description, sequence}` | all `pub` | `trie.rs:75,207,169,176,183` |
| `MappingKind` | **`#[non_exhaustive]`** `enum { Recursive, NonRecursive }` | `keymap/mapping_kind.rs:7-16` |
| `MappingOwner` | `enum { User, Core, Host(CompactString) }` — *not* non-exhaustive | `keymap/mapping_owner.rs:17-25` |
| `NameRegistry` | `pub fn register(&mut self, name: &str) -> u32`; `get_name(&self, u32) -> Option<&str>`; `get_id(&self, &str) -> Option<u32>` | `keymap/name_registry.rs:35,52,58` |
| `KeyEvent` | `#[derive(Debug, Clone, Copy)]`, **hand-written `PartialEq`/`Eq`/`Hash` that compare `key` + `modifiers` only** | `keymap/key_event.rs:16-43` |
| `KeyEvent::{new, char, ctrl, action, with_latin, latin_key, key, modifiers, from_vim_notation, to_vim_notation}` | `pub const fn action(id: u32) -> Self`; `pub const fn with_latin(self, latin: Key) -> Self`; `pub const fn latin_key(&self) -> Option<Key>`; `pub fn to_vim_notation(&self) -> Cow<'static, str>` | `key_event.rs:48,58,68,184,259,270,376,382,303,350` |
| `Key` | **`#[non_exhaustive]`**, includes `Char(char)`, `Enter`, `Escape`, `Leader`, `Plug(u32)`, **`Action(u32)`** | `keymap/key.rs:8-92` |
| `Modifiers` | `bitflags!` over `u8`; `NONE/CTRL/ALT/SHIFT/META`; derives `Copy, PartialEq, Eq, Hash, Default` | `keymap/modifiers.rs:8-29` |
| `LangmapTable` | `pub fn parse(s: &str) -> Result<Self, LangmapError>`; `pub fn remap_key_event(&self, key: KeyEvent) -> KeyEvent` | `keymap/langmap.rs:140,240` |
| `MAX_KEY_SEQUENCE_LEN` | `pub const MAX_KEY_SEQUENCE_LEN: usize = 8;` | `keymap/keymap.rs:140` |
| `Keymap::new` | `pub fn new() -> Self` (core defaults, empty user layer) | `keymap/keymap.rs:298` |
| `parse_keys_from_string` | `pub fn parse_keys_from_string(text: &str) -> Vec<KeyEvent>` | `execution/engine/macro_replay.rs:325`, re-exported `execution/mod.rs:112` |
| `grammar::Parser` | `pub fn new() -> Self`; `pub fn process(&mut self, key: KeyEvent, keymap: &Keymap, mode: Mode) -> GrammarResult`; `pub const fn set_sneak_mode(&mut self, value: bool)` | `grammar/parser.rs:99,107,404` |
| `GrammarResult` | **`#[non_exhaustive]`**; `pub const fn is_pending(&self) -> bool` | `grammar/result.rs:18,60` |
| `primitives::Mode` | **`#[non_exhaustive]`**; `Normal`, `Insert`, `Visual(VisualType)`, `OperatorPending(Operator)`, … | `primitives/mode.rs:90-94` |

`could_start_mapping` is **not** consumed directly from vim-core: it is reached through the existing wrapper `VimController::could_start_mapping(&self, key: vim_core::keymap::KeyEvent) -> bool` (`src/controller/mod.rs:677-679`), which forwards to `VimEngine::could_start_mapping` (`vim-core/.../execution/engine/mapping.rs:78-82`, body `MappingMode::from_mode(mode).is_some_and(|mm| !matches!(self.keymap.lookup(mm, &[key]), TrieLookup::NoMatch))`).

**Non-exhaustive consequences, all load-bearing:**

* `TrieLookup` — every `match` in `resolve.rs` needs a `_ =>` arm. Treat unknown variants as `NoMatch`, never as a hit; a future variant must not silently start consuming keys.
* `Key` — `canonicalize` and `shift_variant` need `_ => k` / `_ => None`.
* `Mode` — the editor probes must be written as a predicate plus its negation (§4.4), never as an enumeration of insert-like modes. `matches!` is fine (it has an implicit fallthrough); a bare `match` is not.
* `MappingKind`, `GrammarResult` — we only *construct* the first and only call `.is_pending()` on the second, so neither forces a wildcard.

**`latin_key` is excluded from `PartialEq` and `Hash`** (`key_event.rs:10-14`, verified in the hand-written impls at `:30-43`). Two consequences: (a) trie lookup and `HashSet<KeyEvent>` treat `Char('о')+latin(o)` and `Char('о')` as identical, which is exactly what the `latin` probe wants; (b) `KeyEvent::new` drops `latin_key` (`key_event.rs:48-56`), so any transform that rebuilds a `KeyEvent` must re-apply `.with_latin(l)` explicitly — see `canonicalize` in §4.6.

**Not consumed, explicitly:** `VimSession::take_key_interest_if_dirty` (declared in `impl VimSession<SessionHost>`; this plugin holds `VimSession<GodotHost>`, `src/controller/mod.rs:94`) and `VimEngine::compute_key_interest`. The shell never computes a key-interest set; see §4.9.

### 4.2 `Outcome` and `Disposition`

```rust
// src/actions/outcome.rs

/// Result of running one action. This is today's `DockInputResult`
/// (src/navigation/dock.rs:20-61) promoted out of the dock module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Consumed in place.
    Handled,
    /// Consumed AND focus moved (callers may need extra bookkeeping).
    FocusChanged,
    /// This action refuses this keystroke. The walk continues to the next
    /// candidate; if none accepts, the key is NOT consumed and Godot's native
    /// handling proceeds — `Tree`'s own type-to-search and arrow keys at the
    /// `gui_input` stage, and the docks' `ED_SHORTCUT` accelerators (F2
    /// rename, Delete) at the later `shortcut_input` stage.
    Declined,
}

impl Outcome {
    pub(crate) const fn is_consumed(self) -> bool { !matches!(self, Self::Declined) }
    pub(crate) const fn accepted(self) -> bool { self.is_consumed() }
}

/// What the transport must do with the raw `InputEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// Call `set_input_as_handled()` on THIS transport's viewport.
    Consume,
    /// Do nothing; Godot handles the event.
    Ignore,
}
```

#### The `Ignored` → `Declined` migration, mechanically

A `type` alias *does* resolve variants — RFC 2338 landed in Rust 1.37, so `pub(crate) type DockInputResult = Outcome;` compiles `DockInputResult::Handled` in both expression and pattern position. What an alias cannot do is **rename** a variant: every `DockInputResult::Ignored` site fails with `E0599` because `Outcome` has no such variant. The stronger reason is P0's gate — the characterization suite is written *before* this move and names the variant in its assertions, so folding the rename into P2 would force P2 to edit P0's test files, destroying its own "P0 passes unmodified" acceptance criterion. The fix is to do the rename **first**, as a standalone behaviour-free commit at the head of the work, keeping the type name and module:

```sh
git grep -l 'DockInputResult::Ignored' -- src/ \
  | xargs sed -i 's/DockInputResult::Ignored/DockInputResult::Declined/g'
```

Verified count: **16 sites — 14 in `src/navigation/dock.rs`, 2 in `src/navigation/filesystem_explorer.rs`** (`grep -rn 'DockInputResult::Ignored' src/ | wc -l` → 16). Two traps:

1. `WindowNavResult::Ignored` is a **different enum with the same variant name** (`src/navigation/window.rs:41-46`: `enum WindowNavResult { Ignored, Focused }`). A blind `sed` on `::Ignored` hits it. Scope the pattern to `DockInputResult::`.
2. `DockInputResult::is_consumed(&self)` (`dock.rs:51-61`) has 5 call sites in `src/plugin/input.rs` (`:142,151,154,161,164`); the alias must keep it working, which is why `Outcome::is_consumed` exists alongside `accepted` rather than only the latter.

`WindowNavResult` is then **deleted, not aliased**, in the same commit that introduces `Outcome`: `Focused → Outcome::FocusChanged` (`window.rs:122`), `Ignored → Outcome::Declined` (`window.rs:54`, `:125`) — 3 sites. `handle_cycle_focus` (`cycle.rs:52-100`) is widened from `-> ()` to `-> Outcome`; its two silent early returns (`:56-58`, `:61-63`) become `Declined`, and its sole caller already discards the value (`cycle.rs:32-33`), so this is behaviour-preserving.

After the rename, `pub(crate) type DockInputResult = crate::actions::Outcome;` genuinely compiles every remaining call site unchanged.

### 4.3 `Caps`

`Caps` names **affordances, not widget classes**. That is the whole of the F2 fix: `handle_navigation` (`src/navigation/dock_nav.rs:96-125`) has exactly three arms — Tree (selection move), ItemList (selection move), RichTextLabel (50 px scroll at `dock_nav.rs:284-298`, returning `true`) — and `j`/`k` has **no widget gate at all** today (`dock.rs:111-126` calls it unconditionally). A `LIST` bit that RichTextLabel does not hold would kill `j`/`k` on the EditorHelp and Output panels, both of which are focusable RichTextLabels (`godot/editor/doc/editor_help.cpp:3490,3521`; `godot/editor/editor_log.cpp:519`) and one of which is reachable by the shipped `:Output` ex-command (`src/host/custom_commands.rs:74-76`). The correct bit is one that all three classes hold.

```rust
// src/actions/caps.rs
bitflags::bitflags! {
    /// Affordances the focused widget + its surface path offer. Every flag
    /// traces to a real dispatch decision in shipped code. Bits 5..15 reserved.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) struct Caps: u16 {
        /// Answers vertical next/prev. Tree, ItemList AND RichTextLabel — the
        /// three arms of `handle_navigation` (dock_nav.rs:105-119). Named for
        /// the AFFORDANCE: a RichTextLabel is not a list, but it does answer
        /// j/k — it scrolls 50px (dock_nav.rs:284-298).
        const VNAV      = 1 << 0;
        /// Expand/collapse. Tree only: `handle_hierarchy` returns false for
        /// every other class (dock_nav.rs:127-136), which is why the
        /// `DockKind::Tree` gates at dock.rs:128,112 are already redundant.
        const HIERARCHY = 1 << 1;
        /// Activate selection. Tree + ItemList. Reproduces the
        /// `DockKind::RichTextLabel => Ignored` arm at dock.rs:224.
        const ACTIVATE  = 1 << 2;
        /// Focus owner is a text input (LineEdit). Reproduces focus.rs:73-82.
        const TEXTENTRY = 1 << 3;
        /// Granted by the `dock.filesystem` SURFACE, not by any widget.
        /// Reproduces the `is_in_filesystem_dock` gate at input.rs:140.
        const FILEOPS   = 1 << 4;
    }
}

/// Pure. `is_a` is `node.is_class(c)` in production and a literal set in tests.
/// Probe order matches `classify_focus` (focus.rs:60-68) and `handle_navigation`
/// (dock_nav.rs:105-119) so the three can never disagree.
pub(crate) fn widget_caps(is_a: &dyn Fn(&str) -> bool) -> Caps {
    if is_a("Tree") {
        // `|` is not const in bitflags 2; `.union()` is.
        Caps::VNAV.union(Caps::HIERARCHY).union(Caps::ACTIVATE)
    } else if is_a("ItemList") {
        Caps::VNAV.union(Caps::ACTIVATE)
    } else if is_a("RichTextLabel") {
        Caps::VNAV
    } else if is_a("LineEdit") {
        Caps::TEXTENTRY
    } else {
        Caps::empty()
    }
}
```

**The requirement expression is a plain subset test** — `spec.requires ⊆ path.caps` — and it stays that way. There is no second gate axis and no disjunction. `LIST` is recoverable as `VNAV ∧ ACTIVATE` and `SCROLL` as `VNAV ∧ ¬ACTIVATE` if anything ever needs them; re-adding a `SCROLL` bit later is purely additive.

Three flags from the original vocabulary are **deleted**: `PANEL` (granted by the forest root to every non-barrier surface, so `requires: PANEL` is a tautology — `godotvim.focus.*` take `Caps::empty()`), `ESCAPE` (has no possible grantor; `handle_escape_from_dock` already declines when the script editor is missing, `dock.rs:243-249`), and `SEARCHBOX` (redundant with `handle_slash`'s own decline when `find_sibling_search_box` returns `None`, `dock.rs:192-194`). Deleting `SEARCHBOX` is what removes the depth-8-climb × depth-20-DFS (`dock_search.rs:37-58`) from the per-focus-change path entirely — `handle_slash` runs it itself, once per `/` press, exactly as today.

`FILEOPS` is the one non-tautological surface-granted cap and must stay: without it, `panelmap dock a godotvim.fs.create` would create files at `res://` root when focus is on a Scene tree, because `get_selected_path` returns `None` for a non-FS Tree and `target_dir` falls back to `"res://"` (`filesystem_explorer.rs:126-130`).

**`Caps` gates BINDINGS, never invocation.** The gate lives in `resolve.rs`; `(spec.run)(&mut cx)` never consults `requires`. That single rule is what makes `:action godotvim.fs.refresh` and `<Action>(godotvim.fs.create)` work from the editor, where the focus owner is the attached CodeEdit and no dock cap is present. Actions that genuinely need a FileSystem-dock target re-assert it inside their own body with the same predicate the surface probe uses (`is_in_filesystem_dock`), which is a strict no-op on the key path.

`dock_kind_of(&Gd<Control>) -> Option<DockKind>` also lives here during the migration, as the last consumer of `DockKind` outside `src/navigation/`. It is **not** deleted with `FocusContext`: `ActionCtx::target_or` returns a `DockKind` (§4.5) and this is the only `Gd<Control>` → `DockKind` classifier, so at P6 it moves to `src/navigation/dock.rs` together with `DockKind` itself, which `filesystem_explorer.rs:72,109,125` already uses independently.

### 4.4 Surface plane types

```rust
// src/actions/surface.rs
use compact_str::CompactString;
use godot::prelude::InstanceId;

pub(crate) type SurfaceId = &'static str;

/// One node of the sampled focus ancestor chain, enriched ONCE per focus
/// change. This is the single Godot→Rust seam: everything downstream is a pure
/// function of `FocusChain`, so it is constructible from literals in tests with
/// no Godot runtime — matching src/testing/mock_text_edit.rs and the ~1200
/// pure `translate_key` tests in src/bridge/input.rs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChainNode {
    pub(crate) class: CompactString,
    pub(crate) name: CompactString,
    pub(crate) instance: InstanceId,
    /// `widget_caps(&|c| node.is_class(c))`, memoized per class name.
    pub(crate) widget_caps: Caps,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FocusChain {
    /// Index 0 is `viewport.gui_get_focus_owner()`; higher indices are
    /// ancestors, bounded by `crate::scene_tree::MAX_DISCOVERY_DEPTH` (= 20,
    /// src/scene_tree.rs:41). EMPTY when there is no focus owner.
    pub(crate) nodes: Vec<ChainNode>,
    pub(crate) attached_editor: Option<InstanceId>,
    pub(crate) editor_mode: Option<vim_core::primitives::Mode>,
    /// `FileSystemDock::is_ancestor_of(focus_owner)`, evaluated once and
    /// unbounded, matching src/navigation/filesystem_explorer.rs:380-386.
    /// Grants Caps::FILEOPS.
    pub(crate) in_filesystem_dock: bool,
    /// Discriminant for the `searchbox` probe ONLY — reproduces focus.rs:78-80.
    /// There is deliberately no `sibling_search_box` field.
    pub(crate) sibling_nav_control: Option<InstanceId>,
    /// Instance equality against `FileSystemExplorer::prompt`
    /// (filesystem_explorer.rs:100-107). Probed BEFORE `searchbox` AND BEFORE
    /// `foreign`: `foreign` claims a "LineEdit with no sibling nav control",
    /// and whether the FS prompt LineEdit has one is not determinable without
    /// running the editor — so `prompt` must be given first refusal rather
    /// than relying on that predicate to miss.
    pub(crate) is_plugin_prompt: bool,
}

impl FocusChain {
    pub(crate) fn focus(&self) -> Option<&ChainNode> { self.nodes.first() }
    pub(crate) fn focus_is(&self, class: &str) -> bool;
    pub(crate) fn index_of_ancestor(&self, class: &str) -> Option<usize>;
    pub(crate) fn attached_editor_focused(&self) -> bool;
    /// Union of `widget_caps` for node 0 only. Ancestors contribute nothing.
    pub(crate) fn widget_caps(&self) -> Caps;
}
```

`ChainNode.class` records the concrete class name; `focus_is`/`index_of_ancestor` compare against the **recorded chain** rather than calling into Godot, which is what keeps probes pure. In production `widget_caps` is computed at sample time from `node.is_class(c)` — the same call the repo already uses throughout (`src/scene_tree.rs:30-36`, `dock_nav.rs:105`). A `ClassDb::is_parent_class` memoization keyed on class name is an optional optimisation; it is *not* required and is **not** verified against gdext v0.4.5 here (gdext sources are not vendored in this checkout), whereas `Gd<Node>::is_class(&str)` is verified by existing compiling code.

#### `Anchor` — why the probe no longer returns `Option<usize>`

`viewport.gui_get_focus_owner()` returning `None` is a real, mandatory state: `classify_focus` returns `FocusContext::Unknown` for it (`src/navigation/focus.rs:46-48`), `input.rs:120-122` maps `Unknown => true` (intercept), and `input.rs:126-134` then **skips `handle_window_nav` and calls `set_input_as_handled()` anyway**. With an empty `nodes` vector no chain index exists, so `Option<usize>` cannot express it.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    /// Anchored at `chain.nodes[idx]`.
    Node(usize),
    /// Matched with NO focus owner at all. Only the `unknown` surface may
    /// return this; `ActionCtx::target` is then `None`.
    Rootless,
}

pub(crate) type Probe = fn(&FocusChain) -> Option<Anchor>;

/// How a surface terminates the upward walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seal {
    /// Bindings on ancestors of this surface still apply.
    Open,
    /// BARE keys (no CTRL/ALT/META) stop here and fall to the control's own
    /// `gui_input`; modifier-bearing keys continue to the forest root. This is
    /// what preserves `<CR>` → `text_submitted` on the FS prompt while keeping
    /// Ctrl+hjkl able to escape a filter box.
    Sealed,
    /// Total hard stop. Dispatch returns `Ignore` immediately.
    Barrier,
}

pub(crate) struct SurfaceSpec {
    pub(crate) id: SurfaceId,
    /// Declared forest parent. Specificity is depth in THIS forest, never
    /// scene-tree depth and never a magic integer.
    pub(crate) parent: Option<SurfaceId>,
    pub(crate) seal: Seal,
    /// Capabilities this surface adds on top of the widget's own.
    pub(crate) grants: fn(&FocusChain) -> Caps,
    /// Pure predicate over the sampled chain. Probes run in registration order
    /// and the FIRST match wins — an ordered total function, preserving the
    /// mutual exclusivity today's `classify_focus` gets structurally.
    pub(crate) probe: Probe,
    /// Runs once per keystroke for every surface on the active path, BEFORE any
    /// lookup, regardless of whether a binding matches. This is where the
    /// stale-prompt auto-dismiss at filesystem_explorer.rs:74-80 lives.
    pub(crate) on_key: Option<fn(&mut ActionCtx<'_>)>,
    /// `true` on `editor.nav` ONLY. When the ANCHOR surface declares it and
    /// `vim_claims(matched_key)` holds, dispatch is abandoned and the key flows
    /// on to `gui_input`. This is a verbatim transcription of
    /// src/plugin/input.rs:106-118: the gate is a property of the CONTEXT,
    /// never of a binding. Not settable from config.
    pub(crate) yields_to_engine: bool,
}
```

`yields_to_engine` on `SurfaceSpec` is where `Arbitration` used to live on `Rule`. Moving it here is not a simplification for its own sake — it makes the `editor.nav`/`panel` duplication gap *unrepresentable*. `editor.nav` carries **zero rules**; the single `<C-h>` rule lives on `panel` (which is `editor.nav`'s declared parent, so it is live while the attached CodeEdit has focus), and the gate runs once on the anchor after resolution has produced a winner and before any candidate executes. A user who writes `panelmap panel <M-h> godotvim.focus.left` inherits the `:map <C-h>` escape hatch for free, and the panel's fatal-reject ("`Yield` on any non-editor surface") is satisfied by construction rather than by a registration-time downgrade. There is no `Arbitration` type and no `yield` token in the config grammar.

```rust
/// The active path, deepest surface first: e.g. [dock.filesystem, dock, panel].
#[derive(Debug, Clone)]
pub(crate) struct SurfacePath {
    pub(crate) ids: Vec<SurfaceId>,
    pub(crate) anchor: Anchor,
    /// Widget caps of node 0 | union of `grants` over every surface on the path.
    pub(crate) caps: Caps,
    /// Seal of the DEEPEST surface.
    pub(crate) seal: Seal,
    /// `yields_to_engine` of the DEEPEST surface only.
    pub(crate) anchor_yields_to_engine: bool,
}

/// The declared parent relation over all registered surfaces.
pub(crate) struct Forest { specs: Vec<&'static SurfaceSpec> }

impl Forest {
    pub(crate) fn get(&self, id: SurfaceId) -> Option<&'static SurfaceSpec>;
    pub(crate) fn ids(&self) -> impl Iterator<Item = SurfaceId> + '_;
    /// Deepest-first, bounded by `self.specs.len()` iterations. A declared
    /// parent cycle is rejected at registration; the bound is defence in depth
    /// so a malformed third-party provider cannot hang `input()`.
    pub(crate) fn path_from(&self, leaf: SurfaceId) -> Vec<SurfaceId>;
    pub(crate) fn is_ancestor_or_self(&self, maybe: SurfaceId, of: SurfaceId) -> bool;
}
```

Two probe shapes are worth writing out because they are the ones that are easy to get wrong.

```rust
// src/actions/providers/unknown.rs — the only surface that may anchor Rootless.
static UNKNOWN: SurfaceSpec = SurfaceSpec {
    id: "unknown",
    parent: Some("panel"),
    seal: Seal::Open,
    grants: |_| Caps::empty(),
    probe: |chain| Some(match chain.nodes.first() {
        Some(_) => Anchor::Node(0),
        None => Anchor::Rootless,
    }),
    on_key: None,
    yields_to_engine: false,
};

// src/actions/providers/editor.rs — a partition by construction, not by
// enumeration, because `Mode` is #[non_exhaustive].
const fn is_nav_mode(m: vim_core::primitives::Mode) -> bool {
    use vim_core::primitives::Mode;
    // Verbatim from src/plugin/input.rs:94-99. Select is deliberately EXCLUDED
    // (it is insert-like — input.rs:100-102).
    matches!(m, Mode::Normal | Mode::Visual(_) | Mode::OperatorPending(_))
}

static EDITOR_NAV: SurfaceSpec = SurfaceSpec {
    id: "editor.nav",
    parent: Some("panel"),
    seal: Seal::Open,
    grants: |_| Caps::empty(),
    // `editor_mode == None` maps HERE, not to the barrier: `is_none_or` at
    // src/plugin/input.rs:92 makes "no controller" mean INTERCEPT.
    probe: |chain| (chain.attached_editor_focused()
        && chain.editor_mode.is_none_or(is_nav_mode)).then_some(Anchor::Node(0)),
    on_key: None,
    yields_to_engine: true,   // carries ZERO rules
};

static EDITOR_INSERT: SurfaceSpec = SurfaceSpec {
    id: "editor.insert",
    parent: None,
    seal: Seal::Barrier,
    grants: |_| Caps::empty(),
    // The exact COMPLEMENT of EDITOR_NAV within "focus is the attached
    // CodeEdit". Written as a negation, NOT as `matches!(m, Insert | Replace |
    // VirtualReplace | CommandLine | Select)`, because a future `Mode` variant
    // would then match NEITHER editor surface, fall through to `foreign`
    // (Barrier), and leak Ctrl+hjkl. Today's `if !is_nav_mode { return false; }`
    // (input.rs:103-105) already treats every non-nav mode as a barrier.
    probe: |chain| (chain.attached_editor_focused()
        && chain.editor_mode.is_some_and(|m| !is_nav_mode(m))).then_some(Anchor::Node(0)),
    on_key: None,
    yields_to_engine: false,
};
```

`nav ^ insert` is then a tautology over any `Option<Mode>`, asserted anyway so a future probe edit fails the suite rather than a user's Ctrl+H. There is no `graph` surface: GraphEdit samples to `unknown`, whose parent is `panel`, and `FocusContext::Unknown => true` (`input.rs:120-122`) already lets Ctrl+hjkl escape a focused GraphEdit today.

### 4.5 Action plane types

```rust
// src/actions/action.rs
use godot::classes::{Control, Viewport};
use godot::prelude::*;

/// Index into `ActionRegistry::specs`, minted by the shell's OWN
/// `vim_core::keymap::NameRegistry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ActionId(pub(crate) u32);

/// Scalar params. Values are DECIMAL INTEGERS ONLY — there is no enum-token
/// form and the grammar must not promise one. A closed integer vocabulary is
/// what makes the sandbox whitelist extension provable: a value can never
/// expand into `:!`, `:source`, or a recursive chain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Params(Vec<(compact_str::CompactString, i64)>);

/// Upper bound on any repeat count. `find_navigable_target` walks up to
/// MAX_ATTEMPTS = 1000 per call (src/navigation/dock_nav.rs:37), so an
/// unbounded count is an editor freeze, not a slow key.
pub(crate) const MAX_ACTION_COUNT: i64 = 100;

impl Params {
    pub(crate) fn int(&self, key: &str, default: i64) -> i64 {
        self.0.iter().find(|(k, _)| k == key).map_or(default, |(_, v)| *v)
    }
    pub(crate) fn set_int(&mut self, key: &str, v: i64);
    /// ALWAYS use this for repeat loops — never `int("count", 1)` directly.
    pub(crate) fn count(&self) -> u32 {
        self.int("count", 1).clamp(1, MAX_ACTION_COUNT) as u32
    }
}

pub(crate) struct ActionSpec {
    pub(crate) id: &'static str,
    pub(crate) desc: &'static str,
    /// Consulted ONLY by the binding resolver when ranking candidates for a
    /// keystroke. Host-originated invocation does not consult it.
    pub(crate) requires: Caps,
    /// False ⇒ `HostRequest::RunAction` fails loudly ("requires panel focus")
    /// instead of declining invisibly. `godotvim.fs.*` are true (they can
    /// locate their own target); `godotvim.item.*` / `godotvim.dock.*` are false.
    pub(crate) host_invocable: bool,
    /// `Some(path)` iff `run` delegates through `ActionCtx::run_editor_shortcut`.
    /// Read by the registration-time shortcut-cycle audit.
    pub(crate) delegates: Option<&'static str>,
    pub(crate) run: fn(&mut ActionCtx<'_>) -> Outcome,
}
```

`ActionSpec` values are `static`s. `Caps::empty()` and `Caps::A.union(Caps::B)` are `const fn` in bitflags 2, and a non-capturing closure coerces to a `fn` pointer in a `static` initializer, so `run: |cx| { … }` is legal there.

#### `ActionCtx` — the three-way borrow, resolved

The naive shape `{ target: Gd<Control>, chain: &'a FocusChain, viewport: &'a mut Gd<Viewport>, plugin: &'a mut GodotVimCore }` does not compile and is not merely awkward: the chain cache lives on the plugin, so `&'a FocusChain` and `&'a mut GodotVimCore` are the same borrow twice; and `target: Gd<Control>` non-optional makes the mandatory no-focus-owner case unconstructible. Three changes, each backed by a pattern already shipping in this repo:

1. **`viewport` is OWNED.** `Node::get_viewport()` returns an owned `Gd<Viewport>` and the borrow of `self.attached_editor` ends at the end of the statement — `src/plugin/input.rs:279-281` already does `if let Some(mut vp) = editor.get_viewport() { vp.set_input_as_handled(); }` and compiles today. `Gd<T>` is a handle; ownership is a pointer copy.
2. **`chain` is `Rc<FocusChain>`, not `&FocusChain`.** Cloning the `Rc` is a refcount bump. The plugin's cache field is `Rc<FocusChain>` and the dispatcher clones it *before* taking `&mut self`. `std::rc::Rc` is already used for exactly this shape of cache at `src/bridge/godot_host.rs:93-94`.
3. **`target: Option<Gd<Control>>` and `params: Params` are owned.**

```rust
pub(crate) struct ActionCtx<'a> {
    /// OWNED: `get_viewport()` yields an owned handle; no borrow of
    /// `self.attached_editor` survives it (src/plugin/input.rs:279-281).
    pub(crate) viewport: Gd<Viewport>,
    /// OWNED + OPTIONAL. `None` is the no-focus-owner case that
    /// src/plugin/input.rs:127-133 must still consume for, and the
    /// `:action` / `<Action>()` case where focus is anywhere.
    target: Option<Gd<Control>>,
    /// Rc, NOT `&FocusChain`: the cache lives on the plugin.
    pub(crate) chain: std::rc::Rc<FocusChain>,
    pub(crate) params: Params,
    pub(crate) plugin: &'a mut crate::plugin::GodotVimCore,
}

impl ActionCtx<'_> {
    pub(crate) fn target(&self) -> Option<&Gd<Control>> { self.target.as_ref() }

    /// Target from the sampled chain, else a caller-supplied canonical control.
    /// Used by `godotvim.fs.*` so `:action` / `<Action>()` work from the editor.
    /// `DockKind` and its `dock_kind_of` classifier both survive the deletion
    /// of `FocusContext` and live in `src/navigation/dock.rs` (§4.3) — without
    /// the classifier this signature has no way to produce its second element.
    pub(crate) fn target_or(
        &mut self,
        locate: fn() -> Option<(Gd<Control>, DockKind)>,
    ) -> Option<(Gd<Control>, DockKind)>;

    /// Deferred because an immediate `grab_focus()` during input processing is
    /// swallowed by Godot's event dispatch loop. Single home for the four
    /// copies at dock.rs:234, window.rs:121, cycle.rs:99,
    /// filesystem_explorer.rs:267 — all four are `.call_deferred("grab_focus", &[])`.
    pub(crate) fn defer_grab_focus(&mut self, target: &Gd<Control>);

    /// Delegate to one of Godot's own registered editor shortcuts. Reuses the
    /// clone-and-inject path at filesystem_explorer.rs:437-472, behind the
    /// `has_method` capability gate and the `ShortcutInjector` re-entrancy
    /// guard. Returns `Outcome::Declined` when unavailable (Godot < 4.6), so
    /// the key is NOT consumed and Godot's own accelerator still fires.
    pub(crate) fn run_editor_shortcut(&mut self, path: &str) -> Outcome;
}
```

The **ordering rule** that makes it compile: take every shared handle out of `self` in its own statement first, then construct `ActionCtx { plugin: self, … }`. This is the NLL pattern the shipped code already depends on at `src/plugin/input.rs:284-288`, where `&mut self.controller` is released after `take_pending_ui_actions()` so `self.handle_pending_ui_action(action)` can take `&mut self` inside the same `if let` body. It is encapsulated in exactly one function:

```rust
// src/plugin/mod.rs
impl GodotVimCore {
    /// The ONLY place an ActionSpec runs. Every shared handle leaves `self`
    /// before `&mut *self` is taken.
    pub(crate) fn run_action_now(
        &mut self,
        spec: &'static ActionSpec,
        params: &Params,
        target: Option<Gd<Control>>,
    ) -> Outcome {
        let chain = std::rc::Rc::clone(&self.focus_chain);  // borrow ends here
        let params = params.clone();                        // borrow ends here
        let Some(viewport) = godot::classes::EditorInterface::singleton()
            .get_base_control()
            .and_then(|c| c.get_viewport())
        else {
            return Outcome::Declined;
        };
        let mut cx = ActionCtx { viewport, target, chain, params, plugin: self };
        (spec.run)(&mut cx)
    }
}
```

Because `ActionRegistry::specs` is `Vec<&'static ActionSpec>`, the `&'static ActionSpec` is **copied out** and the registry borrow ends immediately — no `Rc<ActionRegistry>` needs to stay alive across execution, and no `RefCell` guard may span `(spec.run)(…)`. A config reload that replaces `self.index` with a fresh `Rc` during an in-flight dispatch is safe: the old `Rc` keeps the old index alive, which *is* the atomic index swap the design wants.

#### `ActionRegistry` and the three id spaces

```rust
pub(crate) struct ActionRegistry {
    names: vim_core::keymap::NameRegistry,
    specs: Vec<&'static ActionSpec>,   // indexed by ActionId.0
}

impl ActionRegistry {
    pub(crate) fn register(&mut self, spec: &'static ActionSpec) -> ActionId {
        let id = self.names.register(spec.id);   // idempotent, append-only
        debug_assert!(id as usize <= self.specs.len());
        if id as usize == self.specs.len() { self.specs.push(spec); }
        ActionId(id)
    }
    pub(crate) fn id_of(&self, name: &str) -> Option<ActionId> {
        self.names.get_id(name).map(ActionId)
    }
    pub(crate) fn name_of(&self, id: ActionId) -> Option<&str> { self.names.get_name(id.0) }
    pub(crate) fn spec(&self, id: ActionId) -> &'static ActionSpec { self.specs[id.0 as usize] }
}
```

There are **three disjoint `u32` id spaces sharing one representation**, and the type system does not distinguish them, so state the invariant explicitly:

* `ActionId` — this registry's ids. Never leaves `src/actions/`.
* The **engine's** `NameRegistry` ids, resolved by `VimEngine` through `self.keymap.action_name(id)`. `<Action>(godotvim.fs.create)` in a `.godot-vimrc` is interned there, not here.
* `SlotId`, used as the trie payload via `KeyEvent::action(slot.0)`.

Numeric ids are **not interchangeable** across these. The only legal crossing between the shell and the engine is by **name**, which is why `PendingUiAction::RunRegistryAction` carries a `CompactString` and never a `u32`. There is deliberately no `ActionRegistry::as_key(ActionId) -> KeyEvent` helper: minting a `KeyEvent::action(ActionId)` and handing it to vim-core would resolve against the wrong table.

### 4.6 Key identity

```rust
// src/actions/keys.rs
use vim_core::keymap::{Key, KeyEvent, LangmapTable, Modifiers, MAX_KEY_SEQUENCE_LEN};

/// Ordered probe list for a single physical keystroke. Applied to the WHOLE
/// key, in this order, first hit wins — which is why `/` on a physical-J layout
/// can no longer be shadowed the way the hjkl block at dock.rs:111-146 shadows
/// the SLASH arm at :127.
#[derive(Debug, Clone, Default)]
pub(crate) struct KeyProbes {
    pub(crate) primary: Option<KeyEvent>,   // as typed, after langmap + canonicalize
    pub(crate) latin: Option<KeyEvent>,     // latin_key collapsed (Cyrillic/Greek)
    pub(crate) physical: Option<KeyEvent>,  // US-QWERTY position; opt-in per rule
}

/// `parse_godot_key` → langmap → normalize → canonicalize. The table text comes
/// from `controller.engine().options().langmap()`.
pub(crate) fn probes(ev: &Gd<godot::classes::InputEventKey>, langmap: &LangmapTable) -> KeyProbes;
```

There are **three** probes, not four. A fourth "named key with SHIFT cleared" stage was rejected: it is not a key-identity probe but a per-binding matching tolerance, and globally it would newly fire `godotvim.item.activate` on Shift+Enter and `godotvim.focus.editor` on Shift+Esc in a dock. Three distinct shift regimes exist today and a global stage can express only two:

* `handle_dock_input` rejects **all** modifiers including shift (`dock.rs:100-106`);
* `handle_search_input` rejects ctrl/alt/meta but **not** shift (`dock.rs:166`);
* `FileSystemExplorer::handle_key` uses shift as a **discriminant**: `match (key, shift)` with `(Some(Key::R), true) => refresh` (`filesystem_explorer.rs:87-97`).

So tolerance is `Rule::shift_tolerant`, expanded at registration into two LHS pointing at one `SlotId` (§4.7).

```rust
const CMD_MODS: Modifiers = Modifiers::CTRL.union(Modifiers::ALT).union(Modifiers::META);

/// Applied to BOTH parsed LHS and runtime probes so `<S-r>`, `<S-R>` and `R`
/// intern identically. `KeyEvent::from_vim_notation("<S-R>")` yields
/// `Char('R') + SHIFT`, while `translate_key` strips SHIFT for printables with
/// no CTRL/ALT/META (src/bridge/input.rs:404-407) and produces `Char('R') + NONE`.
/// The uppercase-with-SHIFT case must therefore also clear SHIFT.
pub(crate) fn canonicalize(k: KeyEvent) -> KeyEvent {
    match k.key() {
        Key::Char(c)
            if c.is_ascii_alphabetic()
                && k.modifiers().contains(Modifiers::SHIFT)
                && !k.modifiers().intersects(CMD_MODS) =>
        {
            let out = KeyEvent::new(
                Key::Char(c.to_ascii_uppercase()),
                k.modifiers() & !Modifiers::SHIFT,
            );
            // `KeyEvent::new` DROPS latin_key (key_event.rs:48-56); restore it.
            match k.latin_key() {
                Some(l) => out.with_latin(l),   // key_event.rs:259
                None => out,
            }
        }
        _ => k,   // `Key` is #[non_exhaustive]
    }
}
```

Shifted **non-alphabetic** `Char` LHS is not foldable at all — the shifted glyph is layout-dependent (`<S-1>` is `!` on US, `+` on DE) and `physical_to_ascii` is hardcoded US-QWERTY. It is rejected at load with a diagnostic telling the user to write the character literally, which is also what Vim requires. Named keys keep SHIFT (they return early in `get_named_key`, `src/bridge/input.rs:352-359`), which is why `shift_variant` (§4.7) is meaningful only for them.

```rust
/// Multi-key LHS. `KeyEvent::from_vim_notation` is single-key only, so `gg` and
/// `<Space>ff` need the public multi-key parser, followed by shell-side
/// `Key::Leader` substitution, canonicalization, and a length check.
pub(crate) fn parse_lhs(text: &str, leader: KeyEvent) -> Result<Vec<KeyEvent>, LhsError> {
    let mut keys = vim_core::execution::parse_keys_from_string(text);
    for k in &mut keys {
        if matches!(k.key(), Key::Leader) { *k = leader; }   // key.rs:190
        *k = canonicalize(*k);
    }
    if keys.is_empty() { return Err(LhsError::Empty); }
    if keys.len() > MAX_KEY_SEQUENCE_LEN { return Err(LhsError::TooLong(keys.len())); }
    Ok(keys)
}

#[derive(Debug)]
pub(crate) enum LhsError { Empty, TooLong(usize), ShiftedNonAlpha(KeyEvent) }
```

#### The `<C-w>` grammar guard, at registration

`could_start_mapping` provably cannot see `<C-w>`: `Keymap::lookup` merges the buffer/filetype/global **user** tables and never consults `CORE_KEYMAP` (`keymap/keymap.rs:605-618`). And `<C-w>` is `KeyClass::Action` in `CORE_KEYMAP` (`keymap/core.rs:208-213`), not `Prefix`, so a class-based guard cannot find it either. The answer is to ask vim-core's real state machine, once, at registration:

```rust
use vim_core::grammar::Parser;
use vim_core::keymap::Keymap;
use vim_core::primitives::{Mode, Operator, VisualType};

/// The nav modes in which an `editor.*` surface is live (src/plugin/input.rs:94-99).
const NAV_MODES: [Mode; 3] = [
    Mode::Normal,
    Mode::Visual(VisualType::Char),
    Mode::OperatorPending(Operator::Delete),
];

/// True when `key` puts vim-core's grammar into an `Awaiting*` state — i.e.
/// consuming it at `_input()` destroys the follow-up key.
///   `<C-w>`  → true   (grammar/handlers/ready.rs:141-150, Continue(AwaitingWindowCommand))
///   `<C-\>`  → true   (grammar/parser.rs:129-133, intercepted before classification
///                      and ABSENT from CORE_KEYMAP — the case a key-interest set misses)
///   `<C-h>` / `<C-j>` → false  (KeyClass::Motion → Execute or Invalid)
///   `<C-k>` / `<C-l>` → false  (KeyClass::Unknown → `_ => Invalid`, ready.rs:73)
/// Conservative in the safe direction: bare digits return true, which correctly
/// forbids `panelmap panel 3 …` from breaking `3j`.
pub(crate) fn starts_vim_grammar_sequence(key: KeyEvent) -> bool {
    let keymap = Keymap::new();   // core defaults only — user maps are
                                  // `could_start_mapping`'s job, not this one
    NAV_MODES.iter().any(|&mode| {
        [false, true].into_iter().any(|sneak| {
            let mut parser = Parser::new();
            parser.set_sneak_mode(sneak);              // parser.rs:404; sneak makes s/S two-key
            parser.process(key, &keymap, mode).is_pending()   // result.rs:60-62
        })
    })
}
```

This composes with dispatch-time arbitration to give total coverage: **core grammar prefixes are caught at registration; user mapping prefixes at dispatch by `could_start_mapping`.** Nothing is left to a hand-maintained denylist and nothing can rot when vim-core adds a Ctrl-prefixed grammar entry.

### 4.7 Binding plane types

```rust
// src/actions/bind.rs
use vim_core::keymap::{KeyEvent, MappingEntry, MappingKind, MappingOwner, MappingTrie, TrieLookup};

#[derive(Debug, Clone)]
pub(crate) enum RuleTarget {
    /// A registered ActionSpec. Declining continues the walk.
    Action(ActionId),
    /// Give the key back to Godot AT THIS SURFACE. Terminates the walk and
    /// yields `Disposition::Ignore` — NOT the same as `Outcome::Declined`,
    /// which would fall through to `panel`'s Void Ctrl+hjkl rules and consume
    /// anyway, silently defeating the documented escape hatch. Permitted at
    /// every trust tier: it can only REDUCE what we consume.
    Native,
    /// Delegate to one of Godot's registered editor shortcuts. Not an ActionId
    /// because there is no registered id per shortcut path.
    Shortcut(compact_str::CompactString),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Consumption {
    /// Consume iff the action accepted. Preserves `j` at end-of-list
    /// (dock.rs:113-126) and Enter with nothing selected (dock.rs:220-222).
    Elastic,
    /// Consume regardless of outcome AND terminate the walk — the declarative
    /// form of src/plugin/input.rs:126-134, where `handle_window_nav`'s result
    /// is discarded at :129 and `set_input_as_handled()` fires at :132 with no
    /// focus owner and no target found.
    Void,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Repeat { Allow, Suppress }

#[derive(Debug, Clone)]
pub(crate) struct Rule {
    pub(crate) surface: SurfaceId,
    pub(crate) lhs: Vec<KeyEvent>,        // canonicalized, len ≤ MAX_KEY_SEQUENCE_LEN
    pub(crate) target: RuleTarget,
    pub(crate) params: Params,
    pub(crate) consume: Consumption,
    /// `Suppress` drops `InputEventKey::is_echo()` repeats. Per-rule, not
    /// global: held j/k auto-repeat in docks is desirable; a ~20/s storm of
    /// deferred `grab_focus` from a held Ctrl+J is not.
    pub(crate) repeat: Repeat,
    /// Opt-in physical-position (US-QWERTY) probe. True on exactly 14 rules:
    /// 4 ctrl-hjkl on `panel`, 4 dock hjkl, 1 dock `/`, 5 FS `a/d/r/y/R`.
    /// `resolve_key` (filesystem_explorer.rs:364-374) IS the FS path, so those
    /// 5 are not counted twice. Not generalized to every key.
    pub(crate) physical: bool,
    /// Also register this LHS with SHIFT set. True on exactly the two
    /// `searchbox` rules (dock.rs:166 tolerates shift; dock.rs:100-106 does not).
    pub(crate) shift_tolerant: bool,
    /// `<nowait>`: build the trie entry with `MappingEntry::new_nowait`
    /// (trie.rs:75) so `lookup()` promotes `Prefix` to `ExactOnly` internally
    /// (trie.rs:443-446). Without this field the flag documented in §6.2 has
    /// no storage and `new_nowait` has no caller.
    pub(crate) nowait: bool,
    pub(crate) owner: MappingOwner,       // Host(provider tag) | User
    pub(crate) desc: compact_str::CompactString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SlotId(pub(crate) u32);
```

There is no `Arbitration` field. There is no `arb` token in the config grammar.

**On the name.** The enum is `RuleTarget` everywhere, never `Target`. It is a property of a `Rule`, it sits next to `RuleReject` in the same module, and the bare noun collides with `ActionCtx::target` — the `Gd<Control>` the action acts on — which is a different thing entirely. §5, §6 and the resolver sketches all use the qualified name.

```rust
pub(crate) struct SurfaceBindings {
    pub(crate) surface: SurfaceId,
    pub(crate) trie: MappingTrie,
    /// Bare first-keys that prefix a multi-key rule on this surface. Prefix
    /// reservation is opt-in and user-visible, never speculative.
    pub(crate) reserved: Vec<KeyEvent>,
    /// (canonical LHS, SlotId) — registration-time only. Makes re-insert REUSE
    /// the slot, so the previous rule is not orphaned in the arena where
    /// `:panelmap` would still list it.
    slot_of: Vec<(Vec<KeyEvent>, SlotId)>,
}

#[derive(Default)]
pub(crate) struct BindingIndex {
    /// ≤ ~16 entries. A linear scan on `&'static str` beats hashing here and
    /// costs no dependency. Registration order IS iteration order, which is
    /// what makes the introspector's golden snapshots stable — both
    /// `std::collections::HashMap` and `ahash::AHashMap` are randomly seeded
    /// per process and would not be. Mirrors the const-array idiom at
    /// src/controller/passthrough.rs:53-58 and src/config/presets.rs:39-212.
    surfaces: Vec<SurfaceBindings>,
    arena: Vec<Rule>,
    /// SlotId → arena index. EXACTLY ONE rule per (surface, lhs): the trie
    /// physically cannot hold two (`MappingTrie::insert` does
    /// `node.entry = Some(entry)`, trie.rs:345-348, and `TrieLookup::ExactOnly`
    /// yields one `&MappingEntry`), and last-writer-wins is what makes
    /// `panelunmap` and rebinding work. Candidate PLURALITY comes from the
    /// forest WALK — one candidate per surface on the path — never from a slot.
    slots: Vec<u32>,
    forest: Forest,
    pub(crate) generation: u64,
}
```

The trie payload is an opaque slot; **all** metadata lives in the arena. Two reasons, both verified: `MappingEntry` carries exactly one owner and `insert` overwrites at the same LHS, so a shared LHS across providers would let `remove_by_owner` delete a builtin when a third-party plugin unregisters (teardown is therefore a full rebuild); and `Key::Action(7).to_vim_notation()` renders as literally `<Action>(7)`, so `entries()` cannot be the listing source of truth.

```rust
impl BindingIndex {
    /// A surface is editor-reachable when it is an `editor.*` surface OR an
    /// ancestor of one in the DECLARED forest. `panel` is `editor.nav`'s parent,
    /// so `panelmap panel <C-w> …` is live in the editor — which is why
    /// "reject on editor.* surfaces" is not sufficient.
    fn editor_reachable(&self, surface: SurfaceId) -> bool {
        self.forest.ids()
            .filter(|id| id.starts_with("editor."))
            .any(|ed| self.forest.is_ancestor_or_self(surface, ed))
    }

    pub(crate) fn try_insert(&mut self, rule: Rule) -> Result<(), RuleReject> {
        if self.editor_reachable(rule.surface) {
            if rule.lhs.len() > 1 {
                return Err(RuleReject::MultiKeyOnEditorPath(rule.surface));
            }
            if crate::actions::keys::starts_vim_grammar_sequence(rule.lhs[0]) {
                return Err(RuleReject::VimGrammarPrefix(rule.lhs[0]));
            }
        }
        self.upsert(rule);
        Ok(())
    }

    /// Last writer wins at one (surface, lhs). Shift-tolerant rules insert the
    /// SAME SlotId at a second LHS.
    pub(crate) fn upsert(&mut self, rule: Rule) {
        let slot = self.alloc_or_reuse_slot(rule.surface, &rule.lhs);
        self.slots[slot.0 as usize] = self.arena.len() as u32;
        self.insert_at(rule.surface, &rule.lhs, slot, &rule);
        if rule.shift_tolerant {
            if let Some(shifted) = shift_variant(&rule.lhs) {
                self.insert_at(rule.surface, &shifted, slot, &rule);  // ONE rule, TWO LHS
            }
        }
        if rule.lhs.len() > 1 {
            self.reserve_first_key(rule.surface, rule.lhs[0]);
        }
        self.arena.push(rule);
    }

    fn insert_at(&mut self, surface: SurfaceId, lhs: &[KeyEvent], slot: SlotId, rule: &Rule) {
        let rhs = vec![KeyEvent::action(slot.0)];    // RHS: opaque slot key
        let entry = if rule.nowait {
                MappingEntry::new_nowait(rhs, MappingKind::NonRecursive)   // trie.rs:75
            } else {
                MappingEntry::new(rhs, MappingKind::NonRecursive)          // trie.rs:44
            }
            .with_owner(rule.owner.clone())
            .with_description(Some(rule.desc.clone()));
        self.bindings_mut(surface).trie.insert(lhs, entry);
    }

    pub(crate) fn lookup(&self, surface: SurfaceId, prefix: &[KeyEvent]) -> TrieLookup<'_> {
        self.surfaces.iter().find(|s| s.surface == surface)
            .map_or(TrieLookup::NoMatch, |s| s.trie.lookup(prefix))
    }

    pub(crate) fn is_reserved(&self, surface: SurfaceId, k: KeyEvent) -> bool {
        self.surfaces.iter().find(|s| s.surface == surface)
            .is_some_and(|s| s.reserved.contains(&k))
    }

    pub(crate) fn rule_at(&self, slot: SlotId) -> &Rule {
        &self.arena[self.slots[slot.0 as usize] as usize]
    }

    pub(crate) fn rules(&self) -> impl Iterator<Item = &Rule> + '_;   // introspector
    pub(crate) fn any_rule_targets(&self, id: ActionId) -> bool;      // checkhealth
}

/// Meaningful only for a single NAMED key with no CTRL/ALT/META: `translate_key`
/// keeps SHIFT for named keys (src/bridge/input.rs:352-359) but strips it for
/// printables (:404-407), so a `Key::Char` LHS never needs a shifted twin.
fn shift_variant(lhs: &[KeyEvent]) -> Option<Vec<KeyEvent>> {
    let [k] = lhs else { return None };
    if matches!(k.key(), Key::Char(_)) { return None; }
    if k.modifiers().intersects(CMD_MODS) { return None; }
    Some(vec![KeyEvent::new(k.key(), k.modifiers() | Modifiers::SHIFT)])
}
```

`<nowait>` needs no *resolver* code: `MappingTrie::lookup` honours `entry.nowait()` internally, promoting `Prefix` to `ExactOnly` at `trie.rs:443-446`. What it does need is the `Rule::nowait` bit above, which is the only thing that reaches `MappingEntry::new_nowait` (`trie.rs:75`) — the flag would otherwise be parseable per §6.2 and unstorable.

### 4.8 Resolver types

```rust
// src/actions/resolve.rs — the pure core, zero Godot types.

#[derive(Debug, Clone)]
pub(crate) enum ResolvedTarget {
    Action(&'static ActionSpec),
    Shortcut(compact_str::CompactString),
}

#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub(crate) target: ResolvedTarget,
    pub(crate) params: Params,
    pub(crate) consume: Consumption,
    /// Which surface produced this candidate — for `:panelmap <lhs>` explain.
    pub(crate) surface: SurfaceId,
}

#[derive(Debug, Clone)]
pub(crate) enum Resolution {
    /// Ordered candidates, deepest surface first. The transport runs them and
    /// stops at the first non-`Declined` (or at the first `Void`).
    Run(Vec<Candidate>),
    /// A reserved prefix is live: consume, buffer, arm the shell timer.
    Pending { timeout_ms: u32, fallback: Option<Candidate> },
    /// Consume and clear (a reserved prefix owns its whole subtree).
    DeadPrefix,
    /// Nothing here. Godot handles it. Also the result of `RuleTarget::Native`
    /// and of the `yields_to_engine` gate.
    None,
}

pub(crate) struct ResolveInput<'a> {
    pub(crate) path: &'a SurfacePath,
    pub(crate) probes: &'a KeyProbes,
    pub(crate) pending: &'a [KeyEvent],
    pub(crate) index: &'a BindingIndex,
    /// Needed for TWO reasons, both structural: `requires` is a field of
    /// `ActionSpec`, not of `Rule`, so the capability gate must resolve
    /// `RuleTarget::Action(id)` before it can test anything; and `Candidate`
    /// carries a `&'static ActionSpec`, so the id→spec hop happens here rather
    /// than in the executor. `specs` is `Vec<&'static ActionSpec>`, so the spec
    /// is COPIED out and this borrow does not survive into execution.
    pub(crate) registry: &'a ActionRegistry,
    /// EXACTLY `VimController::could_start_mapping` (src/controller/mod.rs:677-679
    /// → `VimEngine::could_start_mapping`, execution/engine/mapping.rs:78-82),
    /// which is `!matches!(keymap.lookup(mm, &[key]), TrieLookup::NoMatch)` and
    /// therefore already covers user mapping PREFIXES, not just exact matches.
    /// There is NO key-interest-set disjunct: `ctrl('h')` and `ctrl('j')` are
    /// `KeyClass::Motion` in CORE_KEYMAP (keymap/core.rs:121-122) and land in
    /// the normal/operator/visual interest sets (key_interest.rs:188-202), so
    /// such a disjunct would be unconditionally true for two of the four
    /// Ctrl+hjkl keys and would silently kill Ctrl+H / Ctrl+J panel navigation
    /// from the editor while Ctrl+K / Ctrl+L kept working.
    pub(crate) vim_claims: &'a dyn Fn(KeyEvent) -> bool,
    pub(crate) is_echo: bool,
}

pub(crate) fn resolve(input: &ResolveInput<'_>) -> Resolution;
```

`resolve` returns **owned** data — `&'static ActionSpec`, cloned `Params`, cloned `CompactString`. That is not incidental: it lets the transport hold `vim_claims` as a closure over `&self.controller` during resolution and drop that borrow before the executor takes `&mut self`. The transport-side closure is a verbatim transcription of `src/plugin/input.rs:92` + `:117` with the polarity inverted:

```rust
// should_intercept = controller.is_none_or(|c| !c.could_start_mapping(k))
// claims           = !should_intercept
//                  = controller.is_some_and(|c| c.could_start_mapping(k))
let claims = |k: vim_core::keymap::KeyEvent| {
    self.controller.as_ref().is_some_and(|c| c.could_start_mapping(k))
};
```

There is no `KeyClaimCache` and no cached `KeyInterestSet` anywhere in this design. Both were machinery for the deleted disjunct, and the mitigation they depended on is unreachable anyway: `take_key_interest_if_dirty` lives in `impl VimSession<SessionHost>` while this plugin holds `VimSession<GodotHost>` (`src/controller/mod.rs:94`), and `VimEngine::key_interest_dirty` is `pub(crate)`.

### 4.9 Registration types

```rust
// src/actions/registrar.rs

/// WHO wrote a rule, which is what splits the diagnostic policy. Distinct from
/// `MappingOwner`, which records WHICH provider — two builtin providers have
/// different owners and the same provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Provenance {
    /// Shipped provider defaults. A parse failure is a `debug_assert!` plus
    /// `log::error!` — never warn-and-skip (§6.8, §10-P5).
    Builtin,
    /// The single resolved vimrc, or a `:panelmap` typed at the cmdline.
    /// Warn-and-skip per line, accumulating into `Vec<PanelDiagnostic>`.
    User,
}

pub(crate) struct Registrar<'a> {
    index: &'a mut BindingIndex,
    registry: &'a mut ActionRegistry,
    forest: &'a mut Forest,
    diagnostics: &'a mut Vec<PanelDiagnostic>,
    owner: MappingOwner,           // set by `owner()`; tags every rule
    provenance: Provenance,        // Builtin here; User for the vimrc loader
}

impl Registrar<'_> {
    /// Tag for every rule this provider installs: `MappingOwner::Host(tag)`.
    pub(crate) fn owner(&mut self, tag: &'static str);
    pub(crate) fn surface(&mut self, spec: &'static SurfaceSpec);
    pub(crate) fn action(&mut self, spec: &'static ActionSpec) -> ActionId;
    /// Defaults are authored in the SAME text users type and parsed by the SAME
    /// parser (`src/config/panelmap.rs`), so they cannot drift from the
    /// documented syntax. Severity follows `self.provenance`: under `Builtin` a
    /// parse failure is a `debug_assert!` + `log::error!`, never warn-and-skip.
    pub(crate) fn defaults(&mut self, text: &'static str);
}

#[derive(Debug)]
pub(crate) enum RuleReject {
    MultiKeyOnEditorPath(SurfaceId),
    VimGrammarPrefix(KeyEvent),
    UnknownSurface(compact_str::CompactString),
    UnknownAction(compact_str::CompactString),
    ShortcutDeniedAtProjectTier,
    CountOutOfRange(i64),
    Lhs(LhsError),
}

#[derive(Debug)]
pub(crate) struct PanelDiagnostic {
    pub(crate) line: Option<u32>,
    pub(crate) source: &'static str,   // provider tag, or the vimrc path
    pub(crate) reject: RuleReject,
}

// src/actions/providers/mod.rs — the ONLY line a new subsystem adds outside
// its own file. Array ORDER is classification order (§3.3): every surface that
// needs first refusal comes before `foreign`, `foreign` comes before `unknown`
// because `unknown`'s probe is TOTAL, and `panel` never probes at all.
pub(crate) const PROVIDERS: &[fn(&mut Registrar<'_>)] = &[
    editor::register, prompt::register, searchbox::register, filesystem::register,
    dock::register, foreign::register, unknown::register, panel::register,
];
```

A `const` array, not `inventory`/`linkme`. Life-before-main constructors in a `cdylib` that Godot `dlopen`s, under `lto = "fat"` with linker section GC and `reloadable = true` hot-reload, buy nothing here: the requirement is zero *dispatcher* edits, which the array satisfies exactly, and it matches the repo's own idiom (`FILTER_CHAIN`, `src/controller/passthrough.rs:53-58`; `PRESETS`, `src/config/presets.rs:39-212`).

Rejections under `Provenance::User` are **warn-and-skip per line**, following `src/settings/reader.rs:240-264`, and every `PanelDiagnostic` is pushed to Godot's Output panel with `godot_warn!` directly — **not** through the `log` facade, whose default level is `Off` (`src/settings/defaults.rs:14`), which would make every diagnostic invisible on a stock install.

### 4.10 Ownership and the `PendingUiAction` extension

```rust
// src/actions/plane.rs — pure data, zero Gd<T>, unit-testable with no Godot.
pub(crate) struct ActionPlane {
    pub(crate) registry: std::rc::Rc<ActionRegistry>,
    pub(crate) index: std::rc::Rc<BindingIndex>,
    pub(crate) forest: Forest,
    pub(crate) diagnostics: Vec<PanelDiagnostic>,
    pub(crate) generation: u64,
}

impl ActionPlane {
    /// Two layers, never three. `vimrc` is `None` when the file is absent or
    /// `ProjectVimrc::Disabled` blocked it — builtin defaults still load, which
    /// is the zero-config guarantee. Mirrors `VimController::reload_config`
    /// (src/controller/mod.rs:701-724) exactly: clear → builtin text → user text.
    pub(crate) fn rebuild(&mut self, vimrc: Option<&str>);
    pub(crate) fn classify(&self, chain: &FocusChain) -> SurfacePath;
}

// src/plugin/mod.rs — new fields on GodotVimCore (existing fields at :53-86).
pub struct GodotVimCore {
    // … existing …
    plane: crate::actions::ActionPlane,
    focus_chain: std::rc::Rc<crate::actions::FocusChain>,
    /// Cache validity: (focus owner, plugin_epoch, index generation).
    chain_key: Option<(godot::prelude::InstanceId, u64, u64)>,
    /// Bumps on prompt open/close and on config reload — the events that
    /// change what the chain SAYS about an unchanged focus owner. Distinct
    /// from `ActionPlane::generation`, which counts index rebuilds.
    plugin_epoch: u64,
    injector: crate::actions::inject::ShortcutInjector,
}
```

`ActionPlane` lives directly on `GodotVimCore`, **not** behind `Rc<RefCell<…>>` in `ControllerContext`. The reason is that a `RefCell` guard must never span `(spec.run)(&mut cx)` — an action that re-enters the plugin would panic on a second borrow — and there is no remaining consumer on the controller side: `:panelmap` / `:panelunmap` / `:checkhealth` route to the **plugin** via `PendingUiAction::PanelCommand`, and their multi-line report goes to Godot's Output panel, not through the one-line status bar (which is injected as a child of the CodeEdit, `src/ui/status_bar.rs:413-425`, and is therefore unreachable from a dock). The command line still gets a one-line `HostResult::Success { message: Some(…) }` receipt.

**Two ownership rulings, recorded because the alternatives are each defensible.** *(1)* The plane is a plain field rather than `Rc<RefCell<ActionPlane>>`, and that choice is forced rather than aesthetic: `ActionCtx` holds `plugin: &'a mut GodotVimCore` and `chain: Rc<FocusChain>` at the same time (§4.5), so the chain cache cannot live inside a `RefCell` the executor is also reaching through. Interior mutability is kept only where it buys the atomic index swap — `registry` and `index` are `Rc`, so a config reload that installs a fresh `Rc` during an in-flight dispatch leaves the old one alive. *(2)* The chain cache key is three components, not two. A two-tuple of `(focus owner, index generation)` cannot see a prompt opening and closing under an unchanged focus owner, which is exactly the discriminant `is_plugin_prompt` and the S4 stale-prompt hook depend on; `plugin_epoch` is the field that makes §5.4's key expressible instead of merely stated.

```rust
// src/bridge/godot_host.rs — two variants added to the enum at :38-52.
#[derive(Debug, Clone)]
pub(crate) enum PendingUiAction {
    OpenMappingDialog,
    SourceConfigFile,
    ShowUndoTree,
    Vimdebug(compact_str::CompactString),
    PerfReport,
    PerfReset,
    ShowTooltip { symbol: String, line: i32, col: i32, warp_pos: Option<Vector2i> },

    /// Run a registered shell action BY NAME. Never by id: the engine owns a
    /// separate `NameRegistry`, and the shell's `ActionRegistry` is a second,
    /// disjoint instance. Carrying a name also keeps `bridge` free of any
    /// dependency on `crate::actions`.
    RunRegistryAction { name: compact_str::CompactString, count: Option<u32> },

    /// Full text of `:panelmap …` / `:panelunmap …` / `:checkhealth …`,
    /// mirroring the existing `Vimdebug(CompactString)` shape at :355-363.
    PanelCommand(compact_str::CompactString),
}
```

This is the answer to F3. `HostRequest::RunAction` (`src/host/dispatch.rs:437-487`) and `handle_custom_ex_command(id, command, editor: &mut Gd<CodeEdit>)` (`src/host/custom_commands.rs:379-404`) both run inside `GodotHost::handle_request` → `session.process_key`, i.e. inside a `&mut self.controller` borrow of `GodotVimCore`, with `editor: &mut Gd<CodeEdit>` as their only context. Neither can produce the `&mut GodotVimCore` that every `ActionSpec::run` requires. The `PendingUiAction` queue already crosses exactly that boundary, and the producer side needs **no signature change**: `pending_ui_actions: &mut Vec<PendingUiAction>` is already a parameter of `src/host/dispatch.rs:74`.

Three edits complete the path:

* **Producer**, `dispatch.rs:437`: probe the registry as link 0, gated on `name.contains('.') && !name.contains('/')`. Every EditorSettings shortcut path is `section/name`; every registry id is dotted; every entry of `list_all_commands()` (`custom_commands.rs:342-371`) is neither — so the split is total and behaviour-preserving. A startup assertion checks that no registered action id appears in `list_all_commands()`.
* **Relay**, `src/controller/process.rs:240-244`: both new variants must be added to the forward-to-plugin arm, or they hit the inline arms and never reach the plugin.
* **Consumer**, `GodotVimCore::handle_pending_ui_action` (`src/plugin/mod.rs:944-1004`): resolve the name through `self.plane.registry`, copy out the `&'static ActionSpec`, check `host_invocable`, then `self.run_action_now(spec, &params, target)` — which at P3 is still the chain-less form (§10-P2); the `chain` field is threaded in at P4 with no call-site change.

Latency is **zero frames** — `process_cycle` drains the host queue into `ctx.transient` (`process.rs:145-148`) and `handle_gui_input_impl` drains `ctx.transient` at `input.rs:284-288`, same call stack, same frame, no `call_deferred`. It is deferred past exactly three things, in order: engine effects applied to the CodeEdit; `ui_snapshot` + `apply_ui_update` (`input.rs:263-271`); `set_input_as_handled()` (`input.rs:278-282`). Only the second matters — a registry action must never publish user-visible text through `handle_show_message`, because the snapshot is already taken and the text would surface one keystroke late. Recursion is **depth-1 by construction**: `take_pending_ui_actions` is a `std::mem::take` (`src/controller/mod.rs:778`), so anything an action enqueues waits for the next keystroke. That is an intentional bound, not a bug.

Panel-key invocation through `input()` runs **inline** with `&mut self` and zero deferral. That asymmetry is real and is printed by `:panelmap`.

### 4.11 Cargo.toml delta

**Zero.** No line of `/home/firda/projects/godot-vim/Cargo.toml` changes.

Verified by reading the file (line numbers below are exact):

```toml
[dependencies]                                                     # :17
godot = { git = "…/gdext", tag = "v0.4.5" }                        # :18
vim-core = { git = "…/vim-core.git", tag = "v0.7.1" }              # :19
bitflags = "2"                                                     # :20   ← Caps uses this
unicode-segmentation = "1.10"                                      # :21
log = { version = "0.4", features = [...] }                        # :22
compact_str = "0.7"                                                # :23   ← Params / desc / RuleTarget::Shortcut

[dev-dependencies]                                                 # :25
proptest = "1.4"                                                   # :26
smallvec = "1"                                                     # :27   ← stays dev-only

[profile.release]                                                  # :30
opt-level = 2                                                      # :31
lto = "fat"                                                        # :32
```

**`ahash` is rejected.** It is not a dependency at any tier — `grep -rn ahash src/` returns nothing, and it appears in `Cargo.lock` only transitively via `vim-core` and `vim-regex`. Adding `ahash 0.8` would make `getrandom`, `once_cell` and `version_check` direct build inputs of a `cdylib` that Godot `dlopen`s, for a map with roughly a dozen entries consulted once per human keystroke. Neither `std::collections::HashMap` nor `ahash::AHashMap` is the right answer here: both are randomly seeded per process, which would make the introspector's golden-file snapshots order-dependent and force an explicit sort in the listing path. `BindingIndex.surfaces` is therefore a `Vec<SurfaceBindings>` scanned linearly — faster at this size, dependency-free, and deterministic. Where a set is genuinely wanted, `std::collections::HashSet<KeyEvent>` is already an in-repo idiom (`src/controller/passthrough.rs:20,:71`) and proves `KeyEvent: Hash + Eq`.

**The `smallvec` dev→prod promotion is dropped.** `grep -rn smallvec src/` matches only `src/testing/bridge_tests/{undo.rs,macros.rs}`, so it is genuinely dev-only today, and it stays at `Cargo.toml:27` under `[dev-dependencies]`. Every site the design once wrote as `SmallVec` — `SurfacePath.ids`, `Params`, `slots`, `Resolution::Run`, `ShortcutInjector.recent` — is either cached per focus change or built once per keystroke: at human key-repeat rates that is on the order of twenty tiny allocations per second. Use `Vec`. If profiling ever justifies it, `type Candidates = smallvec::SmallVec<[Candidate; 4]>` plus one manifest line is a swap with zero API impact; record that as the escape hatch, do not pay for it up front.

Earlier drafts cited two of these lines wrong: `smallvec` is at **:27**, not :26 (line 26 is `proptest = "1.4"`), and `lto = "fat"` is at **:32**, not :31 (line 31 is `opt-level = 2`).

### 4.12 Residual uncertainty in this section

Named rather than smoothed over:

1. **`ClassDb::is_parent_class` is unverified.** gdext sources are not vendored in this checkout, so the memoized class-name inheritance test used to precompute `ChainNode.widget_caps` could not be confirmed against v0.4.5. The design does not depend on it: production uses `node.is_class(c)` per sampled node, which is verified by compiling code (`src/scene_tree.rs:30-36`). Treat the memoization as an optimisation to be validated before it is written.
2. **`InstanceId` in test fixtures.** `FocusChain` uses `InstanceId` rather than raw `i64` to match the repo (`src/state/shell.rs:19` keys a `HashMap` on it; `src/host/buffer.rs:130` constructs one with `InstanceId::from_i64`). `InstanceId` is `NonZero`-backed in gdext, so golden fixtures must use non-zero values; confirm the exact failure mode (`panic` vs `try_from_i64`) at implementation.
3. **`ActionPlane` as a plain field is a decision, not a discovery.** The plane is a field on `GodotVimCore` rather than `Rc<RefCell<ActionPlane>>` cloned into `ControllerContext`. The tie-breaker is that a `RefCell` guard must not span action execution and the introspector no longer needs controller-side read access. If a future consumer on the controller side does appear, the `Rc<RefCell<…>>` shape returns — and with it the discipline that no borrow may outlive a statement.
4. **`Resolution::Pending` is typed but its timer is not specified here.** The `timeout_ms` source, the shell-side timer instance (which must *not* be `self.mapping_timer`, whose callback early-returns with no editor attached, `src/plugin/input.rs:311`), and the flush semantics belong to the dispatch model; only the type shape is fixed here.
5. **`Rule::physical` is claimed at 14 rules**: 4 `panel` Ctrl+hjkl, 4 `dock` hjkl, 1 `dock` `/`, 5 `dock.filesystem` `a/d/r/y/R`. The four `panel` rules gain the flag as a deliberate pre-registered change (today the physical fallback works from a dock but not from the editor, because the escape-hatch block at `input.rs:110-116` matches the logical keycode only and returns before `direction_from_hjkl`'s physical fallback at `:126` is reached). The FS five are counted once and not twice: `resolve_key` (`filesystem_explorer.rs:364-374`, with `is_fs_key` covering `A|D|R|Y` at `:376-378`) *is* the FileSystem path, not a sixth site on top of it. Verify the enumeration against the default rule table in §12.1 before shipping; a miscount is a silently dead binding on a non-QWERTY layout.
6. **`emit_signal` arity on `Gd<Control>` in gdext v0.4.5** is taken from shipped code (`src/navigation/dock.rs:206,193,194`: `control.emit_signal("item_activated", &[])` and `&[Variant::from(idx)]`). Any executor that moves those calls must preserve both emits and their order; the array `const ITEM_LIST_ACTIVATION_SIGNALS: [&str; 2] = ["item_selected", "item_activated"]` exists so a pure test can catch a dropped emit that no headless test could otherwise see.

---

# 5. Dispatch Model

This section is the complete keystroke resolution path: from the Godot `InputEvent` arriving at a transport, to a `Disposition` and a `set_input_as_handled()` call (or the deliberate absence of one). It is written top-to-bottom in execution order. Types are defined in §4; only the dispatch-critical signatures are repeated here.

The whole model is a pure function with three thin adapters around it. `resolve()` takes no `Gd<T>`, allocates no Godot object, and calls no Godot API. Everything Godot-shaped happens in exactly two places: `FocusChain::sample()` on the way in, and `set_input_as_handled()` on the way out. That is what makes the nine stages testable at all — this repo cannot instantiate `Gd<InputEventKey>` under `cargo test` (it is a `cdylib` GDExtension), which is why `src/bridge/input.rs` already tests the pure `translate_key(keycode, physical, unicode, ctrl, alt, shift, meta)` rather than constructing events.

---

## 5. Dispatch Model

### 5.1 Transports

Three entry points reach the action plane. Each owns its own viewport discipline; none of them owns dispatch policy.

| Transport | Entry point | Viewport used for `set_input_as_handled()` | Live when |
|---|---|---|---|
| **Primary** | `GodotVimCore::input()` → `handle_input_impl` (`src/plugin/input.rs:32`) | `EditorInterface::singleton().get_base_control().get_viewport()` (`input.rs:57-62`) | The event is delivered to the main editor window |
| **Fallback** | `on_fs_prompt_gui_input` (`src/plugin/mod.rs:782-801`) | the prompt `LineEdit`'s own `get_viewport()` | The FileSystem dock has been floated into a separate `Window` |
| **Host** | `HostRequest::RunAction` (`src/host/dispatch.rs:437`) via `PendingUiAction` | none — there is no `InputEvent` | `:action <id>`, `<Action>(id)` from a mapping |

**Why the viewports differ, and why getting it wrong is silent.** Godot registers `_input` per viewport: `input_group = "_vp_input" + itos(get_instance_id())` (`scene/main/viewport.cpp:5578`), dispatched from `Viewport::push_input` at `viewport.cpp:3546` with the in-source comment *"must happen before GUI, order is `_input` -> gui input -> `_unhandled input`"*. `GodotVimCore` is a child of the editor base control, so its `_input` fires only for the main window's viewport. A floated dock or a floated script editor is reparented into a `WindowWrapper`, i.e. a different `Viewport` — where our `_input` never runs, and where calling `set_input_as_handled()` on the *base-control* viewport marks the wrong viewport's event as handled and silently drops consumption. The `gui_input` transport already gets this right today (`input.rs:279-281` uses `editor.get_viewport()`), and the new dispatcher preserves it by making the viewport a parameter of the transport, never a value the resolver computes.

**The dual-transport ambiguity, resolved.** With the hardcoded `if key_event.get_keycode() == Key::ESCAPE { self.fs_explorer.dismiss_prompt(); }` at `src/plugin/mod.rs:795-797` deleted, both the primary transport (surface `prompt`) and the prompt's `gui_input` signal route through the same resolver. That looks like double dispatch. It is not, and neither path may be deleted:

- For a **consumed** key the two are mutually exclusive by construction. `Viewport::push_input` runs `_gui_input_event(ev)` only `if (!is_input_handled())` (`viewport.cpp:3549-3551`), so a key consumed by `input()` never reaches the signal.
- For a **declined** key in the primary viewport, both would run — and `SurfaceSpec::on_key` (the unconditional stale-prompt auto-dismiss, §5.5) would fire twice.
- With the dock floated, `input()` never fires at all and `gui_input` is the *only* transport that can deliver Escape to the prompt.

The discriminator is one line, not a heuristic: the fallback transport returns immediately when its own viewport *is* the base-control viewport.

```rust
// src/actions/transport.rs
pub(crate) fn is_primary_viewport(vp: &Gd<godot::classes::Viewport>) -> bool {
    godot::classes::EditorInterface::singleton()
        .get_base_control()
        .and_then(|c| c.get_viewport())
        .is_some_and(|base| base.instance_id() == vp.instance_id())
}
```

Consumption semantics are identical on both: `Control::_call_gui_input` emits the `gui_input` signal *first* and then returns before both the `_gui_input` virtual and the built-in handler when `get_viewport()->is_input_handled()` (`scene/gui/control.cpp:2572-2586`), so calling `set_input_as_handled()` from inside our handler suppresses `LineEdit`'s native handling exactly as `input()` does. Bare `<CR>` stays unbound on the sealed `prompt` surface and is therefore never consumed on either path, so `text_submitted` (connected at `filesystem_explorer.rs:183`) still fires.

**Known limitation, stated rather than smoothed over.** Because `_input` is per-viewport, *every* dock binding — today's `j/k/h/l`, `/`, Enter, Escape, and the FileSystem `a/d/r/y/R` — is already dead when a dock is floated. This design neither fixes nor worsens that; only the `prompt` surface gets a fallback transport wired in. The `is_primary_viewport` pattern generalises to every dock, and leaving it unwired is a scope decision, not an oversight.

The host transport enters at the action layer and skips key identity, surfaces, bindings, capability gating and consumption entirely; §5.13 states what that costs.

---

### 5.2 The stage list

```
S0    Transport guards            enabled → InputEventKey → is_pressed → bare-modifier filter
                                  → viewport → stale-editor self-heal
S0.5  Injection guard             drop our own synthesized shortcut events (do NOT consume)
S1    Key identity                parse_godot_key → langmap → normalize → canonicalize → KeyProbes
S2    Surface sampling            FocusChain::sample() (cached), ordered classify → SurfacePath
S3    Barrier                     anchor seal == Barrier → Ignore, immediately
S4    Per-surface hooks           on_key for every surface on the path, before any lookup
S5    Resolution                  leaf→root walk: probes × trie × seal × caps → Resolution
S6    Arbitration                 anchor.yields_to_engine && vim_claims(matched) → Ignore
S7    Execution                   run candidates deepest-first, stop at first non-Declined
S8    Consumption                 Elastic/Void → Disposition
S9    Transport commit            set_input_as_handled() on THIS transport's viewport
```

S0 is unchanged from today (`src/plugin/input.rs:33-76`) and runs inside the existing `panic_guard` envelope (`src/plugin/mod.rs:111-112`). One S0 change: echo events are **sampled**, not filtered. `InputEvent::is_echo()` is on the base class in pinned gdext v0.4.5, so `Gd<InputEventKey>` reaches it through `Deref`; the flag is carried into `ResolveInput` and consumed per-rule at S8 rather than discarded globally.

---

### 5.3 S1 — one global key identity

`src/actions/keys.rs` replaces all three ad-hoc per-site fallbacks that exist today: `dock_hjkl`/`hjkl_to_dock` (`src/navigation/dock.rs:72-86`), `direction_from_hjkl`/`hjkl_direction` (`src/navigation/window.rs:27-38`), and `resolve_key`/`is_fs_key` (`src/navigation/filesystem_explorer.rs:364-378`).

Pipeline: `parse_godot_key` (`src/bridge/input.rs:446`) → `LangmapTable::remap_key_event` → `normalize_key_for_mapping` (`src/controller/process.rs:542`) → `canonicalize`. The langmap table comes from `controller.engine().options().langmap()` — `VimEngine::options` is `pub const fn options(&self) -> &VimOptions` (`vim-core/src/execution/engine/public_api.rs:649`), `VimOptions::langmap` is `pub fn langmap(&self) -> &str` (`vim-core/src/primitives/vim_options.rs:969`), and `LangmapTable::parse` / `remap_key_event` are public at `vim-core/src/keymap/langmap.rs:140` and `:240`. godot-vim uses langmap nowhere today; this is new plumbing, cached and rebuilt at config-source time.

The result is three probes, tried **in this order against the whole key**, first hit wins:

| # | Probe | Purpose | Gate |
|---|---|---|---|
| 1 | as-typed `KeyEvent`, post-langmap, canonicalized | the normal case | always |
| 2 | `latin_key` collapsed | Cyrillic / Greek | only when `latin_key` is `Some` — set at `src/bridge/input.rs:410-412` for non-ASCII output only |
| 3 | `physical_to_ascii(physical, shift)` (`src/bridge/input.rs:97`) | Colemak / Dvorak / AZERTY / QWERTZ | **only for rules carrying the `physical` flag** |

There is no fourth probe. The synthesis carried a "named key with SHIFT cleared" probe; it is deleted, because it is not a key *identity* question but a per-binding matching *tolerance*, and there are three regimes in today's source, not two: `handle_dock_input` rejects all modifiers including shift (`dock.rs:100-106`), `handle_search_input` rejects ctrl/alt/meta but tolerates shift (`dock.rs:166`), and `FileSystemExplorer::handle_key` uses shift as a *discriminant* — `match (key, shift) { (Some(Key::R), true) => self.refresh(), … }` (`filesystem_explorer.rs:87-97`). A global stage cannot express that; a per-surface bool expresses only two of three. It is modelled instead as `Rule::shift_tolerant`, expanded at **registration** into a second trie LHS pointing at the same `SlotId`. Exactly two rules carry it (`searchbox <CR>` and `searchbox <Esc>`), and dispatch gains no stage.

### Shift canonicalization

`translate_key` strips SHIFT for printables with no CTRL/ALT/META (`src/bridge/input.rs:399-407`) and keeps it for named keys (which return early in `get_named_key`, `src/bridge/input.rs:20-26`). So the runtime event for `R` is `Char('R') + NONE`. Meanwhile `KeyEvent::from_vim_notation("<S-r>")` yields `Char('r') + SHIFT` and `from_vim_notation("<S-R>")` yields `Char('R') + SHIFT` — `Modifiers::from_vim_prefix` strips the `S-` and the remainder is taken literally (`vim-core/src/keymap/modifiers.rs:43-70`, `key_event.rs:303-318`). All three must intern identically or the binding is dead on arrival. The original fold matched only `(Key::Char(c), Modifiers::SHIFT)` with `c` ASCII-lowercase, so `<S-R>` fell through the catch-all unchanged and could never match:

```rust
// src/actions/keys.rs
const CMD_MODS: Modifiers = Modifiers::CTRL.union(Modifiers::ALT).union(Modifiers::META);

pub(crate) fn canonicalize(k: KeyEvent) -> KeyEvent {
    match k.key() {
        Key::Char(c)
            if c.is_ascii_alphabetic()
                && k.modifiers().contains(Modifiers::SHIFT)
                && !k.modifiers().intersects(CMD_MODS) =>
        {
            let out = KeyEvent::new(
                Key::Char(c.to_ascii_uppercase()),
                k.modifiers() & !Modifiers::SHIFT,
            );
            // `KeyEvent::new` sets latin_key: None (key_event.rs:48-56); restore it.
            match k.latin_key() {
                Some(l) => out.with_latin(l),   // key_event.rs:259
                None => out,
            }
        }
        _ => k,
    }
}
```

Shifted **non-alphabetic** characters are not foldable at all and must not pretend to be. `<S-1>` parses to `Char('1') + SHIFT`, while the runtime produces `Char('!') + NONE` on US and `Char('+') + NONE` on DE. There is no canonical form, because `physical_to_ascii` is a hardcoded US-QWERTY table (`src/bridge/input.rs:97-125`, `KEY_1 → '!'`). Such an LHS is **rejected at load** by `validate_lhs_key` with a diagnostic telling the user to write the literal character (`!`, not `<S-1>`) — which is what Vim requires anyway. That removes the case from the round-trip property's domain by construction instead of shipping a binding that can never fire.

`canonicalize` is applied to *both* sides — parsed LHS at registration and runtime probes at S1 — so the two planes cannot disagree.

### Why a per-match-arm fallback is fatal

The physical-position fallback is not wrong; evaluating it *inside a match arm* is. Both live instances are the same category error.

**Instance 1 — `/` shadowed by hjkl.** `handle_dock_input` evaluates `if let Some(direction) = dock_hjkl(key_event)` at `dock.rs:111` and returns from inside that arm, before the `match keycode` at `dock.rs:150-156` that owns `Key::SLASH`, `Key::ENTER` and `Key::ESCAPE`. `dock_hjkl` is `hjkl_to_dock(logical).or_else(|| hjkl_to_dock(physical))` (`dock.rs:72-76`). So *any* keystroke sitting at a physical H/J/K/L position is claimed by the hjkl arm regardless of what it logically produces — and both `Key::SLASH => handle_slash` at `:127` and the explicit `_ if physical == Key::SLASH => handle_slash` at `:130` are unreachable for it. The `:130` arm is the author's own attempt to give `/` a physical fallback, defeated by an earlier arm's physical fallback. Which concrete layouts put a `/`-producing key at H/J/K/L is layout-database-dependent and I have not enumerated it; the *structural* unreachability is what matters and is directly readable at those line numbers.

**Instance 2 — QWERTZ `z` fires `yank_path`.** This one is not hypothetical. `is_fs_key` matches `Key::A | Key::D | Key::R | Key::Y` (`filesystem_explorer.rs:376-378`) and `resolve_key` tries logical then physical (`:364-374`). German QWERTZ swaps Y and Z, so the key producing `z` sits at physical `Key::Y`: `is_fs_key(Key::Z)` is false, `is_fs_key(Key::Y)` is true, and a QWERTZ user typing `z` in the FileSystem dock executes `godotvim.fs.yank_path`.

The fix is ordering plus opt-in, not deletion. Because the probe order applies to the whole key and consults the binding index at probe 1 before probe 3 exists, logical `/` now resolves first and can never be shadowed. Because probe 3 fires only for `physical`-flagged rules, the QWERTZ alias survives on exactly the **14 rules** that have it today (4 `panel` Ctrl+hjkl, 4 `dock` hjkl, 1 `dock /`, 5 `dock.filesystem a/d/r/y/R` — note fourteen *rules*, not the thirteen distinct *keys* the earlier count reported; `R`/refresh also routes through `resolve_key` at `filesystem_explorer.rs:88`) and on nothing else. A QWERTZ user who wants `z` back writes `panelmap dock.filesystem z <target>` and probe 1 wins.

Two consequences worth naming rather than hiding. First, this is a behaviour *change* in the editor: today Ctrl+hjkl has no physical fallback when focus is the attached CodeEdit, because the escape-hatch block at `input.rs:110-116` matches the **logical** keycode only and `_ => return false` exits before `direction_from_hjkl`'s physical fallback at `:126` is ever reached — so a Cyrillic user gets panel navigation from a dock but not from the editor. Under the new order they get it from both. It ships as a pre-registered red-then-green test, not as a surprise. Second, the physical alias is not sticky across a rebind: a user who writes `panelmap dock.filesystem a godotvim.fs.rename` silently inherits no `<physical>` flag. Whether to warn on that, or make `physical` sticky per `(surface, lhs)`, is open (§13).

---

### 5.4 S2 — FocusChain sampling

`FocusChain::sample()` is the single Godot→Rust seam. It walks `viewport.gui_get_focus_owner()` upward, bounded by `MAX_DISCOVERY_DEPTH` (`src/scene_tree.rs:41`), and enriches the chain **once per focus change** — cached on `(focus_owner InstanceId, plugin_epoch, index_generation)`, where the epoch bumps on prompt open/close and config reload. Per-node it precomputes the class chain and `widget_caps` from `node.is_class(c)` — the string predicate `classify_focus` already uses and the one verified by compiling code (`src/scene_tree.rs:30-36`). A `ClassDb::is_parent_class` memoization keyed on class name is a permitted optimisation, not the design: it is unverified against gdext v0.4.5 (§4.12) and may only be adopted once the invariant test in §3.7 passes with it. Per-chain it precomputes `attached_editor`, `editor_mode`, `in_filesystem_dock` (`FileSystemDock::is_ancestor_of`, `filesystem_explorer.rs:380-386`), `sibling_nav_control` (the discriminant `classify_focus` uses at `focus.rs:73-82` to separate a dock filter box from a foreign `LineEdit`) and `is_plugin_prompt`.

There is deliberately **no** `sibling_search_box` field. The depth-20 sibling DFS (`src/navigation/dock_search.rs:37-58`) stays inside `handle_slash`, run once per `/` press exactly as today (`dock.rs:186-195`, which already declines when it finds nothing), rather than once per focus change. That is what removes the eager-versus-lazy cost question entirely.

Every `SurfaceSpec::probe` is then `fn(&FocusChain) -> Option<Anchor>` — pure, constructible from literals, no Godot linkage.

```rust
// src/actions/surface.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    /// Anchored at `chain.nodes[idx]`.
    Node(usize),
    /// Matched with NO focus owner at all. Only `unknown` may return this.
    Rootless,
}
pub(crate) type Probe = fn(&FocusChain) -> Option<Anchor>;
```

`Anchor::Rootless` exists for exactly one reason, and it is a hard constraint rather than a nicety — see §5.7.

Classification is an **ordered total function**: probes run in `PROVIDERS` order, first match wins, and the ordering is `[editor, prompt, searchbox, filesystem, dock, foreign, unknown, panel]` with `foreign` as the terminal *recognized* arm and `unknown` as the unconditional catch-all. (`foreign` must not be first; if it were, whatever predicate it uses would claim before every other surface.) This preserves the mutual exclusivity today's `classify_focus` gets structurally from an if/else chain (`src/navigation/focus.rs:42-93`), which N independent predicates would silently lose.

The winner's declared forest path, deepest-first, becomes the `SurfacePath`: `ids`, `anchor`, `caps` (widget caps ∪ every `grants` on the path), `seal` (of the deepest surface), `anchor_yields_to_engine` (of the deepest surface).

---

### 5.5 S3 and S4 — barrier and hooks

**S3.** If the anchor surface's seal is `Barrier`, dispatch returns `Ignore` immediately. No ancestor is consulted, no hook runs, no lookup happens. Two surfaces are barriers: `foreign` (a non-attached `CodeEdit`, a plain `TextEdit`, a `LineEdit` with no sibling nav control) and `editor.insert`. This is the structural form of `FocusContext::Foreign => false` (`input.rs:90`) and of `if !is_nav_mode { return false; }` (`input.rs:103-105`), with Select mode staying insert-like per `input.rs:95-102`.

`editor.insert`'s probe is written as the exact **complement** of `editor.nav` within "focus is the attached CodeEdit" — `chain.editor_mode.is_some_and(|m| !is_nav_mode(m))` — not as an enumeration of `Insert | Replace | VirtualReplace | CommandLine | Select`. `vim_core::primitives::Mode` is `#[non_exhaustive]` (`vim-core/src/primitives/mode.rs:90-112`); two positive probes over a non-exhaustive enum cannot be shown total, and a future variant matching neither would fall through to `foreign`, become a Barrier, and leak Ctrl+hjkl. Written as a negation, totality is a tautology (`A ∧ P` xor `A ∧ ¬P`) rather than a test. `editor_mode == None` maps to `editor.nav`, transcribing the `is_none_or` at `input.rs:92` whose intent is "no controller means intercept".

**S4.** For each surface on the path, `on_key` runs once — before any lookup, regardless of whether a binding matches. There is exactly one today: `dock.filesystem`'s stale-prompt auto-dismiss, verbatim from `filesystem_explorer.rs:74-80`. It belongs to no binding and would otherwise be lost by the extraction.

Moving it here changes *when* it runs, and that change is an improvement to be pre-registered rather than avoided. Today it is unreachable for Ctrl+hjkl, because `should_intercept_hjkl` returns at `input.rs:133` before the `FocusContext::Dock` arm ever calls `handle_key`. So today, opening the FS create prompt (which grabs focus, `filesystem_explorer.rs:232`) and pressing Ctrl+L jumps focus to another panel while the prompt stays visible with its `active_control` still pointing at the old FileSystem Tree — and whenever `dismiss_prompt` eventually runs it will `call_deferred("grab_focus")` back to that stale Tree (`filesystem_explorer.rs:262-269`), stealing focus from wherever the user has moved. Running the hook at S4 dismisses the orphan. Everything else is preserved verbatim, including that it fires for *modified* keys — `handle_key` calls `validate_cache()` and the dismiss at `:74-80` **before** the ctrl/alt/meta filter at `:82`, so Alt+X in the FS dock already auto-dismisses today.

`on_key` implementations must be idempotent and cheap: they run on key-repeat echo events too (§5.8), and the fallback transport can deliver a declined primary-viewport key a second time if `is_primary_viewport` is ever removed.

---

### 5.6 S5 — surface path resolution and the candidate walk

The walk is leaf→root over `path.ids`, and for each surface it tries the probes in order. It returns both the candidate list and **the `KeyEvent` that matched**, because S6 must be evaluated on that key and not on a re-derivation from the logical keycode.

```rust
// src/actions/resolve.rs
fn walk_path(input: &ResolveInput<'_>) -> Option<(Candidates, KeyEvent)> {
    for &surface in input.path.ids.iter() {                 // deepest → root
        for probe in ordered_probes(input.probes, surface, input.index) {
            match input.index.lookup(surface, &[probe]) {
                // ExactOnly yields exactly ONE entry, whose RHS is one opaque
                // slot key — so this is `rule_at`, singular. Plurality comes
                // from the WALK, never from an LHS.
                TrieLookup::ExactOnly(entry) => {
                    // `slot_of` reads the entry's single-element RHS, which is
                    // always `KeyEvent::action(slot)` (§4.7 `insert_at`);
                    // `MappingEntry::sequence` is pub at trie.rs:183.
                    let rule = input.index.rule_at(slot_of(entry));
                    let mut out = Candidates::new();
                    match &rule.target {
                        RuleTarget::Native => return None,   // walk TERMINATOR
                        RuleTarget::Action(id) => {
                            // `requires` lives on the ActionSpec, not on Rule:
                            // resolve the target before gating (§4.5, §4.7).
                            let spec = input.registry.spec(*id);
                            if !input.path.caps.contains(spec.requires) {
                                continue;                    // capability gate
                            }
                            out.push(spec, rule.params.clone(), rule.consume);
                        }
                        // No ActionSpec, therefore no `requires`, therefore no
                        // capability gate — stated so it is not an omission.
                        RuleTarget::Shortcut(p) =>
                            out.push_shortcut(p.clone(), rule.consume),
                    }
                    return Some((out, probe));
                }
                TrieLookup::Prefix { .. } => { /* §5.10 */ }
                TrieLookup::NoMatch => {}
                _ => {}                     // TrieLookup is #[non_exhaustive]
            }
        }
        if input.path.seal == Seal::Sealed && !input.probes.has_modifier() {
            return None;                    // sealed: bare keys stop at the anchor
        }
    }
    None
}
```

Four things this encodes.

**`ordered_probes` is where `physical` opt-in lives.** Probes 1 and 2 are offered for every surface; probe 3 is offered only if the index holds a `physical`-flagged rule on that surface. A rule flagged `physical` whose LHS is multi-key applies the flag to the first key only, which is the only key that can enter the trie before the rule is known — stated explicitly because the interaction was previously undefined.

**The capability gate is a plain subset test, and the vocabulary is five affordances.** `Caps` is `VNAV | HIERARCHY | ACTIVATE | TEXTENTRY | FILEOPS`. There is no `LIST` and no `SCROLL`: `LIST` named a widget taxonomy, not an affordance, and a subset test cannot express "LIST or SCROLL". `handle_navigation` has exactly three arms — Tree, ItemList, RichTextLabel (`src/navigation/dock_nav.rs:105-119`) — and today `j`/`k` has **no** widget gate at all (`dock.rs:111-126` calls it unconditionally). The affordance an action needs is "answers vertical next/prev", held by all three classes including `RichTextLabel`, whose arm scrolls 50px and returns `true` (`dock_nav.rs:284-298`). So Tree contributes `VNAV|HIERARCHY|ACTIVATE`, ItemList `VNAV|ACTIVATE`, RichTextLabel `VNAV`, LineEdit `TEXTENTRY`; `godotvim.item.next/prev` requires `VNAV` and `j`/`k` keep scrolling the EditorHelp docs panel and the Output log. `PANEL`, `ESCAPE` and `SEARCHBOX` are deleted as tautologies or dead gates — `PANEL` was granted by the forest root to everything, `ESCAPE` had no possible grantor (`handle_escape_from_dock` already declines when the script editor is missing, `dock.rs:243-249`), and `SEARCHBOX` duplicated `handle_slash`'s own decline. `FILEOPS` survives because it is genuinely non-tautological: without it, `panelmap dock a godotvim.fs.create` on a Scene tree would create files at `res://` root, since `get_selected_path` returns `None` for a non-FS Tree and `begin_create` falls back to `"res://"` (`filesystem_explorer.rs:126-130`).

The gate reads `ActionSpec::requires`, reached by resolving the rule's `RuleTarget::Action(id)` through the registry — `Rule` carries no `requires` of its own, which is why `native` and `<Shortcut>` targets are never gated. A gated-out candidate is skipped *as if `NoMatch`* and the walk continues. That is what makes `h`/`l` inert on an `ItemList` with zero widget knowledge in the dispatcher, replacing the `matches!(dock_kind, DockKind::Tree)` gates at `dock.rs:128,112`.

**`RuleTarget::Native` terminates the walk; it is not a declining action.** This distinction is load-bearing and easy to get wrong. If `native` were modelled as an `ActionSpec` returning `Declined`, the walk would continue to `panel`'s `<C-h>` rule — which is `Consumption::Void` — and consume the key anyway, silently defeating the documented escape hatch. `RuleTarget::Native` returns `Resolution::None` at the surface where it is declared, so `panelmap dock.filesystem <C-h> native` gives Ctrl+H back to Godot inside the FileSystem dock while leaving it bound everywhere else. `panelunmap` is a *different* verb: it removes a rule and lets the walk continue to the parent.

**Exactly one rule per `(surface, lhs)`.** This is forced by the trie, not chosen: `MappingTrie::insert` writes `node.entry = Some(entry)` (`vim-core/src/keymap/trie.rs:345-348`) and `TrieLookup::ExactOnly(&MappingEntry)` yields one entry, so a lookup on one LHS can surface exactly one slot. Candidate **plurality comes from the forest walk** — one candidate per surface on the path — never from a single LHS. Conflating the two sources of plurality is what would destroy `panelunmap` and last-writer-wins.

---

### 5.7 S6 — the editor arbitration seam

This is the highest-stakes stage in the model, because getting it wrong breaks either cross-panel navigation from the editor or the user's own `:map`. Three moves make it small.

### Move 1 — `Arbitration` is a property of the surface, not of a rule

`Arbitration { Claim, Yield }` is deleted from `Rule`, and the `yield` token is deleted from the `panelmap` grammar. It becomes one bool on `SurfaceSpec`:

```rust
pub(crate) struct SurfaceSpec {
    pub(crate) id: SurfaceId,
    pub(crate) parent: Option<SurfaceId>,
    pub(crate) seal: Seal,
    pub(crate) grants: fn(&FocusChain) -> Caps,
    pub(crate) probe: Probe,
    pub(crate) on_key: Option<fn(&mut ActionCtx<'_>)>,
    /// `true` on `editor.nav` ONLY. Verbatim transcription of
    /// src/plugin/input.rs:106-118: today's `should_intercept_hjkl` is
    /// computed from the CONTEXT, before any binding is consulted.
    /// Not settable from config — authored in provider code.
    pub(crate) yields_to_engine: bool,
}
```

This is a faithful transcription, not a simplification. `should_intercept_hjkl` (`input.rs:88-123`) is computed from `context`, before any binding exists, and it suppresses the consumption at `:124-135` wholesale. Arbitration was never a property of a binding.

Four consequences follow directly:

- **`editor.nav` carries zero rules.** The `<C-h>` rule exists exactly once, on `panel`, which is `editor.nav`'s declared forest parent — so the walk from the editor reaches it. The duplication gap ("if `editor.nav` binds only `<C-l>`, then `<C-h>` falls through to `panel`'s `Claim` rule and the `:map` escape hatch silently disappears for three of four keys") becomes **unrepresentable**, not asserted by a startup check that a user's `panelunmap` would break.
- A user rebinding to `panelmap panel <M-h> godotvim.focus.left` inherits the escape hatch for free.
- "`Yield` on a non-editor surface" — which would be actively harmful, since with no controller `is_none_or` means *intercept* and there is no focused `CodeEdit` to fall through to — becomes inexpressible rather than rejected-and-downgraded at registration.
- One of the model's axes disappears.

The gate runs once, on the anchor surface, after resolution has produced a winner and before any candidate executes:

```rust
pub(crate) fn resolve(input: &ResolveInput<'_>) -> Resolution {
    let Some((hits, matched)) = walk_path(input) else { return Resolution::None };
    if input.path.anchor_yields_to_engine && (input.vim_claims)(matched) {
        log::trace!("resolve: yielding {matched:?} to the vim engine");
        return Resolution::None;      // → Disposition::Ignore → flows to gui_input
    }
    Resolution::Run(hits)
}
```

It is evaluated on `matched` — the key the winning probe produced — so `:nnoremap <C-h> x` still wins on a Cyrillic layout, where the rule matched via the physical probe. Evaluating a re-derivation from the logical keycode would silently fail exactly there.

### Move 2 — `vim_claims` is *exactly* `could_start_mapping`

```rust
// src/plugin/input.rs — transport. Verbatim transcription of :92 + :117:
//   should_intercept = controller.is_none_or(|c| !c.could_start_mapping(k))
//   claims           = !should_intercept
//                    = controller.is_some_and(|c| c.could_start_mapping(k))
let claims = |k: vim_core::keymap::KeyEvent| {
    self.controller.as_ref().is_some_and(|c| c.could_start_mapping(k))
};
```

The `is_none_or` → `is_some_and` flip is on the *inverted* predicate, and preserving it is what keeps "no controller ⇒ intercept" true. Get the polarity wrong and the plugin stops navigating panels in exactly the state where nothing else can either.

`could_start_mapping` is `!matches!(keymap.lookup(mm, &[key]), TrieLookup::NoMatch)` (`vim-core/src/execution/engine/mapping.rs:78-82`), reached through `VimController::could_start_mapping` (`src/controller/mod.rs:677-679`), which delegates to `engine()` and therefore works in both `Attached` and `Detached` phases. Because `TrieLookup::Prefix != NoMatch`, it already covers user mapping *prefixes*, not just exact matches — a user with `:nnoremap <C-h><C-h> …` keeps the key.

The closure borrows `self.controller` immutably during `resolve`. `resolve` returns owned data, so the borrow ends before the executor takes `&mut self`. This is precisely why arbitration must live *inside* `resolve()` and not in the execute loop.

### Why the `KeyInterestSet` refinement was wrong (F1)

The design originally defined `vim_claims(key) = could_start_mapping(key) || key ∈ KeyInterestSet[mode]`, on the reasoning that `could_start_mapping` cannot see `<C-w>`, which is grammar rather than a mapping. That disjunct is a **silent, asymmetric regression**, and it does not close the hole it was invented for.

The regression: `compute_key_interest` seeds `normal` from `core_entries(Normal)` + `core_entries(Operator)` and `visual` from `core_entries(Visual)`, for every key whose class is not `Unknown` (`vim-core/src/execution/engine/key_interest.rs:188-202`). `CoreKeymap::add_shared_motion_keys` inserts `KeyEvent::ctrl('h')` and `KeyEvent::ctrl('j')` as `KeyClass::Motion` (`vim-core/src/keymap/core.rs:121-122`) and is called from `build_normal_map` (`:155`), `build_operator_pending_map` (`:279`) and `build_visual_map` (`:333`) — exactly the three modes in which `editor.nav` is live. So `vim_claims(<C-h>)` and `vim_claims(<C-j>)` would be unconditionally true, and Ctrl+H / Ctrl+J would never navigate panels from the editor again. `ctrl('k')` and `ctrl('l')` appear nowhere in `CORE_KEYMAP`, so two of four keys would keep working and the breakage would be invisible in casual testing. Today's code is correct precisely because `Keymap::lookup` merges the buffer/filetype/global *user* tables and never reads `CORE_KEYMAP` (`vim-core/src/keymap/keymap.rs:605-618`).

The insufficiency: `vim_claims` is consulted **only** on the yield path. A `Claim` rule on `<C-w>` — the default arbitration — would steal the key no matter what the disjunct returned. The disjunct was therefore both a regression and unable to serve its own stated purpose.

There is also a shape problem worth recording: `KeyInterestSet`'s fields are `pub normal: Vec<String>` of Vim-notation strings (`key_interest.rs:43-53`), not a keyed set of `KeyEvent`, and there is no `OperatorPending` bucket (`normal` covers it). Membership costs a `to_vim_notation()` allocation — it returns `Cow<'static, str>` that allocates for `Char` keys — plus a binary search per keystroke. It is not the O(1) primitive the resolver signature implied. Related: `VimSession::take_key_interest_if_dirty`, cited in three places as the caching mechanism, is declared in `impl VimSession<SessionHost>` (`vim-core/src/execution/session_host.rs:1103`, with no column-0 `}` before `:2326`); godot-vim holds `VimSession<GodotHost>` (`src/controller/mod.rs:94`) and cannot call it. Deleting the disjunct deletes that whole caching problem.

### Move 3 — the real `<C-w>` hole closes at registration, via vim-core's own parser

`<C-w>` is a genuine hazard: it is in `CORE_KEYMAP` (`vim-core/src/keymap/core.rs:208-213`) as `KeyClass::Action` — *not* `Prefix`, which is why a class-based denylist cannot find it — and it owns a 19-command grammar family. A single-key `panelmap panel <C-w> …` or `panelmap editor.nav <C-w> …` line consumed at `_input()` turns `<C-w>s` into a bare `s`: substitute-character, a destructive edit, with no replay channel to recover the lost key.

It is a **registration**-time question, not a dispatch-time one, and vim-core will answer it authoritatively. `vim_core::grammar::Parser` is fully public (`grammar/mod.rs:113`, `parser.rs:99`/`:107`) and lives in a layer architecturally forbidden from importing `execution`, `effects` or `commands` (`grammar/mod.rs:9-11`) — the same argument that licenses using `MappingTrie`.

```rust
// src/actions/keys.rs
const NAV_MODES: [Mode; 3] = [
    Mode::Normal,
    Mode::Visual(VisualType::Char),
    Mode::OperatorPending(Operator::Delete),
];

/// True when `key` puts vim-core's grammar into an `Awaiting*` state — i.e.
/// consuming it at `_input()` destroys the follow-up key.
pub(crate) fn starts_vim_grammar_sequence(key: KeyEvent) -> bool {
    let keymap = Keymap::new();   // core defaults only; user maps are
                                  // `could_start_mapping`'s job, not this one
    NAV_MODES.iter().any(|&mode| {
        [false, true].into_iter().any(|sneak| {
            let mut parser = Parser::new();
            parser.set_sneak_mode(sneak);                    // parser.rs:404
            parser.process(key, &keymap, mode).is_pending()  // result.rs:60-62
        })
    })
}
```

Traced against v0.7.1 source: `<C-w>` → `Continue(AwaitingWindowCommand)` (`grammar/handlers/ready.rs:141-150`) → true. `<C-\>` → true, intercepted before classification at `grammar/parser.rs:129-133` and **absent from `CORE_KEYMAP` entirely** (`grep -n backslash vim-core/src/keymap/core.rs` → zero hits) — which is the prefix a hand-maintained denylist or a `CoreKeymap`-pinned test would miss. `<C-h>`/`<C-j>` → `KeyClass::Motion` → `handle_ready_motion` (`ready.rs:358-386`) → `Execute` or `Invalid`, neither pending → false. `<C-k>`/`<C-l>` → `KeyClass::Unknown` → `_ => Invalid` (`ready.rs:73`) → false. Bare digits return true (`Continue(Ready{count})`), which conservatively forbids `panelmap panel 3 …` from breaking `3j`.

The registration guard is applied on any surface that is **an ancestor-or-self of an `editor.*` surface**, not merely on `editor.*` itself — because `panel` is `editor.nav`'s declared parent and a `panel` rule is live while the attached CodeEdit has focus. It covers **single-key** LHS as well as multi-key, because `<C-w>` alone is the whole bug, and it subsumes the separate "reject multi-key on editor surfaces" rule into one predicate:

```rust
impl BindingIndex {
    fn editor_reachable(&self, surface: SurfaceId) -> bool {
        self.forest.ids().filter(|id| id.starts_with("editor."))
            .any(|ed| self.forest.is_ancestor_or_self(surface, ed))
    }

    pub(crate) fn try_insert(&mut self, rule: Rule) -> Result<(), RuleReject> {
        if self.editor_reachable(rule.surface) {
            if rule.lhs.len() > 1 {
                return Err(RuleReject::MultiKeyOnEditorPath(rule.surface));
            }
            if crate::actions::keys::starts_vim_grammar_sequence(rule.lhs[0]) {
                return Err(RuleReject::VimGrammarPrefix(rule.lhs[0]));
            }
        }
        self.upsert(rule);
        Ok(())
    }
}
```

Rejections are warn-and-skip, mirroring `src/settings/reader.rs:240-263`, and surface in `:checkhealth godotvim`.

The two halves compose to total coverage: **core grammar prefixes are caught at registration** by the parser probe; **user mapping prefixes are caught at dispatch** by `could_start_mapping`. Neither can rot — the first asks vim-core's real state machine, the second is the check that already ships.

### The no-focus-owner case, and why `Anchor` exists

`src/plugin/input.rs:126-134` is a hard constraint: when `viewport.gui_get_focus_owner()` returns `None`, today's code skips `handle_window_nav` entirely and still calls `viewport.set_input_as_handled(); return;` unconditionally. Ctrl+hjkl is consumed with no focus owner and no target found.

With `probe: fn(&FocusChain) -> Option<usize>` returning a chain index, that case is *unconstructible*: an empty `chain.nodes` admits no valid index, and there is no `Gd<Control>` to place in a non-optional `ActionCtx.target`. `Anchor::Rootless` plus `target: Option<Gd<Control>>` makes it constructible:

```rust
// src/actions/providers/unknown.rs — the unconditional catch-all arm.
static UNKNOWN: SurfaceSpec = SurfaceSpec {
    id: "unknown",
    parent: Some("panel"),
    seal: Seal::Open,
    grants: |_| Caps::empty(),
    probe: |chain| Some(match chain.nodes.first() {
        Some(_) => Anchor::Node(0),
        None    => Anchor::Rootless,
    }),
    on_key: None,
    yields_to_engine: false,
};
```

This reproduces `classify_focus`'s `let Some(focus_owner) = … else { return FocusContext::Unknown }` (`focus.rs:46-48`) and `FocusContext::Unknown => true` (`input.rs:120-122`). The path becomes `[unknown, panel]`, so `panel`'s `<C-h>` rule is a live candidate. Capabilities still resolve: `Rootless` contributes no widget caps, and `godotvim.focus.*` requires `Caps::empty()` — the old `requires: PANEL` was a tautology. The executor opens with `let Some(target) = cx.target().cloned() else { return Outcome::Declined };`, which is the verbatim transcription of `input.rs:127-130`, and `Consumption::Void` supplies the consume that `:132` supplies today.

---

### 5.8 S7 and S8 — execution and consumption

Candidates run deepest-first. The first non-`Declined` outcome stops the walk. Consumption is computed **downstream** of the outcome, from the winning rule's declared policy — never by the action, which is the fifth joint the old match arms fused.

```rust
let mut disposition = Disposition::Ignore;
for (spec, params, consume) in plan {
    let outcome = self.run_action_now(spec, &params, target.clone());
    if consume == Consumption::Void {
        // input.rs:129 discards handle_window_nav's result; :132-133 consumes
        // and returns regardless. Void therefore consumes AND stops the walk —
        // even on Declined, even when `run` short-circuited on target == None.
        disposition = Disposition::Consume;
        break;
    }
    if outcome.accepted() { disposition = Disposition::Consume; break; }
    // Declined + Elastic → next candidate (precedence 12)
}
if disposition == Disposition::Consume { viewport.set_input_as_handled(); }
```

- **`Elastic`** (the default, and every non-`panel` rule) consumes iff the action accepted. This is what preserves the two tri-state returns the whole design rests on: `j` at the end of an `ItemList` (`dock.rs:113-126`) and Enter with nothing selected (`dock.rs:220-222`) are not consumed, so Godot's Tree type-to-search and arrows (`gui_input`) and the docks' F2/Delete accelerators (`shortcut_input`) still see the key.
- **`Void`** (the four `panel` Ctrl+hjkl rules) consumes regardless of outcome **and terminates the walk**. That termination is not cosmetic: it is the difference between transcribing `input.rs:132-133` and inventing something. The cost is real and should be stated — a `Void` rule is a hard key sink on every surface it is registered on, and the way out is `panelmap <surface> <key> native`, not `panelunmap`.

`Outcome` is `Handled | FocusChanged | Declined` with `accepted() == !matches!(self, Declined)`. `FocusChanged` is `Handled` for consumption purposes; the distinction exists for the callers that need focus bookkeeping.

**Where `run_action_now` gets its borrows.** Every shared handle leaves `self` in its own statement *before* `ActionCtx { plugin: self, … }` is constructed — the NLL discipline the shipped code already depends on at `src/plugin/input.rs:284-288`, where `&mut self.controller` is released after `take_pending_ui_actions()` so `self.handle_pending_ui_action(action)` can take `&mut self` in the same `if let` body. Concretely: `Rc::clone(&self.focus_chain)` is a refcount bump on the plugin's cached chain (§4.10), the `&'static ActionSpec` is *copied* out of `registry.specs: Vec<&'static ActionSpec>` so no registry borrow survives, and `viewport` is owned — `Node::get_viewport()` returns an owned `Gd<Viewport>` and `Gd<T>` is a handle, which is why `input.rs:279-281` already compiles today doing exactly this. The plane is a plain field, not `Rc<RefCell<…>>`, precisely so that no guard can span `(spec.run)(…)`; what remains uncheckable by the compiler is that every shared handle really does leave `self` in its own statement, and §5.11 is why re-entrancy is possible at all.

### Echo and key repeat

Godot key-repeat events report `is_pressed() == true`, and there is no `is_echo()` filter anywhere in the plugin today — so holding Ctrl+J currently fires a ~20/s storm of deferred `grab_focus` calls. `Repeat` is a per-rule policy, not a global filter, because held `j`/`k` auto-repeat in a dock is desirable and must be preserved.

`Repeat` is consulted after the winner is known but before `run`:

- **`Repeat::Allow`** (every rule except the four `panel` focus rules): the echo runs the action normally.
- **`Repeat::Suppress`** (the four `panel` focus rules): the echo **consumes without running**. Not `Ignore` — returning `Ignore` would leak the repeated Ctrl+J to Godot's own handling, which today's unconditional `set_input_as_handled()` at `input.rs:132` never does. Suppress kills the `grab_focus` storm while keeping the key out of Godot's hands.

`on_key` hooks at S4 still run on echo events, because the winner is not yet known when they run. They are required to be idempotent, which the one shipped hook is (`prompt_is_active()` guards the dismiss).

**Interaction with the pending buffer.** This was previously unspecified, and the failure it admits is concrete: an echo arriving mid-sequence on a `Repeat::Allow` rule would push a duplicate key and turn `gg` into `ggg`. The rule is therefore: **while `pending` is non-empty, echo events are consumed and discarded.** They do not extend the buffer, they do not re-run a candidate, and they do **not** restart the shell timer — if they did, holding a reserved prefix key would keep the timer alive indefinitely and the prefix would never resolve. A held key means "do that again", never "type another key"; the prefix is already reserved and therefore already consuming, so discarding costs nothing and leaks nothing.

---

### 5.9 Precedence

Total, deterministic, and printed verbatim by `:panelmap <lhs>`. Every tiebreak below is declared data, never array position and never a magic integer.

1. **Transport guards.** `enabled`, `InputEventKey`, `is_pressed`, bare-modifier keycode filter (`input.rs:33-76`). Not a policy layer — the event never enters the model.
2. **Injection guard.** An event matching a live injection fingerprint is dropped *without consuming*, so Godot's own handler still receives it (§5.11).
3. **Barrier.** If the anchor surface's seal is `Barrier` (`foreign`, `editor.insert`), return `Ignore`. Nothing below runs.
4. **Deepest declared surface wins**, walking the forest path leaf→root: `dock.filesystem` > `dock` > `panel`. Depth is authored in `SurfaceSpec::parent`, validated at registration, printed by `:panelmap`. It is never scene-tree depth — Godot's focus chain is generic-at-the-leaf and specific-at-the-ancestor (`dock` comes from `gui_get_focus_owner()`, `dock.filesystem` from `FileSystemDock::is_ancestor_of`, `filesystem_explorer.rs:380-386`), so scene depth would invert FileSystem-first refusal.
5. **Probe order within a surface**: as-typed → `latin_key` → physical-position, the last only for `physical`-flagged rules. First hit wins.
6. **Seal.** At a `Sealed` anchor (`searchbox`, `prompt`), a *bare* key that matched nothing stops the walk and falls to the control's own `gui_input`; a key bearing CTRL/ALT/META continues to the forest root.
7. **Capability gate.** A candidate whose `requires` is not a subset of `path.caps` is skipped as if `NoMatch`; the walk continues.
8. **Trie semantics within one surface.** `ExactOnly` fires. `<nowait>` short-circuits inside `lookup()` itself (`vim-core/src/keymap/trie.rs:445`). A longer mapping makes its first key a reserved prefix.
9. **Same `(surface, lhs)`: last writer wins.** Load order is exactly two layers — builtin provider defaults (`MappingOwner::Host(tag)`, in `PROVIDERS` order), then the **single** resolved vimrc in file line order. There is no user-then-project merge, because `config::path::resolve` returns exactly one file (`src/config/path.rs:23-61`): the EditorSettings override if set, else `res://.godot-vimrc`, else `user://.godot-vimrc`, as three mutually exclusive early returns. Owners are recorded so `:panelmap <lhs>` reports *why* a rule won.
10. **`RuleTarget::Native` terminates.** Walk stops, nothing consumed. Distinct from `panelunmap`, which removes a rule and lets the walk continue.
11. **Arbitration.** If the anchor declares `yields_to_engine` and `vim_claims(matched_key)` holds, abandon dispatch and return `Ignore`; the key flows on to `gui_input` and the engine.
12. **Declination.** `Declined` continues to the next candidate. Exhausting the list means not consumed.
13. **Consumption policy.** `Elastic` consumes iff accepted; `Void` consumes regardless and stops the walk.

---

### 5.10 Pending prefixes and the shell timer

Multi-key sequences exist only outside the editor path. `BindingIndex::try_insert` rejects any multi-key LHS on a surface that is an ancestor-or-self of an `editor.*` surface (§5.7), which includes `panel` — so the shell plane can never hold a pending prefix while the sampled leaf is the editor. `<C-w>s`, `gg`, `gU`, `gv` remain vim-core's, where `:map` already works.

**Reservation, not speculation.** Godot's `_input()` stage has no synchronous replay channel: `Input::parse_input_event` buffers under `use_accumulated_input` and a consumed key is destroyed forever. So a prefix key is consumed **only** when the user has explicitly bound a sequence starting with it on that surface. Binding `dd` on `dock.filesystem` implicitly reserves bare `d` there; the reservation is printed by `:panelmap` and by `:checkhealth`, never inferred silently.

The resolution rules, in order:

1. `pending` empty and the key **not reserved** on this surface: single-key exact lookup only. **No state is created and nothing is consumed on a miss.** This is why `g` then `o` still reaches Godot's Tree incremental type-to-search when nothing reserves `g`.
2. Key **is** reserved, or `pending` is non-empty: push (capped at `MAX_KEY_SEQUENCE_LEN = 8`, `vim-core/src/keymap/keymap.rs:140` — the shell caps at the same value deliberately so the two planes cannot disagree), then `lookup(&pending)`.
   - `ExactOnly` → candidates, clear `pending`.
   - `Prefix { exact }` → consume, buffer, arm the shell timer at `timeoutlen`, remember `exact` as the timeout fallback.
   - `NoMatch` → clear `pending` and **consume this key too**. A reserved prefix owns its whole subtree; otherwise the terminating key leaks into Tree incremental search. This is a deliberate divergence from Vim, which would flush both as literals, and it is reported by `:checkhealth` rather than left for a user to discover.
3. For any control whose surface reserves a bare prefix, `Tree::set_allow_search(false)` / `ItemList::set_allow_search(false)` is applied to **that control only**, restored on focus change, on teardown and inside `panic_guard` recovery. This removes the type-to-search conflict at its source instead of racing it.

`pending` clears on execute, on `NoMatch`, on timeout, on focus-owner change, on plugin disable, and on config reload — and deliberately **not** on echo, which is consumed and discarded with the buffer left intact (§5.8).

**The timer is a second `Gd<Timer>`, never `self.mapping_timer`.** `on_mapping_timeout_impl` opens with `let Some(editor) = &self.attached_editor else { return; }` (`src/plugin/input.rs:310-313`) — it early-returns with no editor attached, which is the common case for dock browsing, and if it *did* run it would flush the **engine's** typeahead into the open file while focus is on a dock. The shell timer is cloned from `init_mapping_timer` (`src/plugin/mod.rs:130`) into a separate field.

**Where `timeoutlen` comes from.** Attached, it is `controller.timeoutlen()` → `self.engine().timeoutlen()` (`src/controller/mod.rs:682-684`), which works in both `Attached` and `Detached` phases since `engine()` is available in both. Detached — no controller reachable at all — the source is `SettingsSnapshot.timeoutlen` (`src/settings/snapshot.rs:150`), the same `i64` that is clamped and fed into `VimOptions::set_timeoutlen_ms` at `snapshot.rs:194-196`. Naming this matters because the headline capability here is "a dock prefix resolves with no script open". One honest caveat: the field's existence and its feed into `VimOptions` are verified; that it is refreshed on **every** path that changes the EditorSettings value is not, and should be confirmed before the sequence phase relies on it.

---

### 5.11 Re-entrancy guard for shortcut delegation

`RuleTarget::Shortcut(path)` and the `godotvim.fs.{delete,rename}` executors both delegate to one of Godot's own registered editor shortcuts by cloning its `InputEventKey` and re-injecting it via `Input::parse_input_event` — the path proven at `src/navigation/filesystem_explorer.rs:437-472`. That re-enters the same pipeline that dispatched it.

The hazard is not a slow loop; it is a hard hang. `Input::flush_buffered_events` is `while (buffered_events.front()) { pop_front(); _parse_input_event_impl(e); }` (`core/input/input.cpp:1630-1643`) and `parse_input_event` **appends to that same list** when `use_accumulated_input` is true, which is the default (`input.cpp:1611-1614`, `input.h:167`). So an event injected from inside `_input` is re-dispatched by the same flush call in the same frame. `panic_guard` gives zero protection: a livelock is not a panic. A rule binding `<F2>` to `<Shortcut>(filesystem_dock/rename)` — whose accelerator *is* F2 — hangs the editor, and so does `panelmap dock.filesystem <F2> godotvim.fs.rename` through a Rust action rather than a shortcut target. Today's defaults are safe only by accident: `d`/`r` differ from Delete/F2.

Three layers, all required:

- **Layer 1 — registration-time cycle rejection.** `ActionSpec` carries `delegates: Option<&'static str>` so delegation is statically known rather than discovered inside `run`; that is what makes the check total across both `<Shortcut>` rules and Rust actions. At every index build, `audit_shortcut_cycles` resolves each delegated path's event array, builds edges from each rule's canonicalized first LHS key to every `InputEventKey` in the target shortcut, and rejects any rule on a cycle (self-collision is the length-1 case) with warn-and-skip plus a `:checkhealth` diagnostic. Surfaces are deliberately ignored — the injected key is re-dispatched against a freshly sampled chain, so a surface-aware check would be unsound.
- **Layer 2 — runtime fingerprint suppression (stage S0.5).** Every injection is stamped with `(get_keycode_with_modifiers(), get_physical_keycode_with_modifiers(), Engine::get_process_frames())`. S0.5 drops matching events for `INJECT_SUPPRESS_FRAMES = 2` frames, where *drop* means **return without consuming** — Godot's own handler must still receive it, since that is the entire point of delegating. The fingerprint is the `Key` pair, not the parsed `KeyEvent`: `translate_key` consults `get_unicode()` (`src/bridge/input.rs:446-456`) which a synthesized event never sets, so a `KeyEvent` fingerprint would be unstable for printable accelerators. A `device` marker is not used — `DEVICE_ID_*` semantics changed between 4.5 and 4.8-dev.
- **Layer 3 — a hard per-frame injection budget** (`INJECT_BUDGET_PER_FRAME = 4`), the backstop for cycles Layer 1 cannot see because a user rebound the shortcut in Editor Settings after the audit ran. Exhaustion logs once and drops.

An `injecting: Cell<bool>` in-flight flag does not work and must not be substituted: `parse_input_event` returns immediately (the event is only queued), so the flag is already cleared when the re-dispatch arrives, and an unkeyed bool would suppress unrelated keystrokes.

Two residuals. The 2-frame window can swallow a genuine user press of the same accelerator within ~33ms of a delegated injection; judged negligible because the rename dialog grabs focus first, but it is a real keystroke loss. And every shortcut-delegating call is gated on `has_shortcut_api()` (a cached `settings.has_method("get_shortcut")`) and routed through `try_call` rather than `call`, because `EditorSettings::get_shortcut` was ClassDB-bound only in 4.6 while `addons/godot_vim/godot_vim.gdextension:3` declares `compatibility_minimum = "4.5"`, and gdext generates vararg `call()` as `try_call().unwrap_or_else(|e| panic!("{e}"))` — an unguarded call **panics** on 4.5, it does not return nil. Absent the API, `run_editor_shortcut` returns `Outcome::Declined`, the key is not consumed, and Godot's native Delete/F2 accelerators still fire.

---

### 5.12 Worked traces

Each row: keystroke and context → sampled chain → surface path (deepest first) → winning probe and candidate → outcome → disposition.

| # | Keystroke / context | Chain (leaf→root, abridged) | Surface path | Probe → candidate | Outcome | Consumed? |
|---|---|---|---|---|---|---|
| 1 | `d`, FileSystem dock Tree focused | `[Tree(VNAV\|HIER\|ACT), …, FileSystemDock]`, `in_filesystem_dock: true` | `[dock.filesystem, dock, panel]`, caps `VNAV\|HIER\|ACT\|FILEOPS` | p1 `d` → `dock.filesystem d` → `godotvim.fs.delete` (needs `FILEOPS` ✓) | `Handled` | **Yes** — Elastic + accepted |
| 2 | `j`, same context | same | same | p1 `j`: `dock.filesystem` `NoMatch` → walk continues → `dock j` → `godotvim.item.next` (needs `VNAV` ✓) | `Handled` | **Yes**. FileSystem-first refusal is now precedence, not the `if fs_result.is_consumed()` branch at `input.rs:139-147` |
| 3 | `j`, Script list `ItemList`, last item already selected | `[ItemList(VNAV\|ACT), …]` | `[dock, panel]` | p1 `j` → `dock j` → `item.next` | `Declined` (`handle_navigation` returns false, `dock.rs:113-119`) | **No** — walk exhausts; Godot's ItemList sees the key |
| 4 | `l`, same `ItemList` | same | same | p1 `l` → `dock l` → `item.expand` requires `HIERARCHY`; path caps are `VNAV\|ACT` → **gated out**, treated as `NoMatch`; `panel` has no `l` | — (never ran) | **No** — replaces the `DockKind::Tree` gate at `dock.rs:136` |
| 5 | `j`, EditorHelp `class_desc` RichTextLabel | `[RichTextLabel(VNAV), …]` | `[dock, panel]` | p1 `j` → `dock j` → `item.next` (needs `VNAV` ✓) | `Handled` (scrolls 50px, `dock_nav.rs:284-298`) | **Yes**. Under the old `Caps::LIST` gate this was a silent regression on two shipped surfaces (docs panel and the `:Output` log) |
| 6 | `<C-h>`, **no focus owner at all** | `nodes: []` | `unknown` probes `Anchor::Rootless` → `[unknown, panel]`, caps empty | p1 `<C-h>` → `panel <C-h>` → `godotvim.focus.left` (requires `Caps::empty()` ✓) | `Declined` — `cx.target()` is `None`, so `handle_window_nav` is skipped, verbatim `input.rs:127-130` | **Yes** — `Consumption::Void` consumes and stops the walk, verbatim `input.rs:132-133` |
| 7 | `<C-h>`, attached CodeEdit focused, Normal mode, user has `:nnoremap <C-h> x` | `[CodeEdit]`, `attached_editor: Some(id)`, `editor_mode: Some(Normal)` | `[editor.nav, panel]`; `editor.nav` carries **zero** rules and sets `yields_to_engine` | p1 `<C-h>` → `editor.nav` `NoMatch` → `panel <C-h>` → `focus.left`; `matched = <C-h>` | **S6 gate fires**: anchor yields ∧ `vim_claims(<C-h>)` = `could_start_mapping` = true → `Resolution::None` | **No** — flows to `gui_input`, engine runs `x`. Without the mapping the gate is false and focus moves left |
| 8 | `<C-l>`, dock filter `LineEdit` focused | `[LineEdit(TEXTENTRY), …]`, `sibling_nav_control: Some(_)` | `[searchbox, panel]`, seal `Sealed` | p1 `<C-l>`: `searchbox` has only `<CR>`/`<Esc>` → `NoMatch`; key bears CTRL so the seal does **not** stop the walk → `panel <C-l>` → `focus.right` | `FocusChanged` | **Yes** — Ctrl+hjkl escapes a filter box unconditionally |
| 9 | `x`, same filter `LineEdit` | same | same | p1 `x`: `searchbox` `NoMatch`; key is **bare** → seal stops the walk at the anchor | — | **No** — the `LineEdit` types `x`. One rule replaces the `is_prompt_active` special case at `input.rs:156-159` and the search-box passthrough |
| 10 | `<C-h>`, Project Settings `LineEdit` (no sibling nav control) | `[LineEdit, …]`, `sibling_nav_control: None` | `foreign`, seal `Barrier` | — S3 returns before S4 | — | **No** — "never intercept in Foreign" (`input.rs:90`) is now structural |

Two rows worth reading together: **7** is the entire arbitration seam in one line, and **6** is the reason `Anchor` is a two-variant enum instead of a `usize`.

---

### 5.13 What this stage list deliberately does not decide

- **Host-originated invocation skips the model entirely.** `HostRequest::RunAction`, `:action <id>` and `<Action>(id)` arrive with no keystroke, no surface and no probe. They resolve the action **by name** through `ActionRegistry::id_of`, hop to `&mut GodotVimCore` through `PendingUiAction::RunRegistryAction { name, count }`, and run the `ActionSpec` directly. Consequences that must be stated rather than glossed: `Caps` is a **binding-plane gate only** — `registry.run` never consults `requires`, which is what makes `:action godotvim.fs.refresh` work from the cmdline. Actions that are meaningless without panel focus carry `host_invocable: false` and report a real failure ("requires panel focus") instead of declining invisibly. And the hop costs zero frames but is ordered *after* `apply_ui_update` and `set_input_as_handled` in the same call stack, so a registry action must never publish user-visible text through `handle_show_message` — it would surface one keystroke late. Panel-key invocation runs inline with `&mut self`; that asymmetry is real and `:panelmap` prints it.
- **`vim_claims` is not consulted anywhere except the anchor-yields path.** A rule on a non-`editor.*`-reachable surface never asks the engine anything. That is deliberate: the engine has no opinion about a key pressed while a Tree has focus.
- **`editor.nav` has no per-rule arbitration override.** With `yields_to_engine` on the surface, there is no way to express "a key the engine claims but the shell must win anyway". No such case exists today and a per-rule override is precisely what was rejected, but it is a genuine expressiveness ceiling and `:checkhealth` should say so rather than let it be discovered.
- **`starts_vim_grammar_sequence` probes a *default* `Keymap`.** A `<Leader>` remapped onto a Ctrl key, or a future vim-core option that alters core grammar arity, could in principle create a grammar prefix the guard does not see. The dispatch-time `could_start_mapping` gate catches every *user*-created prefix, so the residual is bounded to future engine options — and it is pinned by a vim-core-version test asserting the six expected verdicts for `<C-w>`, `<C-\>`, `<C-h>`, `<C-j>`, `<C-k>`, `<C-l>`. Those verdicts were traced by reading v0.7.1 source, not executed; the very first implementation task is that test, and if any verdict disagrees the guard's *shape* holds but its exclusion set needs re-derivation.
- **The `DeadPrefix` divergence from Vim is a choice, not a discovery.** Vim would flush both keys as literals. Given that `set_allow_search(false)` is already applied to exactly the controls that reserve a prefix, flushing might now be harmless and strictly more Vim-faithful. It is left as an open question (§13) rather than silently settled.

---

## 6. Config Syntax

### 6.1 Where bindings live: one file, two layers

There is exactly **one** active config file, ever. `config::path::resolve` (`src/config/path.rs:23-61`) is three mutually exclusive early returns:

1. the EditorSettings string `plugins/GodotVim/mapping/config_file_path` if non-empty (`src/settings/keys.rs:56`, registered as a free string at `src/settings/registration.rs:102`) — marked `is_project_level: false`, i.e. trusted, because the user chose the path;
2. otherwise `res://.godot-vimrc` if `FileAccess::file_exists` says so (`path.rs:38`) — `is_project_level: true`;
3. otherwise `user://.godot-vimrc` (`path.rs:51-54`) — `is_project_level: false`.

They are never layered, never merged. Two consequences the reader must hold onto:

- **A project that ships a `res://.godot-vimrc` silently and completely shadows the user's `user://.godot-vimrc`.** The only evidence today is a `log::debug!` line (`path.rs:43-47`), and the default log level is `Off` (`src/settings/defaults.rs:14`). This is pre-existing behaviour and this design does not change it — layering would force per-line provenance through `ConfigDocument`, `apply_vimrc_policy` (which is keyed on the single `is_project_level` bool, `sandbox.rs:369-388`), every diagnostic, and the whole-file-backed MappingDialog (`src/ui/mapping_dialog.rs:598-620` reads exactly one path through `writer::read_file`). Instead the shadowing becomes *discoverable*: `:checkhealth godotvim` prints `user://.godot-vimrc exists but is shadowed by res://.godot-vimrc` whenever the resolved path is `res://` and the user file also exists. That is a two-line `FileAccess::file_exists` check, not a semantic change.
- **`is_project_level` is the only trust input.** Combined with the `plugins/GodotVim/security/project_vimrc` enum (`Disabled | Sandbox | Trusted`, `src/settings/keys.rs:78`), it decides whether `sandbox_config_text` runs at all.

The shell plane's precedence is therefore **two layers, mirroring what `VimController::reload_config` (`src/controller/mod.rs:701-724`) already does for the engine plane** — clear, apply builtin text, apply user text:

```rust
// src/actions/plane.rs
impl ActionPlane {
    /// `vimrc` is `None` when the file is absent OR `ProjectVimrc::Disabled`
    /// blocked it. Builtin defaults load in both cases — that is the
    /// zero-config guarantee, and it must not depend on a security setting.
    pub(crate) fn rebuild(&mut self, vimrc: Option<&str>) {
        let mut index = BindingIndex::default();                 // layer 0
        self.diagnostics.clear();
        for register in crate::actions::providers::PROVIDERS {   // layer 1
            let mut r = Registrar::new(
                &mut index, &mut self.registry, &mut self.forest,
                &mut self.diagnostics, Provenance::Builtin,      //   Host(tag)
            );
            register(&mut r);
        }
        if let Some(text) = vimrc {                              // layer 2
            for (lineno, line) in text.lines().enumerate() {
                let Some(parsed) = crate::config::panelmap::parse_panel_line(line.trim())
                else { continue };
                match self.resolve_rule(&parsed) {
                    Ok(rule)  => index.upsert(rule),             // last writer wins
                    Err(diag) => self.diagnostics.push(diag.at_line(lineno + 1)),
                }
            }
        }
        // The atomic swap: an in-flight dispatch holding the old Rc keeps the
        // old index alive and finishes against it (§4.5).
        self.index = std::rc::Rc::new(index);
        self.generation += 1;
    }
}
```

`rebuild` is called **unconditionally** from a restructured `source_config_from_disk` (`src/plugin/mod.rs:1174-1195`) — before the `apply_vimrc_policy` result is unwrapped and before the file-existence early return at `:1176-1178`. Placing it inside `if let Some(text)` as the original draft did would mean setting `project_vimrc = Disabled`, or simply having no vimrc, destroys the builtin Ctrl+hjkl defaults. The second controller guard at `src/plugin/mod.rs:808-810` (`on_config_saved`) is deleted so shell-plane hot-reload does not depend on engine liveness; the `!self.enabled` guard at `:805-807` stays.

Within layer 2, precedence is **file line order, last writer wins per `(surface, canonical_lhs)`**. This is forced, not chosen: `MappingTrie::insert` does `node.entry = Some(entry)` and `TrieLookup::ExactOnly` yields exactly one entry, so one LHS on one surface can only ever surface one rule. Plurality in the candidate list comes from the *forest walk* (one candidate per surface on the path), never from one LHS.

### 6.2 Grammar

Two verbs, single-line, no cross-line state — required, because `sandbox.rs` sanitizes raw text line by line before any structured parse.

```
panelmap   [<flag> ...] <surface> <lhs> <target> [key=value ...]
panelunmap <surface> <lhs>
```

- **`<flag>`** — a subset of `<nowait> <physical> <void> <norepeat> <shift>`, each at most once, in any order, occupying exactly one lexical slot between the verb and the surface. `<nowait>` sets `Rule::nowait`, which builds the trie entry with `MappingEntry::new_nowait` so `MappingTrie::lookup` promotes `Prefix` to `ExactOnly` internally (`trie.rs:443-446`) and a shorter LHS fires immediately instead of waiting out a longer one. `<physical>` opts the rule into the US-QWERTY position probe (probe 3). `<void>` sets `Consumption::Void` (consume regardless of the action's outcome). `<norepeat>` sets `Repeat::Suppress` (drop `is_echo` events). `<shift>` sets `Rule::shift_tolerant`, which at registration inserts a second trie LHS with SHIFT set pointing at the same `SlotId`. There is **no `yield` token** — arbitration is a property of `SurfaceSpec` (`yields_to_engine`, true on `editor.nav` only) and is unreachable from config by construction.
- **`<surface>`** — exactly one declared surface id, matching `^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$`. There are no prefix selectors, no groups, and no implicit default scope: forest inheritance already does that job, and depth in the declared forest is the *only* specificity mechanism.
- **`<lhs>`** — Vim notation, parsed by `vim_core::execution::parse_keys_from_string` (re-exported at `execution/mod.rs:112`), then `Key::Leader` substitution, then `canonicalize`, capped at `MAX_KEY_SEQUENCE_LEN = 8` (`vim-core/src/keymap/keymap.rs:140`).
- **`<target>`** — one of: a dotted registered action id (`^[A-Za-z][A-Za-z0-9_]*(\.[A-Za-z0-9_]+)+$`, must contain a dot so it can never collide with a bare keyword); the literal `native`; or `<Shortcut>(section/path)`. These are the three variants of `RuleTarget` — `Action(ActionId) | Native | Shortcut(CompactString)`. `native` and `<Shortcut>` are *not* actions and build no `ActionCtx`.
- **`key=value`** — at most four, keys unique and matching `^[A-Za-z0-9_.]+$`, values **decimal integers only**, `^-?[0-9]{1,10}$`. There is no enum-token form and the grammar must not promise one; a bool is `flag=1`. `count` is validated at load into `1..=100` and clamped again at runtime by `Params::count()`, because `find_navigable_target` walks up to `MAX_ATTEMPTS = 1000` per call (`src/navigation/dock_nav.rs:37`) and an unbounded count is an editor freeze.

The five-flag vocabulary is a deliberate decision with a single justification: **provider defaults are authored in exactly the text a user types, parsed by exactly the same parser** (`Registrar::defaults`). If `<void>`/`<norepeat>`/`<shift>` were provider-only tokens, the shipped defaults would be written in a dialect the documented grammar does not describe, the anti-drift test between parser and sandbox whitelist could not be stated as one property, and a user rebinding Ctrl+hjkl could not reproduce the shipped semantics. None of the three widens the target vocabulary — they alter matching tolerance and consumption of an already-permitted verb — so all five are accepted at every trust tier.

### 6.3 Where each directive is legal

| Constraint | Rule | Enforced at |
|---|---|---|
| Barrier surfaces take no rules | `panelmap foreign …` / `panelmap editor.insert …` is a registration error. Dispatch returns `Ignore` before any lookup on those surfaces, so a rule there is unreachable by construction — and accepting one would let a project vimrc claim keys inside a Project Settings LineEdit or in Insert mode. | registration, warn-and-skip |
| Editor-reachable surfaces are single-key only | A surface is editor-reachable when it is an `editor.*` surface **or an ancestor of one in the declared forest**. `panel` is `editor.nav`'s parent, so `panelmap panel <C-w>h …` is rejected — "reject on `editor.*`" alone would miss it. | registration |
| No vim grammar prefixes on editor-reachable surfaces | `keys::starts_vim_grammar_sequence(k)` asks vim-core's own `grammar::Parser` whether `k` puts the grammar into an `Awaiting*` state, across the three nav modes and both sneak settings. True for `<C-w>` and `<C-\>` and for bare digits; false for `<C-h>/<C-j>/<C-k>/<C-l>`. This closes the hole generically instead of via a denylist that rots. | registration |
| `<Shortcut>(path)` is untrusted-denied | Stripped by `sandbox_config_text` at project level under `Sandbox`, and re-rejected by the loader as defence in depth. | sandbox + loader |
| `<Shortcut>(path)` needs Godot ≥ 4.6 | Warn-and-skip at load when `has_shortcut_api()` is false; `:checkhealth` prints an explicit "unavailable on Godot < 4.6" line. See §12.3. | loader |
| `<Shortcut>` / delegating actions may not form an injection cycle | A rule whose first LHS key appears in the delegated shortcut's own event array is a self-collision and is rejected. `<F2>` → `<Shortcut>(filesystem_dock/rename)` is the length-1 case and would hard-hang the editor. | index build |
| `native` is legal everywhere | Every surface, every trust tier. It can only *reduce* what the plugin consumes. | — |
| Unknown action id | Rejected at load; nothing is inserted. A typo can never become a silent key black hole. | loader |

### 6.4 Worked examples

```vim
" ── 1. Cross-panel focus, in the exact form the plugin ships it.
"    `panel` is the forest root, so this is live on every non-Barrier surface:
"    docks, filter boxes, the FS prompt, the attached editor in nav modes, and
"    with no focus owner at all. <void> reproduces src/plugin/input.rs:126-134,
"    where handle_window_nav's result is discarded and set_input_as_handled()
"    fires even when nothing was found.
panelmap <physical> <void> <norepeat> panel <C-h> godotvim.focus.left

" ── 2. Rebind cross-panel focus to Alt. Unbind first: `panelunmap` and
"    `panelmap` are separate verbs, and rebinding needs both.
panelunmap panel <C-h>
panelmap <physical> <void> <norepeat> panel <M-h> godotvim.focus.left

" ── 3. Rebind dock item nav. `godotvim.item.next` requires Caps::VNAV, which
"    Tree, ItemList AND RichTextLabel all contribute — so this keeps working on
"    the Output panel and the docs panel, which scroll 50px per press.
panelunmap dock j
panelunmap dock k
panelmap <physical> dock <C-n> godotvim.item.next
panelmap <physical> dock <C-p> godotvim.item.prev

" ── 4a. `<C-w>h` on `panel` — REJECTED, twice over, warn-and-skip + checkhealth.
"    (a) `panel` is `editor.nav`'s declared parent, so it is editor-reachable
"        and takes single-key LHS only;
"    (b) `<C-w>` starts vim-core's window-command grammar, so consuming it at
"        _input() would turn `<C-w>s` into a bare `s` — a destructive edit.
panelmap panel <C-w>h godotvim.focus.left

" ── 4b. `<C-w>h` on `dock` — LEGAL. `dock` is not an ancestor of any editor.*
"    surface, so multi-key is allowed and `<C-w>` is not a grammar prefix there.
"    This reserves bare `<C-w>` on `dock`; :panelmap prints the reservation.
panelmap dock <C-w>h godotvim.focus.left
panelmap dock <C-w>l godotvim.focus.right

" ── 4c. …and from the EDITOR, `<C-w>h` already works and needs no panelmap line
"    at all: vim-core's grammar resolves it to WindowNavAction::Left, which
"    reaches the same `godotvim.focus.left` ActionSpec. If you want a different
"    editor key, that is `:nnoremap`, not `panelmap`:
nnoremap <C-w><C-h> <Action>(godotvim.focus.left)

" ── 5. A FileSystem action, nvim-tree taste: move create off `a` onto `n`.
"    `dock.filesystem` is deeper in the forest than `dock`, so it gets first
"    refusal; j/k still fall through to `dock`. This is the hardcoded
"    src/plugin/input.rs:140-150 branch expressed as depth.
panelunmap dock.filesystem a
panelmap <physical> dock.filesystem n godotvim.fs.create

" ── 6. A multi-key sequence. Binding `dd` implicitly RESERVES bare `d` on
"    dock.filesystem; the reservation is printed by :panelmap and :checkhealth,
"    and Tree::set_allow_search(false) is applied to that control only.
panelmap dock.filesystem dd godotvim.fs.delete

" ── 7. Unbinding. Removes the rule; the forest walk then CONTINUES to the
"    parent surface. This does NOT hand the key back to Godot.
panelunmap dock.filesystem y

" ── 8a. Give-back, globally. Terminates the walk at `panel`: nothing is
"    consumed, Ctrl+H reaches Godot's own handling everywhere.
panelmap panel <C-h> native

" ── 8b. Give-back, scoped. Only in the FileSystem dock; `panel`'s rule still
"    fires everywhere else. Impossible today — Ctrl+H is consumed
"    unconditionally in dock/searchbox/unknown contexts (input.rs:120-135).
panelmap dock.filesystem <C-h> native

" ── 8c. Give-back, editor only. In the attached CodeEdit the key flows to
"    gui_input and the vim engine; from a dock it still moves panels.
panelmap editor.nav <C-h> native

" ── 9. Scoped vs. shallow, same LHS, two depths. In the FileSystem dock `r`
"    renames; in every other dock it moves to the previous item.
panelmap dock r godotvim.item.prev
panelmap <physical> dock.filesystem r godotvim.fs.rename

" ── 10. A capability-gated action on a widget that lacks the capability.
"    `godotvim.item.expand` requires Caps::HIERARCHY, which only a Tree grants.
"    On an ItemList (Script list) or a RichTextLabel (Output, docs) the
"    candidate is SKIPPED as if NoMatch, the walk continues to `panel`, nothing
"    matches, and the key reaches Godot. No DockKind branch anywhere.
panelmap <physical> dock l godotvim.item.expand

" ── 11. Parameterized binding. Values are decimal integers only; count is
"    validated into 1..=100 at load and clamped again at runtime.
panelmap dock <C-d> godotvim.item.next count=10
panelmap dock <C-u> godotvim.item.prev count=10

" ── 12. Leader works in panels exactly as in a buffer. `let mapleader` is
"    already sandbox-safe today (sandbox.rs:172-188).
let mapleader = " "
panelmap dock.filesystem <leader>ff godotvim.fs.create

" ── 13. Sealed surfaces. `searchbox` and `prompt` are separate surfaces, both
"    Sealed: an unbound BARE key stops there and reaches the LineEdit (typing
"    works, text_submitted fires), while a modifier-bearing key continues to
"    `panel` so Ctrl+hjkl still escapes both. <shift> reproduces
"    handle_search_input's guard at dock.rs:166, which rejects ctrl/alt/meta
"    but tolerates shift.
panelmap <shift> searchbox <CR>  godotvim.search.accept
panelmap <shift> searchbox <Esc> godotvim.search.cancel
panelmap         prompt    <Esc> godotvim.prompt.dismiss

" ── 14. Delegate to one of Godot's own registered editor shortcuts: the plugin
"    owns the KEY, Godot owns the BEHAVIOUR. Deliberately NOT <F2> — F2 is that
"    shortcut's own accelerator, and binding it would re-inject the event into
"    the same Input flush and hang the editor. ILLEGAL in a sandboxed project
"    vimrc; SKIPPED on Godot < 4.6.
panelmap dock.filesystem <C-r> <Shortcut>(filesystem_dock/rename)

" ── 15. Cycle focus ships no default key (nothing is free on `panel`, and a
"    bare letter would steal Tree type-to-search). The recipe lives commented
"    in .godot-vimrc.sample; run :checkhealth godotvim to confirm no collision.
panelmap dock <M-]> godotvim.focus.cycle_next
panelmap dock <M-[> godotvim.focus.cycle_prev

" ── 16. A disabled rule, written by the MappingDialog's toggle. Round-trips as
"    ConfigLine::PanelMap { enabled: false }, not as a Comment.
" disabled: panelmap dock j godotvim.item.next

" ── 17. The reverse direction needs no new syntax and no panelmap line: one
"    namespace, four front-ends.
nnoremap <leader>ff <Action>(godotvim.fs.create)
"    …and `:action godotvim.fs.refresh` works on the cmdline for free.
```

Lines that are **rejected**, and what the user sees:

```vim
panelmap foreign <Esc> godotvim.focus.editor   " Barrier surface takes no rules
panelmap editor.insert <C-h> godotvim.focus.left  " ditto
panelmap dock j :!rm -rf /                     " target is not an action id or `native`
panelmap dock <S-1> godotvim.item.next         " unresolvable shift-fold, see §6.8
panelmap dock <Bogus> godotvim.item.next       " unknown key notation, see §6.8
panelmap dock <F2> <Shortcut>(filesystem_dock/rename)  " injection self-collision
set panelsafety=off                            " no shell-plane knob is a `set` option
```

### 6.5 Sandbox

`sandbox_config_text` (`src/config/sandbox.rs:60-105`) is a whitelist with two branches and a terminal `else`. **`panelmap` falls into the terminal `else` today** and is rewritten to `" [sandbox] stripped: …` with only a `log::warn!`, because:

- `is_safe_non_mapping_line` (`:147-190`) accepts only empty lines, `"`-prefixed lines, `set `/`se ` with no blocked option, and `let mapleader`;
- `is_mapping_line` (`:195-206`) delegates to `is_map_or_noremap_abbrev` (`:262-322`), whose `matches_abbrev` (`:229-231`) requires `name.len() <= full.len()`. Every `full` in that table is at most 8 characters and the only 8-character entries are the `*noremap` forms; `panelmap` is 8 characters and equals none of them, and `panelunmap` is 10. The two namespaces are **provably disjoint**, which is why `panelmap` deliberately has no abbreviation.

A **third branch** is inserted at `:93`, between the `is_mapping_line` block and the terminal `else`, leaving both existing branches byte-for-byte untouched:

```rust
        } else if is_panel_line(trimmed) {
            if panel_line_is_safe(trimmed) {
                output.push_str(line);
                output.push('\n');
            } else {
                log::warn!("sandbox: stripped panel binding from project vimrc: {}", trimmed);
                output.push_str("\" [sandbox] stripped: ");
                output.push_str(trimmed);
                output.push('\n');
            }
        } else {
```

`is_panel_line` takes the first whitespace-delimited token, applies `trim_end_matches('!')` — mirroring `sandbox.rs:203` and vim-core's own `split_head` at `grammar/ex_parser.rs:273`, so `panelmap!` cannot slip through — and compares case-insensitively against exactly `panelmap` and `panelunmap`. `panel_line_is_safe` is a closed, deny-by-default token whitelist over the grammar of §6.2: flag set, surface regex, LHS printable-ASCII with no space, `:`, `|` or `"`, target ∈ {dotted action id, literal `native`}, params `k=v` with integer values, at most four.

**Why the closed vocabulary makes this safe, precisely.** The reason `sandbox.rs:67-81` strips *all* recursive maps unconditionally is that a Vim RHS is a **key sequence** that re-enters the mapping engine and can compose innocuous fragments into `:!` or `:source`. A `panelmap` RHS is not a key sequence and re-enters no expander: it is one token that must resolve at load time to an entry in `ActionRegistry`, whose contents are fixed at compile time by `const ActionSpec` values in `src/actions/providers/`. There is no `<expr>`, no NOREMAP propagation, no recursion depth, and `Params` values are integers with no string channel. The entire attack surface of a sandboxed panel line is therefore *which of the ~21 shipped verbs runs, with which small integer*.

The one escape from that closed set is `<Shortcut>(path)`, because it reaches `HostRequest::RunAction`'s shortcut branch (`src/host/dispatch.rs:444-464`) → `godot_calls::get_shortcut`, which resolves **any** registered editor shortcut path — `editor/run_project` included. That is why exactly that target is denied, and only that one.

**Why `native` is legal at every trust tier.** `sandbox_config_text` is reached only for project-level files under `ProjectVimrc::Sandbox` (`apply_vimrc_policy`, `sandbox.rs:369-388`: non-project → unchanged, `Disabled` → `None`, `Trusted` → unchanged). "Permitted at every tier" is therefore achieved simply by accepting it in the untrusted branch. Semantically it is monotone in the safe direction — it can only *reduce* what the plugin consumes, at worst restoring the behaviour a user would have without the plugin installed. Denying it would block the one verb whose purpose is to make the plugin get out of the way.

**No security toggle may be expressible as a `set` line.** `is_safe_non_mapping_line`'s `set` arm computes `has_blocked` over `BLOCKED_SET_OPTIONS` (`:109-134`) and returns `!has_blocked` — so *any* option name not on that 24-entry list passes verbatim into `reload_config`. Four binding rules:

- **R1 (the real guarantee).** The shell plane reads **no** option from `ConfigLine::Setting` or from `VimOptions`. Its only configuration inputs are EditorSettings keys under `plugins/GodotVim/`, which a `res://` file structurally cannot write. Reviewer check: `src/actions/` contains no reference to `ConfigLine::Setting` or `VimOptions`.
- **R2.** Every security or dispatch-disabling knob lives under `plugins/GodotVim/security/`, following the existing `shell_execution` / `file_access_scope` / `project_vimrc` precedent (`src/settings/keys.rs:76-78`).
- **R3 (defence in depth).** Reserved shell-plane option names are added to `BLOCKED_SET_OPTIONS` anyway, in long and short form: `panelsafety`, `pnsf`.
- **R4.** A test enumerates `actions::RESERVED_SET_OPTION_NAMES` and asserts each is present in `BLOCKED_SET_OPTIONS`.

R3 carries a naming constraint that is invisible from the design and would otherwise produce a whitelist entry that silently never fires: `extract_option_name_from_token` (`sandbox.rs:138-144`) splits the token on `['=','?','!','+','-',':']` and then strips a leading `no`. A reserved name must therefore match `^[a-z]+$` and must **not** begin with `no` — `nodispatch` would normalize to `dispatch` and never match. `panelsafety`/`pnsf` satisfy both.

Two further requirements on the sandbox itself. First, **deliberate duplication, not shared code**: `panel_line_is_safe` must not call `parse_panel_line`, because the sandbox's whole discipline is to sanitize raw text *before* any structured parse — this mirrors the existing, explicitly documented duplication of the engine's abbreviation logic at `sandbox.rs:260-261`. Pay for it with an anti-drift **proptest over generated panel lines** (not a fixed table): every line `parse_panel_line` accepts with an `Action` or `Native` target must pass `panel_line_is_safe`, and every line it rejects must fail it. Second, the **loader re-validates**, re-rejecting `RuleTarget::Shortcut` when `is_project_level && policy == Sandbox`, so a future refactor that stops calling the sandbox cannot open the hole.

### 6.6 Storage and round-trip

`ConfigLine::PanelMap(Box<PanelPayload>)` joins the six existing variants at `src/config/types.rs:161-170`. `src/config` gains **no** dependency on `src/actions`: `parse_panel_line` produces a `ParsedPanelMap` of raw string tokens (`unmap`, five flag bools, `surface`, `lhs`, `target: Option<String>`, `params: Vec<(String, String)>`), and `src/actions/panelmap.rs` resolves those tokens to `RuleTarget` / `ActionId` / `SurfaceId` with warn-and-skip. That layering is what keeps the parser pure and Godot-free.

**The parser has TWO branches and both must be edited.**

*Edit 1* — inside the existing `" disabled:` sub-branch at `src/config/parser.rs:40-51`, which today calls only `try_parse_mapping_command` and therefore degrades `" disabled: panelmap dock j godotvim.item.next` to `ConfigLine::Comment`. The panel parser is tried **after** the mapping parser fails, so today's behaviour is preserved byte-for-byte:

```rust
            if let Some(cmd_str) = trimmed.strip_prefix("\" disabled:") {
                let cmd_str = cmd_str.trim_start();
                if let Some(parsed) = try_parse_mapping_command(cmd_str) { /* … unchanged … */ }
                if let Some(parsed) = super::panelmap::parse_panel_line(cmd_str) {
                    pending_preset = None;
                    lines.push(ConfigLine::PanelMap(Box::new(PanelPayload {
                        enabled: false, parsed,
                    })));
                    continue;
                }
            }
```

*Edit 2* — the enabled arm, placed **after** `pending_preset = None;` at `:87` and **before** the `set `/`se ` arm at `:89`. That placement is exact and load-bearing: a panel line is not a mapping, so it must discard a stale preset marker exactly as `set`/`let`/`Other` do. `try_parse_mapping_command` at `:72` provably cannot shadow it — `parse_map_command_prefix`'s COMMANDS table (`:148-171`) contains no prefix that `panelmap ` starts with.

The disabled-**preset** branch at `:54-66` is left alone: `PRESETS` (`src/config/presets.rs:39-212`) contains no panel entries and panel rules are never preset-managed. A guard test pins that `" preset:disabled` followed by `" panelmap …` still becomes a `Comment`.

The writer gains one arm after the Mapping arm at `writer.rs:57`, reusing the self-contained `" disabled: ` convention (`writer.rs:47-49`) rather than the preset-marker machinery. `serialize`'s match is wildcard-free (`writer.rs:17-66`) and `ConfigLine` is not `#[non_exhaustive]`, so **the compiler forces the arm to exist** — that is the safety net.

**The round-trip property is DOCUMENT-level identity, not text-level fixpoint.** The text-level property `parse(serialize(parse(x))) == parse(x)` is vacuous here and would pass with the disabled-branch bug present: `ConfigLine::Comment` stores `raw_line` (`parser.rs:68`), `ConfigLine::Other` stores `raw_line` (`parser.rs:99`), and the writer re-emits both verbatim (`writer.rs:22-25`, `:58-61`) — so a panel line that has *lost its identity* still round-trips as text. Three tests, in order of strength:

1. `disabled_panelmap_roundtrip`, structurally copied from the existing `disabled_user_mapping_roundtrip` (`parser.rs:372-404`): build a `ConfigDocument` holding one `PanelMap { enabled: false }`, assert `serialize` yields exactly `"\" disabled: panelmap dock j godotvim.item.next\n"`, reparse, assert it is still a `PanelMap` with `enabled == false`. **This is the test that catches the missing disabled branch.**
2. `parse_config(serialize(&doc)) == doc` as a proptest generated from typed `ConfigDocument`s. Needs `PartialEq, Eq` derived on `ConfigLine` and `MappingPayload` (`ParsedMapping` already has them at `types.rs:141`) — a two-word diff. `proptest = "1.4"` is already a dev-dependency (`Cargo.toml:26`).
3. The text-level fixpoint retained as a weaker second property over documents interleaving panel lines with mappings, presets and settings, because it catches writer/parser drift on the *other* variants.

**Defaults are never written to disk.** `generate_default_config` (`writer.rs:104-142`) takes only `&[PresetDefinition]` and emits no `panelmap` lines; it needs no change. An existing `.godot-vimrc` is byte-identical after upgrade. The MappingDialog's new "Panel Keys" tab renders provider defaults as read-only rows with an **Override** button that writes exactly one user line — which fixes both a file-backed dialog showing an empty tab on a fresh install, and nvim-tree's documented `view.mappings.list` failure where a frozen default table means upgrades can never fix an untouched default.

Finally, `panelmap` is *simultaneously* a config directive and a cmdline ex-command, and that collision is resolved as a feature with `:map` semantics: `:panelmap` lists all rules, `:panelmap <lhs>` explains resolution for that key, and `:panelmap <surface> <lhs> <target> [k=v]` installs a session rule through the **same** `parse_panel_line` the vimrc uses. One parser, one meaning, pinned by a test.

### 6.7 `panelunmap` versus `native`

The draft carried a contradiction — worked example 2 documented `panelunmap panel <C-h>` as the rebinding recipe while a test-strategy invariant made `Consumption::Void` builtins un-unbindable, and all four Ctrl+hjkl defaults are `Void`. **That invariant is deleted.** It was paternalistic, it blocked the primary workflow, and it was never a panel constraint. It is replaced by a pure query over the rule arena after the full load: `:checkhealth godotvim` warns when no rule on any non-Barrier surface targets `godotvim.focus.{left,right,up,down}`. The user is told they have no cross-panel navigation; they are not prevented from choosing it.

Underneath the contradiction was a real semantic gap, and closing it is what makes the "I want Ctrl+h back for Godot" story work at all:

| Verb | Effect on the forest walk | Consumed? |
|---|---|---|
| `panelunmap <surface> <lhs>` | Removes the rule. The walk **continues** to the parent surface. | Whatever the parent decides |
| `panelmap <surface> <lhs> native` | Installs `RuleTarget::Native`. `resolve()` returns `Resolution::None` immediately — the walk **stops**. | Never |

```rust
// src/actions/resolve.rs — the line that makes `native` different from unbinding.
match &rule.target {
    RuleTarget::Native      => return Resolution::None,  // -> Disposition::Ignore
    RuleTarget::Action(id)  => out.push(registry.spec(*id),
                                        rule.params.clone(), rule.consume),
    RuleTarget::Shortcut(p) => out.push_shortcut(p.clone(), rule.consume),
}
```

Three recipes, all working, all documentable in one table:

1. **Move panel-left to Alt+h.** `panelunmap panel <C-h>` then `panelmap <void> <norepeat> panel <M-h> godotvim.focus.left`. `:checkhealth` stays silent because `focus.left` is still bound.
2. **Give Ctrl+h back to Godot everywhere.** `panelmap panel <C-h> native`. `:checkhealth` warns that `focus.left` is unbound — correct, informative, non-blocking.
3. **Give Ctrl+h back only in the FileSystem dock.** `panelmap dock.filesystem <C-h> native`. The deepest surface wins and `panel`'s rule is never reached.

The trap this documents: **`panelunmap dock <C-h>` does not give Ctrl+H back**, because the walk falls through to `panel`, whose rule is still live. A user who tries the obvious thing gets no effect and no diagnostic unless the distinction is spelled out. Note also that all three recipes are *strictly more capable than today*, where Ctrl+H is consumed unconditionally in dock/searchbox/unknown contexts — `set_input_as_handled()` fires at `src/plugin/input.rs:132` regardless of the nav result — and there is no way to give it back from a dock at all.

### 6.8 Errors: what the user actually sees

Diagnostics are **pushed** on every index build (startup, config reload, provider registration/unregistration) through `godot_warn!` / `godot_print!` **directly, never through the `log` facade** — `defaults::LOG_LEVEL` is `"Off"` (`src/settings/defaults.rs:14`), so a `log::warn!` diagnostic is invisible on a stock install. Godot's Output panel is itself a dock, so it is reachable with no script open. They are also **pullable** via `:checkhealth godotvim` and `:panelmap <lhs>` — intercepted in `GodotHost::handle_request_inner` beside `vimdebug`, relayed as `PendingUiAction::PanelCommand(CompactString)` through the forward-to-plugin arm at `src/controller/process.rs:240-244`, and rendered by the plugin into Godot's Output panel, which is where a multi-line report has to go — and, for a user with no script open and no working keybinding, via a one-shot bool EditorSettings key `plugins/GodotVim/diagnostics/print_report`. The status bar is *not* a channel: it is injected as a child of the attached `CodeEdit` (`src/ui/status_bar.rs:414-426`) and is structurally unreachable from a dock.

| Failure | Detection | User-visible result |
|---|---|---|
| **Unknown action id** — `panelmap dock j godotvim.item.nxt` | Load-time lookup in `ActionRegistry`, which is closed at compile time | Warn-and-skip that one line. **Nothing is inserted**, so `j` keeps its previous meaning rather than becoming a permanent silent black hole. `:checkhealth` prints `line 12: unknown action id 'godotvim.item.nxt'`. |
| **Unknown surface id** | Load-time lookup against the declared forest | Same shape. Adjacent: a rule whose `requires` no surface on its own forest path can ever grant is a *registration* error, not a mystery — `every_rule_is_referentially_intact` fails the build. |
| **Bad key notation** — `panelmap dock <Bogus> …` | `parse_lhs` pre-scans for `<…>` tokens and requires `KeyEvent::from_vim_notation` to accept each one | Rejected with `LhsError::UnknownNotation("<Bogus>")`. This check is **mandatory, not cosmetic**: `parse_keys_from_string` does not fail on an unknown notation — `parse_macro_entries` falls through to character-by-character parsing (`vim-core/src/execution/engine/macro_replay.rs:261-270`), so `<Bogus>` would silently become a **seven-key** sequence `< B o g u s >` and would reserve bare `<` on that surface. |
| **Unresolvable shift fold** — `panelmap dock <S-1> …` | `canonicalize` rejects SHIFT on a non-alphabetic `Char` | Rejected at load. `<S-1>` parses to `Char('1') + SHIFT` while the runtime event is `Char('!') + NONE` on US and `Char('+') + NONE` on DE; folding is impossible without a layout table (`physical_to_ascii` is hardcoded US-QWERTY, `src/bridge/input.rs:97-99`). Vim requires the user to spell the produced character too. Message: `write the character the key produces, e.g. `!``. |
| **LHS too long / empty** | `parse_lhs` caps at `MAX_KEY_SEQUENCE_LEN = 8` | Warn-and-skip. The shell caps at the same value as vim-core deliberately, so the two planes cannot disagree. |
| **Out-of-range param** — `count=0`, `count=9223372036854775807` | Loader validates `count ∈ 1..=100`; `Params::count()` clamps again | Warn-and-skip at load; clamp at runtime as defence in depth. |
| **Illegal placement** — Barrier surface, multi-key on an editor-reachable surface, grammar prefix, `<Shortcut>` at untrusted tier, `<Shortcut>` on Godot < 4.6, injection cycle | `BindingIndex::try_insert` → `RuleReject` | Warn-and-skip per line with the specific reason, plus a `:checkhealth` entry. |
| **Sandbox-stripped line** | `sandbox_config_text` | The line is replaced with `" [sandbox] stripped: …` **in memory only** — the file on disk is untouched. The MappingDialog's Panel Keys tab calls `apply_vimrc_policy` and badges such lines red rather than displaying them as live. |
| **Malformed builtin default** | `Provenance::Builtin` | **Never warn-and-skip.** Warn-and-skip is correct for user text and wrong for shipped text. A builtin that fails to parse is `log::error!` plus `debug_assert!`, so a debug build and CI fail loudly; a release build degrades rather than panicking inside a `cdylib` the editor dlopens. The P5 golden test (`all_provider_defaults_load_with_zero_diagnostics`) asserts `diagnostics.is_empty()`, an exact field-by-field table against §12.1, and `rules().count() == SHIPPED_DEFAULTS.len()` — it is the only thing standing between a rename and a silently dropped shipped binding, and it must run in CI. |

---

## 7. Extensibility

### 7.1 The contract

A new subsystem adds **one file** under `src/actions/providers/` and **one line** in `PROVIDERS`. `src/actions/resolve.rs`, `src/actions/dispatch.rs`, `src/plugin/input.rs`, `src/actions/caps.rs` and every existing provider are untouched. That is the whole claim, and it is deliberately "zero *dispatcher* edits", not "zero edits".

Registration is a `const` array, not link-time magic:

```rust
// src/actions/providers/mod.rs
pub(crate) const PROVIDERS: &[fn(&mut Registrar<'_>)] = &[
    editor::register,     // editor.insert, editor.nav   (Barrier / yields)
    prompt::register,     // prompt                      (Sealed)
    searchbox::register,  // searchbox                   (Sealed)
    filesystem::register, // dock.filesystem
    dock::register,       // dock
    foreign::register,    // foreign  — Barrier, but only AFTER every surface
                          //            that needs first refusal, and BEFORE
                          //            `unknown` or it is unreachable (§3.3)
    unknown::register,    // unknown  — TOTAL probe, must stay last-but-one
    panel::register,      // panel    — root, never probes
];
```

`inventory`/`linkme` are rejected outright. Life-before-main constructors in a `cdylib` that the Godot editor `dlopen`s, under `lto = "fat"` (`Cargo.toml:32`) with linker section GC and GDExtension hot-reload, are a cross-platform footgun for zero semantic gain. A `const` array is compile-time checked, reviewable in a diff, deterministically ordered — which the introspector's golden-snapshot tests depend on — and costs exactly one line.

The `Registrar` is the only API a provider sees:

```rust
// src/actions/registrar.rs
pub(crate) struct Registrar<'a> {
    index: &'a mut BindingIndex,
    registry: &'a mut ActionRegistry,
    forest: &'a mut Forest,
    diagnostics: &'a mut Vec<PanelDiagnostic>,
    owner: MappingOwner,           // set by `owner()`; tags every rule
    provenance: Provenance,        // Builtin here; User for the vimrc loader
}

impl Registrar<'_> {
    /// Every rule this provider registers is tagged MappingOwner::Host(tag).
    pub(crate) fn owner(&mut self, tag: &'static str);
    pub(crate) fn surface(&mut self, spec: &'static SurfaceSpec);
    pub(crate) fn action(&mut self, spec: &'static ActionSpec) -> ActionId;
    /// Parsed by `crate::config::panelmap::parse_panel_line` — the SAME
    /// parser as `.godot-vimrc`, so defaults cannot drift from the documented
    /// syntax. Under Provenance::Builtin a parse failure is a
    /// `debug_assert!` + `log::error!`, never a warn-and-skip.
    pub(crate) fn defaults(&mut self, text: &'static str);
}
```

### 7.2 A debugger panel, end to end

Godot's debugger dock is `EditorDebuggerNode` (an `EditorDock` in 4.8-dev, `editor/debugger/editor_debugger_node.h:47`), and its stack-frame list is a `Tree` (`ScriptEditorDebugger::stack_dump`, `editor/debugger/script_editor_debugger.h:151`). Its step commands are already registered editor shortcuts: `debugger/step_over` (F10), `debugger/step_into` (F11), `debugger/continue` (F12) — `editor/debugger/debugger_editor_plugin.cpp:48-53`.

```rust
// src/actions/providers/debugger.rs — the ENTIRE new file.
use crate::actions::{
    ActionCtx, ActionSpec, Anchor, Caps, FocusChain, Outcome, Registrar, Seal, SurfaceSpec,
};
use crate::navigation::{handle_navigation, NavDirection};

pub(crate) const OWNER: &str = "godotvim.debugger";

// ── 1. Declare the surface. `parent` is the ONLY specificity mechanism.
static DEBUGGER: SurfaceSpec = SurfaceSpec {
    id: "dock.debugger",
    parent: Some("dock"),          // inherits j/k/h/l, /, <CR>, <Esc> for free
    seal: Seal::Open,
    grants: |_chain| Caps::empty(),
    // Pure over the enriched chain — no Gd<T>, so it is unit-testable from a
    // literal FocusChain with no Godot runtime. Deliberately narrow: it claims
    // only the stack-frame Tree, so it can never shadow `dock` for a widget it
    // does not understand.
    probe: |chain: &FocusChain| {
        (chain.index_of_ancestor("EditorDebuggerNode").is_some()
            && chain.focus_is("Tree"))
            .then_some(Anchor::Node(0))
    },
    on_key: None,
    yields_to_engine: false,       // not an editor surface
};

// ── 2. Declare named actions. ONE signature for every verb in the plugin.
static STEP_OVER: ActionSpec = ActionSpec {
    id: "godotvim.debugger.step_over",
    desc: "Debugger: step over",
    requires: Caps::empty(),
    host_invocable: true,          // needs no panel focus to be meaningful
    // Declared, not discovered inside `run` — this is what makes the
    // injection-cycle audit total. `s` != F10, so the audit accepts.
    delegates: Some("debugger/step_over"),
    // We own the KEY, Godot owns the BEHAVIOUR. On Godot < 4.6 this returns
    // Declined (EditorSettings::get_shortcut is unbound there), so the raw key
    // reaches Godot and F10 still works. Honest degradation, not a black hole.
    run: |cx: &mut ActionCtx<'_>| cx.run_editor_shortcut("debugger/step_over"),
};

static FRAME_UP: ActionSpec = ActionSpec {
    id: "godotvim.debugger.frame_up",
    desc: "Debugger: select previous stack frame",
    requires: Caps::VNAV,          // affordance, not widget class
    host_invocable: false,         // meaningless without the stack Tree focused
    delegates: None,
    run: |cx: &mut ActionCtx<'_>| {
        let Some(t) = cx.target().cloned() else { return Outcome::Declined };
        let mut moved = false;
        for _ in 0..cx.params.count() {          // clamped to 1..=100
            moved |= handle_navigation(&t, NavDirection::Prev, 0);
        }
        // Declining is how we compose with Godot: at the top of the list the
        // key falls through to the Tree's own handling instead of dying here.
        if moved { Outcome::Handled } else { Outcome::Declined }
    },
};

// ── 3. Register. Defaults are authored in the SAME text users type and go
//       through the SAME parser, so they cannot drift from the documentation.
pub(crate) fn register(r: &mut Registrar<'_>) {
    r.owner(OWNER);
    r.surface(&DEBUGGER);
    r.action(&STEP_OVER);
    r.action(&FRAME_UP);
    r.defaults(
        "panelmap dock.debugger s godotvim.debugger.step_over\n\
         panelmap dock.debugger K godotvim.debugger.frame_up\n",
    );
}
```

```rust
// src/actions/providers/mod.rs — the ONLY line added outside the new file.
    filesystem::register,
    debugger::register,   // <-- the entire diff
    dock::register,
```

Note the position: `dock.debugger` is a child of `dock`, so its `register` must appear before `dock::register`. That is validation rule **V4** below, and it is checked, not trusted.

**What happens at startup.** `ActionPlane::rebuild` clears the index, runs every `PROVIDERS` entry in array order tagging rules `MappingOwner::Host("godotvim.debugger")`, then applies the single resolved vimrc last-writer-wins. `s` and `K` are single-key LHS on a surface that is not editor-reachable — `dock.debugger` is not an ancestor of any `editor.*` surface — so neither the grammar-prefix guard nor the multi-key rejection applies. The injection-cycle audit resolves `debugger/step_over`'s event array, finds F10, builds the edge `s → F10`, finds no cycle, and accepts.

**What the user gets, without the provider author writing any of it.** With the debugger's stack Tree focused: `j`/`k` move the selection and `Enter` activates it, inherited from `dock`; `/` focuses the dock's filter box if it has one; `Esc` returns to the script editor; `s` steps over; `K` walks up the stack; Ctrl+hjkl still moves between panels, because `panel` is `dock`'s ancestor and `dock.debugger`'s seal is `Open`. All six of `godotvim.debugger.*` and the inherited verbs appear in `:panelmap` with their descriptions, are cross-referenced against `EditorSettings.get_shortcut_list()` by `:checkhealth godotvim`, and are rebindable:

```vim
panelunmap dock.debugger s
panelmap   dock.debugger <C-n> godotvim.debugger.step_over
panelmap   dock.debugger 3K    godotvim.debugger.frame_up count=3
```

and reachable from the editor without a panel key at all:

```vim
nnoremap <leader>ds <Action>(godotvim.debugger.step_over)
```

### 7.3 What the author supplies, and what is free

**Supplied.** The surface id and its parent; a `Seal`; a `grants` function; a `probe` that is a pure `fn(&FocusChain) -> Option<Anchor>`; an `on_key` hook if the subsystem has state that must be reconciled on every keystroke; one `ActionSpec` per new verb with `id`, `desc`, `requires`, `host_invocable`, `delegates` and `run`; a `defaults` string in `panelmap` syntax; and — this one is mandatory, not optional — at least one golden `FocusChain` fixture per new surface, because the partition audit (V5) runs over the golden table and a surface with no fixture is a surface nobody proved disjoint. The author also owns declination discipline: an action that cannot act must return `Declined`, or it becomes a key sink on its surface.

**Free.** Rebindability, `panelunmap`, `native` give-back, and last-writer-wins precedence. Listing in `:panelmap`, explanation in `:panelmap <lhs>`, and conflict reporting in `:checkhealth`. Every generic verb the parent surface already has — parenting to `dock` is the entire cost of inheriting `godotvim.item.next/prev/expand/collapse/activate`, `godotvim.dock.search` and `godotvim.focus.editor`. The capability gate, so a verb requiring `HIERARCHY` is automatically inert on an `ItemList` with no widget name in the dispatcher. Seal and barrier semantics. Echo suppression via `Repeat::Suppress`. The opt-in physical-position probe for Colemak/Dvorak/AZERTY. Langmap remapping and Shift canonicalization, so `<S-r>`, `<S-R>` and `R` intern identically. The `panic_guard` envelope. And the four front-ends onto one id — with one honest asymmetry: the panel key runs *inline* with `&mut GodotVimCore`, while `:action`, `<Action>(…)` and `HostRequest::RunAction` are relayed one drain later through `PendingUiAction` (same frame, zero `call_deferred`, so zero frames of latency, but strictly after `apply_ui_update` and `set_input_as_handled`). The practical constraint that follows: a registry action must not publish user-visible text through `handle_show_message`, because the UI snapshot has already been taken. `:panelmap` prints the asymmetry.

**Not free.** `Caps` is a closed `bitflags` vocabulary. A provider that needs a genuinely new affordance must edit `src/actions/caps.rs` — bits 5..15 are reserved and documented, but the ceiling is real. The mitigation is that almost every such constraint belongs in the probe instead: `dock.debugger` self-restricts to `focus_is("Tree")` rather than inventing a `STACKFRAME` bit. Likewise, a new verb with genuinely new behaviour is a Rust PR; `RuleTarget::Shortcut(path)` and `delegates` remove that cost only for behaviour Godot already has a registered shortcut for, and only on Godot ≥ 4.6.

### 7.4 Ordering and validation: why a third party cannot silently collide

Every rule below is enforced at registration. Diagnostics are severity-split by `Provenance`: `Builtin` failures are a `debug_assert!` plus `log::error!` — a shipped default that does not load is a build failure, never a warning — while `User` failures accumulate into the `Vec<PanelDiagnostic>` that `:checkhealth godotvim` prints, with per-line warn-and-skip following the repo's existing idiom at `src/settings/reader.rs:240-264`.

| # | Rule | What it prevents |
|---|---|---|
| **V1** | A `SurfaceId` may be declared at most once. A second declaration is an error, never an overwrite. | A third-party provider silently redefining `dock` or `panel`. |
| **V2** | `ActionRegistry::register` errors if the id string is already registered to a *different* `&'static ActionSpec` pointer. | `NameRegistry::register` is idempotent, so a duplicate id would silently alias a builtin verb and the third-party `run` would never execute. |
| **V3** | Every `SurfaceSpec::parent` must name a declared surface, and the parent graph must be acyclic. | A typo'd parent producing a one-element path with no `panel`, i.e. a surface that silently loses Ctrl+hjkl. |
| **V4** | The `PROVIDERS` order must be a linear extension of the forest's descendant-before-ancestor order, and `unknown` (the unique surface with a total probe) must be the last probing entry. | `dock` claiming before `dock.debugger`, which would make the child surface unreachable and its bindings silently dead. |
| **V5** | Partition audit over the golden `FocusChain` table: if two surfaces both claim a fixture, they must be forest-related (ancestor/descendant). Otherwise it is an error naming both ids and the fixture. | Two unrelated third-party surfaces both claiming the same focused control, resolved by array position — nondeterministic to the authors, invisible to the user. |
| **V6** | Within the **builtin layer**, a `Host`-owned rule may not overwrite another `Host`-owned rule at the same `(surface, lhs)`. First writer wins; the second is a diagnostic. Since builtins register first, a third party cannot displace a builtin binding. | A debugger provider quietly stealing `j` on `dock` from every other panel. A third party that wants a builtin key must ship a *commented recipe* for the user's vimrc, not a default. |
| **V7** | The single resolved vimrc is applied **after** all builtins and always wins, last-writer-wins in file line order. Teardown of a provider is a full `BindingIndex` rebuild — never `MappingTrie::remove_by_owner`. | `MappingEntry` carries exactly one owner and `insert` overwrites at the same LHS, so owner-scoped removal would delete a builtin binding that a third party happened to shadow. |
| **V8** | A rule on any surface that is an ancestor-or-self of an `editor.*` surface is rejected if its LHS is multi-key, or if `starts_vim_grammar_sequence(lhs[0])` holds. | `panelmap panel <C-w> …` turning `<C-w>s` into a destructive bare `s`. Note the predicate is over the *declared forest*, so `panel` is covered — "reject on `editor.*` surfaces" alone is not sufficient. |
| **V9** | Referential integrity at the end of `rebuild`: every `RuleTarget::Action(id)` resolves in `ActionRegistry`; every `Rule.surface` is declared; no rule's `requires` names a `Caps` bit that no surface on its own forest path can ever grant. | A rule that can never fire, which is otherwise indistinguishable from a mystery. |
| **V10** | The injection-cycle audit rejects any rule whose canonicalized first LHS key lies on a cycle of `delegates` edges, self-collision being the length-1 case. | `panelmap dock.debugger <F10> <Shortcut>(debugger/step_over)` — a hard editor hang that `panic_guard` cannot catch. |

Two of these deserve a note on scope. V6 is the rule that makes "cannot silently collide" true rather than aspirational, and its cost is real: a provider genuinely cannot claim a key another provider already claimed on the same surface, even if its own surface is more specific. That is intentional — a *more specific surface* is the supported way to win, and it costs one `parent` link. V5 is only as strong as the golden table; it proves disjointness over the fixtures that exist, not over all reachable editor states, which is why a fixture per new surface is a hard requirement rather than a courtesy.

### 7.5 Runtime registration

An optional addon that appears and disappears at runtime uses the same shapes: `plane.register_provider(register_fn, MappingOwner::Host(tag))` and `plane.unregister_provider(tag)`, the latter triggering a full rebuild from `PROVIDERS` plus the resolved vimrc — milliseconds for a few hundred rules, and correct by V7. The `generation` counter on `ActionPlane` bumps on every rebuild, which invalidates the `FocusChain` cache key and any reserved-prefix state, so no stale-context class of bug survives a reload.

---

## 8. vim-core Decision

**Verdict: do not fork.** `Cargo.toml:19` stays `vim-core = { git = "https://github.com/hmdfrds/vim-core.git", tag = "v0.7.1" }`. No v0.8.0 is cut, no lockfile churn, no two-repo release coupling.

**The Cargo.toml delta for this entire feature is zero lines.** `bitflags = "2"` (`Cargo.toml:20`) is already a real dependency and is what `Caps` is built on; `compact_str = "0.7"` (`:23`) is already there and matches vim-core's resolved version. `ahash` is *not* added — `BindingIndex` holds one entry per declared surface (nine builtins plus third-party), so a `Vec<SurfaceBindings>` scanned linearly beats hashing at that size, removes the dependency question, and — decisively — gives the introspector a deterministic iteration order, which neither `std::collections::HashMap` (SipHash) nor `ahash::RandomState` can, both being randomly seeded per process. `smallvec` is *not* promoted either: it appears in `src/` only under `src/testing/bridge_tests/{undo,macros}.rs`, so it is genuinely dev-only (`Cargo.toml:27`), and every site the design used it for is built at most once per keystroke. Use `Vec`. If a profile ever shows allocation pressure, promotion is a one-line manifest change and a type-alias swap.

### 8.1 The keymap layer is architecturally forbidden from importing execution

This is the formal proof that vim-core's real trie runs headless inside Godot's global `input()`, where no `VimSession` exists. `vim-core/vim-core/src/keymap/mod.rs:15-20` reads:

```
//! # ⚠️ ARCHITECTURE ENFORCEMENT ⚠️
//!
//! **ALLOWED imports**: std, `primitives::Mode` (for mode-aware classification)
//! **FORBIDDEN imports**: commands, grammar, effects, execution
//!
//! Keymap is a LOW layer - consumed by grammar for key classification.
```

and it is enforced in CI by `vim-core/tests/architecture.rs`. Nothing re-exported from `keymap/mod.rs:36-51` can transitively require an engine, a session, a document, or a host. Every API this design consumes, verified against the checked-out tag:

- `MappingTrie` — `insert(&mut self, lhs: &[KeyEvent], entry: MappingEntry)` (`trie.rs:334`), `lookup(&self, prefix: &[KeyEvent]) -> TrieLookup<'_>` (`:427`), `remove(&mut self, lhs: &[KeyEvent]) -> Option<MappingEntry>` (`:363`), `remove_by_owner(&mut self, owner: &MappingOwner) -> usize` (`:517`), `entries(&self) -> Vec<(Vec<KeyEvent>, &MappingEntry)>` (`:572`). No session, host, buffer or document parameter anywhere.
- `TrieLookup<'a>` (`trie.rs:264-279`) — `NoMatch | ExactOnly(&'a MappingEntry) | Prefix { exact: Option<&'a MappingEntry> }`. It is `#[non_exhaustive]`, so every match in `resolve.rs` needs a wildcard arm. `<nowait>` is short-circuited *inside* `lookup()` at `trie.rs:443-445`.
- `MappingEntry::new(sequence: Vec<KeyEvent>, kind: MappingKind) -> Self` is a `pub const fn` (`trie.rs:44`); `.with_owner(MappingOwner)` (`:154`), `.with_description(Option<CompactString>)` (`:161`), `.nowait() -> bool` (`:207`), `.description() -> Option<&str>` (`:176`).
- `NameRegistry::{new, register(&mut self, name: &str) -> u32, get_name(&self, id: u32) -> Option<&str>, get_id(&self, name: &str) -> Option<u32>}` (`name_registry.rs:28,35,52,58`) — idempotent and append-only.
- `KeyEvent::action(id: u32) -> Self` is a `pub const fn` (`key_event.rs:184`); `KeyEvent::new(Key, Modifiers)` (`:48`); `with_latin(mut self, latin: Key) -> Self` (`:259`). `latin_key` is excluded from `PartialEq`/`Hash`, so layout metadata never perturbs a trie lookup — but `KeyEvent::new` drops it, which is why `canonicalize` restores it explicitly (§4).
- `MAX_KEY_SEQUENCE_LEN: usize = 8` (`keymap.rs:140`). The shell caps LHS at the same value deliberately, so the two planes cannot disagree.
- `LangmapTable::parse(s: &str) -> Result<Self, LangmapError>` (`langmap.rs:140`), `remap_key_event(&self, key: KeyEvent) -> KeyEvent` (`:240`).
- `vim_core::execution::parse_keys_from_string(text: &str) -> Vec<KeyEvent>` (defined `execution/engine/macro_replay.rs:325`, re-exported `execution/mod.rs:112`) — the only *public* multi-key parser. `KeyEvent::from_vim_notation` is single-key only, which is why `gg` and `<Space>ff` need it.
- `vim_core::grammar::Parser::process(&mut self, key: KeyEvent, keymap: &Keymap, mode: Mode) -> GrammarResult` (`grammar/parser.rs:107`) and `GrammarResult::is_pending(&self) -> bool` (`grammar/result.rs:60-62`), used at *registration time* to close the `<C-w>` grammar hole (§3, §5). The same argument licenses it: `grammar/mod.rs:9-10` forbids `commands, execution, effects, dispatch, mode`.
- `VimEngine::could_start_mapping(&self, key: KeyEvent) -> bool` (`execution/engine/mapping.rs:78-82`), reached through `VimController::could_start_mapping` (`src/controller/mod.rs:677-679`), which works in both `ControllerPhase::Attached` and `Detached`.

**Explicitly not consumed, and this correction is load-bearing.** `VimSession::take_key_interest_if_dirty` is declared in `impl VimSession<SessionHost>`; godot-vim holds `VimSession<GodotHost>` (`src/controller/mod.rs:94`), so the method does not exist for it, and `key_interest_dirty` is `pub(crate)`. `KeyInterestSet`'s fields are four `Vec<String>` of Vim-notation strings (`key_interest.rs:43-53`), not a keyed set of `KeyEvent`; membership would cost a `to_vim_notation()` allocation plus a binary search per keystroke. Nothing in this design touches either — `vim_claims` is exactly `could_start_mapping` (§5).

**What is re-implemented, honestly accounted (~150 lines).** `TypeaheadBuffer::resolve_key` / `ResolveResult` / `TypeaheadFlags` are `pub(in crate::execution::engine)` and genuinely unreachable. The re-implementation is small for an architectural reason rather than by luck: the shell's RHS is a closed vocabulary of exactly one opaque slot key, so there is no recursive expansion, no NOREMAP propagation, no `<expr>`, no recursion-depth guard. What remains is prefix accumulation capped at 8, the three-way `TrieLookup` branch, and timeout resolution. That same closed vocabulary is what makes the `sandbox.rs` whitelist extension defensible — the two properties are the same property.

### 8.2 The `res://` committed-vimrc failure has no fix under a fork

`config::path::resolve` returns `res://.godot-vimrc` whenever that file exists (`src/config/path.rs:38-48`), tested *before* `user://` (`:51-59`), and `res://` is version-controlled. A fork that teaches vim-core a `<surface=…>` map modifier means a teammate still on v0.7.1 opening the same repository parses `<surface=dock>` as part of an LHS — a junk mapping, installed silently, with no diagnostic. The fork camp's architect could not solve this and mitigated with a banner.

`panelmap` has no such failure in either direction, but the *mechanism* differs from what was originally claimed and the difference matters. A `panelmap` line reaching vim-core's `source_config_text` is not rejected by the parser: `parse_named_command`'s terminal `else` returns `Ok(ExCommand::Custom { command })` (`grammar/ex_parser.rs:818-821`), and the line is then dropped by `source_config_text`'s `_ => {}` catch-all at `source.rs:90` — not by the `Err` continue at `:40-41`. Two consequences. (a) Inertness rests on a catch-all rather than a parse failure, so the sandbox whitelist (§6) must stand on its own and may not lean on "vim-core cannot parse it". (b) `ExCommand::Custom` is precisely godot-vim's own custom-ex-command channel, so `panelmap` is simultaneously a config directive and a reachable cmdline token — which is why `:panelmap` as the introspector command and `panelmap` as the directive are deliberately the same name routed through one interception point (§5), not an accidental collision.

An older godot-vim preserves the line as `ConfigLine::Other(raw_line)` (`src/config/parser.rs:99`) and re-emits it verbatim (`writer.rs:58-61`). So a project vimrc carrying panel bindings is safe to commit while teammates are mid-upgrade — the property the forked alternative could not offer.

### 8.3 The cost accounting was wrong once, in the optimistic direction

The fork bill went **7 → ~14 production sites** under critique, plus `:map`-style listing that does not exist in vim-core today, plus `parse_unmap_command` parser work, plus a context layer in `vim-core/vim-core/src/keymap/keymap.rs` — which is **1214 lines** against `GlobalRule::FileLineLimit(1500)` declared at `vim-core/tests/architecture.rs:213`. Call it 1100–1400 net lines in a second repository behind arch-test drift gates, and a permanent two-repo tag-desync tax on every hotfix. Staying pinned also keeps this feature entirely clear of vim-core's `DriftSensitiveEnumExhaustiveness` on `Key`, `EffectHostCoverage`, and the `HostRequestKind::ALL` coverage assertion. A design that must be right about a second repo's architecture gates in order to ship the first repo's feature is a schedule risk this plan will not take for a bounded gain.

### 8.4 Three vim-core extensions explicitly rejected

**`Mode::Dock`.** Impossible at acceptable cost. `Mode` is `#[non_exhaustive]` (`primitives/mode.rs:92`), so godot-vim can neither add a variant nor match one exhaustively; `ModeHandler` is sealed (`pub trait ModeHandler: super::sealed::Sealed`, `mode/types.rs:47`, with the private `mod sealed` at `mode/mod.rs:53`). A new variant breaks roughly fifteen exhaustive matches inside vim-core plus the FFI crates under `#![deny(non_exhaustive_omitted_patterns)]`. It is also semantically wrong: dock focus is not an editing mode — no buffer, no cursor, no registers, no operator-pending state.

**`MappingMode::Dock`.** Cheap in vim-core, and still rejected. `ModeMap<T>([T; MappingMode::COUNT])` (`keymap.rs:28`) bakes `COUNT = 7` (`:174`) into every mode-indexed array in the crate. More importantly it is *strictly less expressive* than the requirement: a flat discriminant cannot represent an ordered forest path with fall-through (`dock.filesystem → dock → panel`), cannot carry a capability gate, and has no place to put declination. It would buy a namespace and cost the mechanism.

**Hijacking `Keymap`'s filetype overlay as a named-context layer.** The tempting one — rejected on semantics, not availability. `Keymap` holds exactly one `active_filetype: Option<CompactString>` (`keymap.rs:292`), so a *stack* like `[dock.filesystem, dock, panel]` is inexpressible. Its merge order is fixed at buffer > filetype > global (`keymap.rs:605-618`), not this design's precedence. Driving it from focus changes would clobber genuine `.gd`/`.gdshader` filetype mappings on every dock click. And decisively, its RHS is a key sequence expanded through the vim grammar: a dock `j` would run through a user's `nnoremap j gj` and try to move a cursor that does not exist. Action ids make that class of RHS cross-contamination structurally impossible.

---

## 9. Rejected Alternatives

### 9.1 The four losing camps

**Engine-first — fork vim-core to v0.8.0 with a context layer on `Keymap` and a document-free `resolve_ui_key` / `ui_drain_next_key` / `ui_key_interest` channel on `VimEngine` (camp `SURFACES`).** Its premise correction was right and is adopted in substance: the brief's "no vim session, no mode, no mapping trie" is false — `ControllerPhase::Detached { engine, state }` (`src/controller/mod.rs:92-96`) carries a live `VimEngine`, and `engine()`/`engine_mut()` work in both phases. The conclusion does not follow. Under critique the design conceded that context lookup must fold *context nodes only* — no buffer layer, no filetype layer, no global user layer — because otherwise one enabled `inoremap jj <Esc>` swallows characters in every dock filter box. Once the context lookup is disjoint from every editor layer, what a `Keymap` instance provides over a standalone `MappingTrie` is the merge function (~30 lines, and this design needs a different one anyway, since capability gating and declination have no analogue in `merge_lookups`) plus `:map` syntax. Weighed against §8.2's unfixable `res://` failure and §8.3's cost, two of three judges named forking a fatal reject. Its best ideas survive: the never-hold-a-prefix-in-the-editor invariant is now a registration-time rule enforced by `vim_core::grammar::Parser`, and it is *stronger* than SURFACES stated it — the guard covers single-key LHS too, because `<C-w>` alone is the whole bug, and it covers any surface that is an ancestor-or-self of an `editor.*` surface, because `panel` is `editor.nav`'s declared parent.

**Shell-first — a second, parallel binding world beside the editor's (camp `AUX`, post-cut).** AUX produced the two most elegant mechanisms in the debate and both are adopted verbatim: the declared context forest (which replaces every magic-integer ladder) and the key-class-aware seal (bare keys stop at a sealed surface and reach the control's own `gui_input`; modifier-bearing keys continue to the root). What sank it as a whole design is the missing naming spine: without an action-id namespace, the shell plane is a genuinely separate system with its own listing command, its own timeout knob, its own unbind verb, and no route from `:action`, `<Action>(name)`, or `HostRequest::RunAction`. Its concrete failure was the `EDITOR_RESERVED` denylist — a hand-maintained list that must grow every time vim-core adds a Ctrl-prefixed grammar entry, and which provably cannot cover `<C-\>`: that key is intercepted unconditionally at `grammar/parser.rs:129-133` and appears nowhere in `CORE_KEYMAP` (`grep -n backslash vim-core/src/keymap/core.rs` returns zero hits). Asking `Parser::process(...).is_pending()` catches both without any list.

**Godot-native — `EditorSettings` + `Shortcut` resources as the canonical store, with Editor Settings ▸ Shortcuts as the rebinding UI (camp `SHEETS`).** Fatal on five independent axes; see §9.2 for the full list. SHEETS nonetheless produced three of the strongest grafts, all adopted: the sample-once `NodeFacts` seam that makes every probe a pure `fn(&FocusChain) -> Option<Anchor>`, `RuleTarget::Shortcut(path)` delegation so the plugin owns the KEY and Godot owns the BEHAVIOUR, and per-binding echo policy (`Repeat::Suppress`) to kill the held-Ctrl+J `grab_focus` storm.

**Context-declarative — a boolean predicate language over context atoms (camp `FOCAL`): DNF normalization, `||`, whole-chain `!`, `A > B` ancestor matching, `slot == value`.** Users asked for rebindable keys and would have received a compiler. The 90% task — move panel-left from Ctrl+h to Ctrl+w h — required typing a ~70-character predicate twice, and the identity of `navunmap` under DNF normalization was never defined. Zed shipped `!` and `>` semantics wrong for years and had to make a breaking change; re-litigating that inside a Godot plugin is unjustifiable. All three judges named it a fatal reject. Its specificity ladder was separately fatal: it derived depth from the Godot focus chain, which is generic-at-the-leaf and specific-at-the-ancestor (`dock` comes from `gui_get_focus_owner()`, FileSystem membership from `fs_dock.is_ancestor_of(control)` at `src/navigation/filesystem_explorer.rs:380-386`), so deepest-wins *inverts* the FileSystem-first-refusal hard constraint. FOCAL's useful half is grafted: provider defaults render as read-only rows with an Override button that writes exactly one user line, and the surface token is optional because the action declares its home.

### 9.2 Mechanism-level rejects (binding)

**Godot's `EditorSettings` / `Shortcut` system as the canonical binding store.** Verified fatal five ways. `add_shortcut` / `get_shortcut` / `get_shortcut_list` were ClassDB-bound only in Godot 4.6-stable, while `addons/godot_vim/godot_vim.gdextension:3` declares `compatibility_minimum = "4.5"`; pinned gdext v0.4.5 generates zero shortcut methods on `EditorSettings`. The store is per-user and project-global (`editor_settings-4.N.tres`), so a project file's unbind leaks into every other project the user opens. A `Shortcut` is an unordered array of OR'd alternatives and structurally cannot express a sequence or a context. `add_shortcut` returns void and discards the object you passed in exactly when the user has already rebound — so "cache the returned Shortcut" caches the default for precisely the users who customized. And `Gd<Shortcut>` / `Gd<InputEventKey>` cannot be constructed under `cargo test` in a `cdylib`, which would put the entire dispatch core outside this repo's only test harness. Retained read-only, `has_method`-gated, as a conflict-report source and a delegation target.

**Speculative consumption of an ambiguous multi-key prefix.** `set_input_as_handled()` at the `_input()` stage has no synchronous replay channel, so a consumed prefix key is destroyed forever. Concretely: with `gg` bound on a dock, typing `g` then `o` to type-search for "Goblin" loses both keys, because `Tree::allow_search` defaults true and the Tree never sees them. On editor-reachable surfaces it is worse — any `<C-w>`-prefixed binding turns `<C-w>s` into a bare `s`, a destructive edit, from a shipped default. Replaced by explicit prefix *reservation*: opt-in, printed by the introspector and `:checkhealth`, forbidden outright on any surface that is an ancestor-or-self of an editor surface, and complemented by `Tree::set_allow_search(false)` applied to exactly the focused control that reserves one.

**Hand-assigned magic-integer specificity ladders in any form** — FOCAL v1's scene-tree depth, SHEETS v1's `240/230/220`, the winner's own literal `rank: u16`. A global z-index namespace two independent providers will collide in undiscoverably. It produced concrete failures in the debate: one ladder put `foreign`(240) above `search`(220) with `foreign` as a hard stop, killing every dock filter box and the FS create prompt outright. All three judges flagged it. Replaced by `SurfaceSpec::parent`, forming a forest whose depth is authored, published, printed by `:panelmap`, and validated at registration — two surfaces that can claim the same `FocusChain` without an ancestor relation are a registration error surfaced in `:checkhealth`, not an array-order coin flip.

**Predicate / DNF languages as the primary user-facing interface.** Any design whose second phase is "build a language" will not finish, and the 90% task must not require typing a boolean expression twice. Conjunction of a single named scope — with the scope optional because the action declares its default — is the ceiling. This is a statement about the *primary* interface: forest inheritance already provides the composition users actually need.

**`inventory` / `linkme` link-time provider registration.** Life-before-main constructors in a `cdylib` that the Godot editor `dlopen`s, under `lto = "fat"` (`Cargo.toml:32`) with linker section GC and `reloadable = true` hot-reload, is a cross-platform footgun for zero semantic gain, and it is hostile to the `panic_guard` envelope. A `const PROVIDERS: &[fn(&mut Registrar<'_>)]` array is compile-time checked, reviewable in a diff, and matches the repo's own idiom (`FILTER_CHAIN` in `src/controller/passthrough.rs`, `PRESETS` in `src/config/presets.rs`). The requirement is zero *dispatcher* edits, which the array satisfies exactly.

**A blanket physical-keycode fallback evaluated per match arm.** This is the live `/`-on-physical-J bug: the hjkl block at `src/navigation/dock.rs:111-146` precedes the `Key::SLASH` arm at `:127`, so on a Colemak/Dvorak layout the logical `/` never reaches `handle_slash`. Generalized naively it fires `godotvim.fs.yank_path` when a QWERTZ user presses `z`. Replaced by exactly one ordered probe applied once to the whole key — as-typed after langmap, then `latin_key` normalization, then US-QWERTY position; there is no fourth "named key with SHIFT cleared" stage, because that is a per-binding tolerance (`Rule::shift_tolerant`) rather than a key identity (§5.3) — with the physical probe opt-in per rule, set on exactly the fourteen rules that have that fallback today (fourteen *rules*, not thirteen: `R`/refresh also routes through `resolve_key` at `src/navigation/filesystem_explorer.rs:88`, feeding the `(Some(Key::R), true)` arm at `:95`).

**Moving completion-popup routing out of `gui_input` into the `input()` registry.** `_input` is registered per-viewport (`input_group = "_vp_input" + id`), so it never fires for floating script editors, where `gui_input` does — completion navigation would silently stop working there. It also sits outside the IME guard, so a Japanese user committing a kanji candidate with Enter would have the commit swallowed, and it cannot express `try_handle_completion`'s "handled but do not consume" outcome that lets Godot's own CodeEdit navigate the list. If completion becomes rebindable it stays on the `gui_input` transport (P9).

**Two further mechanisms that silently defeat stated hard constraints.** *Per-rule `Arbitration::Yield`* is deleted entirely, not merely restricted: it moves to `SurfaceSpec::yields_to_engine`, `true` on `editor.nav` alone and unreachable from `panelmap` syntax, so the duplication gap (a `panel` rule that never consults `vim_claims`) becomes unrepresentable rather than asserted at startup (§3, §5). *Arbitrary key-sequence right-hand sides* and *security toggles expressible as `set` lines* are both rejected: the sandbox whitelist extension is defensible only because the RHS is a closed vocabulary that re-enters no expander, and `is_safe_non_mapping_line` passes any `set` whose tokens are not in `BLOCKED_SET_OPTIONS`, so a `set panelsafety=off` line in a `res://` vimrc would sail straight through. The shell plane therefore reads no `set` option at all; every security knob lives under `EditorSettings` `plugins/GodotVim/security/`, which a `res://` file structurally cannot write.

---

## 10. Implementation Phases

Ten phases. Every phase names its dependencies, its files, its verification gate, and whether it ships and reverts on its own.

**Dependency edges.** `P0 → {P1, P2, P4}`; `P1 → P5`; `P2 → {P3, P4, P5}`; `P3 → P6`; `P4 → {P5, P6}`; `P5 → {P6, P7, P9}`; `P6 → {P7, P8, P9}`. Two of those are easy to miss and are stated rather than implied: `P2 → P4` because `surface.rs`'s `ChainNode::widget_caps`, `SurfaceSpec::grants` and `SurfacePath::caps` all need `Caps` from P2's `caps.rs`; and `P1 → P5` because P5's registration guard needs `starts_vim_grammar_sequence`, `parse_lhs` and `canonicalize` from P1's `keys.rs`. The numbered sequence P0..P9 is a valid topological order of that edge set.

Three properties this ordering buys. **(i)** P0–P5 are all zero-behaviour-change except P3, which is purely additive (`:action <registry id>` starts working; nothing existing changes) — so the entire foundation lands with a trivial revert story. **(ii)** P6 is the single behaviour-change merge, and P0's characterization suite is its acceptance gate. **(iii)** P7 is the first user-writable binding and it lands strictly *after* load-time validation (P5) and the introspector (P6) — which is what the judge panel required when it fatally rejected shipping a config surface before validation and introspection exist.

Two prerequisite commits land ahead of, or inside, P0 and are independently revertable:

- **Rename `DockInputResult::Ignored` → `Declined` in place** (16 sites: `src/navigation/dock.rs` ×14, `src/navigation/filesystem_explorer.rs` ×2), as a standalone leading commit *before* the characterization suite is written. A `type` alias does resolve variants (RFC 2338, Rust 1.37+), but it cannot *rename* one: `DockInputResult::Ignored` would fail `E0599` against an `Outcome` that has no such variant. The stronger reason is that P0's characterization assertions name the variant, so folding this into P2 would make P2 edit P0's tests and void its own "P0 passes unmodified" gate. Adding `Declined` as an associated `const` alias instead is worse, not better: a const in pattern position does not participate in exhaustiveness checking.
- ~~Thread `attached_editor_id` into `is_navigable_control` so a non-attached CodeEdit stops being a legal Ctrl+hjkl target.~~ **Withdrawn — do not implement.** It was written, reviewed, and reverted. The trap it targets self-heals via auto-attach (§1.3), so the edit removes working navigation into the shader editor and any second visible `CodeEdit` while fixing nothing. `window.rs:166-169`'s reasoning is correct for plain `TextEdit` precisely because `TextEdit` can never be attached; `CodeEdit` always can. If a future change ever narrows the attach policy, revisit this — and put the rule in `is_window_candidate` / `is_cycle_candidate`, keeping `is_navigable_control` a pure type predicate.

### P0 — Characterization harness (deps: none)

**Why first.** `grep -rn "cfg(test)" src/navigation/ src/scene_tree.rs` returns **zero matches** — verified. `src/plugin/input.rs` (532 lines) likewise has no test module. This subsystem has never been tested, and every later phase is unverifiable until it is.

**Work.** Build `src/testing/focus_fixture.rs`: `FocusChain` as literal data (class chain, instance ids, precomputed facts) with no `Gd<T>` anywhere, plus `src/testing/action_recorder.rs`, a `RecordingCtx` that logs `grab_focus` / `emit_signal` / `push_input` calls with their arguments instead of performing them. Then characterize *current* behaviour of all seven hardcoded sites: the Ctrl+hjkl direction table (`src/navigation/window.rs:23-39`), dock `j`/`k`/`h`/`l` including the `DockKind::Tree` gates (`src/navigation/dock.rs:111-146`), `/`/Enter/Esc (`:124-132`), search-box Esc/Enter with its Shift tolerance (`:138-159`), FS `a`/`d`/`r`/`y` with the `(Some(Key::R), true)` refresh discriminant (`src/navigation/filesystem_explorer.rs:87-97`), the FS prompt's hardcoded `Key::ESCAPE` (`src/plugin/mod.rs:789-798`), and both tri-state `Declined` returns (`dock.rs:120-126`, `:195-198`).

**Files.** NEW `src/testing/{focus_fixture,action_recorder}.rs`; NEW `#[cfg(test)]` modules in `src/navigation/{dock,dock_nav,window,cycle,filesystem_explorer,focus}.rs`.

**Gate.** `cargo test` green, with four pre-registered `#[ignore]`d red tests documented as known bugs (§11). Reviewer check: no `Gd<T>` appears in any fixture type.

**Shippable / revertable.** Yes, trivially — it adds only tests.

### P1 — One key vocabulary, behind today's dispatcher (deps: P0)

**Work.** Widen `physical_to_ascii` (`src/bridge/input.rs:97`) and `normalize_key_for_mapping` (`src/controller/process.rs:541`) to `pub(crate)`. Add `src/actions/keys.rs` with `probes()`, `canonicalize()`, `validate_lhs_key()` and `parse_lhs()`. Plumb `LangmapTable::parse(controller.engine().options().langmap())` into a table rebuilt at the config-source sites — new plumbing; godot-vim uses langmap nowhere today. Call `parse_godot_key` once at the top of `handle_input_impl` and thread the probe list into the three existing handlers, deleting `dock_hjkl`/`hjkl_to_dock` (`dock.rs:71-86`), `direction_from_hjkl`/`hjkl_direction` (`window.rs:23-39`) and `resolve_key`/`is_fs_key` (`filesystem_explorer.rs:363-378`).

**Files.** MOD `src/bridge/input.rs`, `src/controller/process.rs`, `src/plugin/input.rs`, `src/navigation/{dock,window,filesystem_explorer}.rs`; NEW `src/actions/keys.rs`.

**Gate.** P0's suite passes unchanged **and** three of the four red tests turn green: `/` on a physical-J layout now reaches `handle_slash`; numpad Enter works (`get_named_key` already maps `KP_ENTER → Key::Enter` at `src/bridge/input.rs:23`); Ctrl+hjkl resolves via the physical probe from the editor. Plus the layout matrix and the corrected normalization proptest (§11).

**Shippable / revertable.** Yes. No registry exists yet; this is a pure consolidation of three ad-hoc key decoders into one.

### P2 — Control plane: every executor becomes a named `ActionSpec` (deps: P0)

**Work.** Zero behaviour change. Move `DockInputResult` to `src/actions/outcome.rs` as `Outcome` (the variant rename already happened, so a `type` alias genuinely does compile every remaining site). Add `Caps` (final vocabulary: `VNAV`, `HIERARCHY`, `ACTIVATE`, `TEXTENTRY`, `FILEOPS`) plus the transitional `dock_kind_of`, `ActionSpec`, `ActionId`, `ActionRegistry` over `vim_core::keymap::NameRegistry`, `ActionCtx` with the single `defer_grab_focus()` helper collapsing the four `call_deferred("grab_focus", &[])` copies (`dock.rs:231-234`, `window.rs:118-121`, `cycle.rs:96-99`, `filesystem_explorer.rs:264-267`), `Params` with a clamped `count()`, and `RuleTarget::{Action, Native, Shortcut}`. Every existing executor body becomes an `ActionSpec::run`. The old match arms now call `registry.run(id, &mut ctx)`.

**Two members of §4's types land here as stubs, and the staging is deliberate.** `ActionCtx` ships **chain-less** at P2 — its `chain: Rc<FocusChain>` field cannot exist before `FocusChain` does, and `FocusChain` is P4 — so `run_action_now(spec, params, target)` builds a `{ viewport, target, params, plugin }` context until P4 adds the fifth field. Its signature does not change when it does. Likewise `plane.rs` ships holding only `registry`, `diagnostics` and `generation`: `forest: Forest` arrives with P4 and `index: Rc<BindingIndex>` with P5, which is why P5's file list reads `MOD src/actions/plane.rs`. Neither stub is a shortcut around a dependency; both are the reason P2 can depend on P0 alone.

**Files.** NEW `src/actions/{mod,outcome,caps,action,plane}.rs`; MOD `src/navigation/{dock,dock_nav,window,cycle,filesystem_explorer}.rs` (bodies extracted, logic unchanged); MOD `src/plugin/input.rs`.

**Gate.** P0's suite passes **unmodified** — that is the entire point of doing this behind the old dispatcher. Plus: id interning is idempotent; `as_key(id).key() == Key::Action(id.0)`; every registered action id contains a dot (a registration-time assertion, because P3's name split depends on it); the `RecordingCtx` proves `godotvim.item.activate` on an ItemList emits **both** `item_selected(idx)` and `item_activated(idx)` (`dock.rs:210-221`), which a "verbatim move" can silently halve.

**Shippable / revertable.** Yes.

### P3 — Host → plugin action bridge, one namespace (deps: P2)

**Work.** This is the old Phase 6, moved five phases earlier because it is the cheapest, least risky change in the plan and it de-risks F3 before anything depends on it. Add `PendingUiAction::{RunRegistryAction, PanelCommand}` — **name-carrying, never id-carrying**, so `bridge` never depends on `crate::actions` (the `Vimdebug(CompactString)` variant at `src/bridge/godot_host.rs:39-52` is the precedent). Probe the registry as link 0 of the `HostRequest::RunAction` arm (`src/host/dispatch.rs:437`); `crate::host::execute` already receives `pending_ui_actions: &mut Vec<PendingUiAction>` at `src/host/dispatch.rs:74`, so no signature changes. Add both variants to the forward-to-plugin arm of `handle_host_pending_ui_action` (`src/controller/process.rs:240-244`), or they are swallowed inline and never reach `&mut GodotVimCore`. Reconcile the dead `Effect::WindowMove*` → `CompoundAction::WindowNav` route (constructed at `src/effects/dispatch.rs:946-982`, discarded at `src/bridge/godot_host.rs:236`) by deleting the promotion rather than leaving two sources of truth. The consumer runs the P2 **chain-less** `ActionCtx`: P3 depends on P2 alone precisely because no `FocusChain` is constructed here, and the `chain` field arrives with P4 without touching this call site.

**Gate.** All the way through, in one frame: the relay runs inside the same `handle_gui_input_impl` call with no `call_deferred`, so "deferred by one drain" costs zero frames — it defers only past `apply_ui_update` and `set_input_as_handled`, which constrains exactly one thing: registry actions must not publish text through `handle_show_message`. Regression test that every name in `list_all_commands()` still resolves through the custom-command chain and is not shadowed; assert the `name.contains('.') && !name.contains('/')` split is total over `list_all_commands()` ∪ the full registry.

**Files.** MOD `src/host/dispatch.rs`, `src/bridge/godot_host.rs`, `src/controller/{process,mod}.rs`, `src/plugin/mod.rs`, `src/effects/{dispatch,compound}.rs`, `src/navigation/cycle.rs`.

**Shippable / revertable.** Yes, and it is purely additive: `:action godotvim.focus.left` starts working; nothing existing changes behaviour.

### P4 — Surface plane, with `classify_focus` still live (deps: P0)

**Work.** Add `src/actions/surface.rs` and the eight providers. `FocusChain::sample()` performs all Godot work once per focus change, computing `widget_caps` from `node.is_class(c)` (a `ClassDb::is_parent_class` memoization is an optional optimisation, unverified against gdext v0.4.5 — §4.12). Every `probe` is a pure `fn(&FocusChain) -> Option<Anchor>`, where `Anchor::Rootless` (returnable only by `unknown`) reproduces `classify_focus`'s no-focus-owner arm. Classification is an ordered total function, and the array order is `[editor, prompt, searchbox, filesystem, dock, foreign, unknown, panel]`. **`foreign::register` is not last.** `unknown`'s probe is the total one — it is the fallthrough that mirrors `src/navigation/focus.rs`, and it must be the last *probing* entry — so `foreign` sits immediately before it: put `foreign` after `unknown` and it becomes unreachable, every Foreign context falls to `unknown` → `panel`, and Ctrl+hjkl is consumed inside a Project Settings `LineEdit` in direct violation of `src/plugin/input.rs:90`. Put it first and `prompt` / `searchbox` / `filesystem` / `dock` / `editor` lose their first refusal. `editor.insert` is written as the exact *complement* of `editor.nav` within "focus is the attached CodeEdit", not as an enumeration, because `Mode` is `#[non_exhaustive]`. **`classify_focus` is not deleted here.** The `graph` surface is dropped: GraphEdit already samples to `unknown`, whose parent is `panel`, so the invariant holds without it.

**Files.** NEW `src/actions/surface.rs`, `src/actions/providers/{mod,panel,dock,filesystem,searchbox,prompt,editor,foreign,unknown}.rs`; MOD `src/navigation/dock_search.rs`, `src/scene_tree.rs`.

**Gate.** The golden table of ~40 literal `FocusChain` fixtures (§11); the partition audit; the shadow `debug_assert!` comparing new classification against `classify_focus` over a scripted corpus with a divergence count of exactly zero; the restated focus-trap invariant.

**Shippable / revertable.** Yes — the new classifier compiles and is fully tested while nothing in production reads it. This is the split that makes the old Phase 3 buildable at all.

### P5 — Binding plane + panel-line parser + provider defaults (deps: P2, P4)

**Work.** `src/actions/bind.rs` (per-surface `MappingTrie` with an opaque `SlotId` payload and a side rule arena, held in a `Vec<SurfaceBindings>` in registration order) plus `src/config/panelmap.rs` — the pure `parse_panel_line` grammar, with **load-time rejection of unknown action ids**, `validate_lhs_key`, the `starts_vim_grammar_sequence` registration guard, and the multi-key ban on any surface that is an ancestor-or-self of an `editor.*` surface. `BindingIndex::rebuild` mirrors `VimController::reload_config` (`src/controller/mod.rs:701-724`) exactly — clear, then builtin defaults at `MappingOwner::Host(tag)` in `PROVIDERS` order, then the single resolved vimrc, last-writer-wins. `Provenance::{Builtin, User}` splits the diagnostic policy: a builtin default that fails to parse is a `debug_assert!` plus `log::error!`; user diagnostics accumulate for `:checkhealth`. **The index is built and nothing reads it yet** — marked with `#[allow(dead_code)]` plus a doc comment naming P6 as the consumer, which is the repo's own idiom (`src/config/writer.rs:71`).

**Files.** NEW `src/actions/{bind,panelmap}.rs`, `src/config/panelmap.rs`; MOD `src/actions/plane.rs`.

**Gate — this phase's headline deliverable.** The full-default-set golden test asserted three ways (§11). Plus the registration-guard tests: `<C-w>` and `<C-\>` rejected; `<C-h>`/`<C-j>`/`<C-k>`/`<C-l>` accepted; a nine-key LHS rejected; `<S-1>` rejected with the "write `!`, not `<S-1>`" diagnostic.

**Shippable / revertable.** Yes. It is a dead index with a live test suite.

### P6 — Dispatcher cutover + introspector, shipped together (deps: P1, P2, P3, P4, P5)

**Work.** Add `src/actions/{resolve,dispatch,introspect}.rs`. Rewrite `handle_input_impl` (`src/plugin/input.rs:32-172`) to the staged model of §5: guards → probes → sample → barrier → hooks → resolve → arbitrate → execute → consume. `classify_focus` and `FocusContext` are deleted **in the same commit that removes their last caller** — `src/plugin/input.rs:76` and the two `FocusContext` matches at `:88-123` and `:137-167`. `DockKind` moves to `dock.rs` rather than dying with `focus.rs`, because `dock.rs:128,113,178-200` and `filesystem_explorer.rs:72,109,125,474-498` use it independently. Route the FS prompt's `gui_input` (`src/plugin/mod.rs:789-798`) through the same resolver on surface `prompt`, with an explicit primary-viewport discriminator rather than deleting either transport. Add re-entrancy protection for `run_editor_shortcut` (keyed fingerprint, frame window, hard per-frame budget) and the `has_shortcut_api` gate with `try_call`. Ship the introspector in the same merge, at three tiers: **push** (every index build emits diagnostics through `godot_warn!`/`godot_print!` directly, never `log::warn!`, because the default log level is Off), **pull via the command line** (`:panelmap`, `:panelmap <lhs>`, `:checkhealth godotvim`, intercepted in `GodotHost::handle_request_inner` beside `vimdebug` and relayed to the plugin as `PendingUiAction::PanelCommand` — the plane lives on `GodotVimCore`, and a multi-line report belongs in the Output panel rather than the one-line status bar), and **pull with no command line at all** (a one-shot `plugins/GodotVim/diagnostics/print_report` bool that `on_settings_changed` reads and writes back to false — Editor Settings is reachable from the menu bar with zero scripts open and zero working keybindings).

**Files.** NEW `src/actions/{resolve,dispatch,introspect}.rs`; MOD `src/plugin/input.rs` (net ~90 lines removed), `src/plugin/mod.rs`, `src/bridge/{godot_host,godot_calls}.rs`, `src/host/dispatch.rs`; DELETE `src/navigation/focus.rs`.

**Gate.** P0's characterization suite passes **UNMODIFIED**. That is the acceptance gate for the whole refactor. Plus the resolver table tests covering every precedence row (§11) and the introspector golden snapshots.

**Shippable / revertable.** Shippable, and it is the *only* behaviour-change merge in the plan. Revertable as one commit, but it is the largest single revert; that is deliberate and is why P0–P5 exist.

### P7 — Config surface: sandbox, file sourcing, round-trip, dialog (deps: P5, P6)

**Work.** `ConfigLine::PanelMap(Box<PanelPayload>)` joins the six variants at `src/config/types.rs:161-170`; one classification arm in `parser.rs` **and** one in the `" disabled: ` sub-branch at `parser.rs:40-51`, which otherwise degrades a disabled panelmap line to `ConfigLine::Comment`; one `serialize` arm in `writer.rs` (the match has no wildcard, so the compiler forces it). Add the third branch to `sandbox_config_text` between the `is_mapping_line` block and the terminal `else`, with a closed deny-by-default token whitelist; `<Shortcut>(path)` denied at project level, `native` permitted at every tier. Fix both controller guards — the one at `src/plugin/mod.rs:1188-1193` and the second at `on_config_saved` `:808-810` — and, critically, place the shell-plane rebuild so that it is **not** inside `if let Some(text)`: `apply_vimrc_policy` returns `None` under `ProjectVimrc::Disabled`, and `source_config_from_disk` returns early at `:1176-1178` when the file is missing, so the naive placement means a security setting or simply having no vimrc **destroys the builtin Ctrl+hjkl defaults**. Add the MappingDialog "Panel Keys" tab (read-only default rows + Override) and the `:checkhealth` line for `res://` shadowing `user://`. Add `panelsafety`/`pnsf` to `BLOCKED_SET_OPTIONS` as defence in depth.

**Files.** MOD `src/config/{types,parser,writer,sandbox,mapping_service}.rs`, `src/plugin/mod.rs`, `src/ui/mapping_dialog.rs`.

**Gate.** The document-level round-trip proptest and the `disabled_panelmap_roundtrip` unit test (§11); the sandbox anti-drift proptest; a typo'd action id warns once and inserts nothing; hot-reload swaps the index atomically and one broken rule does not remove the working ones; panel bindings load and hot-reload while the controller is in `ControllerPhase::Detached`.

**Shippable / revertable.** Yes, and this is the first release in which a user can write a binding.

### P8 — Sequences: reserved prefixes, shell timer, scoped `allow_search` (deps: P6)

**Work.** `src/actions/sequence.rs`: the pending buffer capped at `MAX_KEY_SEQUENCE_LEN` = 8, prefix reservation, and a **dedicated** `Gd<Timer>` — never `self.mapping_timer`, whose callback early-returns with no editor attached (`src/plugin/input.rs:311`) and would otherwise flush the *engine's* typeahead into the open file while focus is on a dock. `timeoutlen` when detached comes from `SettingsSnapshot.timeoutlen` (`src/settings/snapshot.rs:150`). `Tree::set_allow_search(false)` / `ItemList::set_allow_search(false)` applied to the focused control only when its surface reserves a bare prefix, restored on focus change, teardown, and inside `panic_guard` recovery. `pending` clears on execute, `NoMatch`, timeout, focus-owner change, plugin disable and config reload. It does **not** clear on echo: while `pending` is non-empty an echo is **consumed and discarded** (§5.8) — it does not extend the buffer, does not re-run a candidate, and does not restart the shell timer. That is what stops an auto-repeating key turning `gg` into `ggg` *without* aborting a legitimate held-key sequence, which clearing would do; and not restarting the timer is what stops a held prefix key from keeping the buffer alive forever.

**Gate.** `gg` fires on the second `g`; `g` then `x` consumes both and reports `DeadPrefix`; an unreserved `g` is never consumed and never buffered; `<nowait>` on `d` fires immediately despite `dd`; a dock prefix armed with no script open resolves on timeout and `self.mapping_timer` was never armed; forced-panic test asserting `allow_search` restoration.

**Shippable / revertable.** Yes, and it is genuinely optional — the core is P0–P7.

### P9 — Proof surfaces: debugger provider, completion on `gui_input` (deps: P5, P6)

**Work.** (a) `src/actions/providers/debugger.rs` exactly as written in §7, as executable proof that a new panel costs one file plus one manifest line. (b) Model `src/controller/completion.rs`'s hardcoded `<C-@>`/`<C-n>`/`<C-p>`/Up/Down/Tab/Enter/Esc/Backspace as an `editor.completion` surface resolved from the **`gui_input`** transport inside the existing IME guard.

**Gate.** (a) `git diff --stat` on `src/actions/{resolve,dispatch}.rs` and `src/plugin/input.rs` must be **empty**. (b) Characterization tests written *first* against today's `try_handle_completion`, covering the `Some(true)` / `Some(false)` / `None` trichotomy and the IME-preedit path, then re-run unchanged. Manual verification in a floating script editor.

**Shippable / revertable.** Independently revertable, and deliberately last: (b) carries the highest regression risk in the plan.

---

## 11. Test Strategy

### 11.1 The premise

`grep -rn "cfg(test)" src/navigation/ src/scene_tree.rs` returns **nothing** — verified against the working tree. `src/plugin/input.rs` (532 lines, the transport this design rewrites) has no test module either. Meanwhile the repo has 969 `#[test]` functions in `src/`, 123 of them in `src/bridge/input.rs` alone, and `src/testing/` contains `mock_text_edit.rs` plus `bridge_tests/{cursor,dispatch,text_mutations,multi_cursor_sync,undo,scroll}.rs` (122 tests). So the house style is proven and the gap is specific: everything editor-side is tested through pure-Rust stand-ins, and everything shell-side is tested by hand. The test strategy is therefore not a section of the plan — it is P0, and every later phase is gated on it.

### 11.2 Layer 1 — the characterization suite (the centrepiece)

**One rule governs the whole refactor: the suite written in P0 must pass UNMODIFIED after P6.** Not "pass with updated expectations" — byte-identical assertions. It is the acceptance gate for the dispatcher cutover, and it is why the `Ignored → Declined` rename lands *before* the suite is written rather than during P2.

It pins all seven hardcoded sites, and four cases in particular that a reviewer would otherwise "tidy":

- **`godotvim.item.activate` on a Tree returns `Handled` even with no selection** (`src/navigation/dock.rs:204-208` emits `item_activated` with no arguments and returns `Handled` unconditionally), while the ItemList branch declines when nothing is selected (`:195-197`). Pre-existing asymmetry, preserved verbatim, pinned explicitly.
- **The ItemList double-emit** — `item_selected(idx)` *and* `item_activated(idx)` (`:190-194`), because different editor docks listen to different signals. Verified through `RecordingCtx`, since `Gd<Control>::emit_signal` is unreachable headlessly.
- **`j`/`k` on a RichTextLabel scroll 50px and return `Handled`** (`src/navigation/dock_nav.rs:284-298`), reached because `dock.rs:111-126` has no `DockKind` gate on `j`/`k` at all. The tests must name **EditorHelp's `class_desc` and EditorLog's `log` specifically**, not "a RichTextLabel": in Godot 4.8-dev a RichTextLabel is focusable only where `set_selection_enabled(true)` has run, so a generic fixture would pin a case that cannot occur. Both are reachable, and `:Output` is a shipped ex-command that focuses one.
- **Ctrl+H navigates panels from a focused attached CodeEdit in Normal mode with no user mapping present**, and `:nnoremap <C-h> x` flips it. This is the direct regression guard on the arbitration seam.

**Four deliberately-RED tests**, `#[ignore]`d at P0 with a documented reason and turning green at a named phase. A refactor that fixes a bug it did not intend to fix is suspicious, so every intended fix is pre-registered as an assertion rather than discovered in review:

| Red test | Turns green | Why it is red today |
|---|---|---|
| `/` on a logical-SLASH / physical-J layout reaches `handle_slash` | P1 | the hjkl block at `dock.rs:111-146` shadows the SLASH arm at `:127` |
| Numpad Enter activates in a dock | P1 | never routed through `parse_godot_key`, though `get_named_key` already maps `KP_ENTER → Key::Enter` (`src/bridge/input.rs:23`) |
| Ctrl+hjkl navigates from the **editor** on a Cyrillic/Greek layout | P1 | the escape-hatch block at `src/plugin/input.rs:110-116` matches the logical keycode only and `return false`s before the physical fallback at `:126` is reached |
| `handle_escape_from_dock`'s plain-`TextEdit` fallback is a one-way trap | unfixed, low probability | `dock.rs:255-266` focuses a `TextEdit`, which is always `Foreign` (`focus.rs:85-87`) and can never auto-attach, so neither `Ctrl+hjkl` nor `Esc` escapes it |

### 11.3 Layer 2 — headless vs. live

**Headless (the bulk of the value).** The entire dispatch model is a pure function of `(FocusChain, KeyProbes, pending, BindingIndex, ActionRegistry, vim_claims, is_echo)` — the registry is in that tuple because `requires` lives on `ActionSpec` and the capability gate must resolve `RuleTarget::Action(id)` before it can test anything (§4.8). No `Gd<T>` appears in `surface.rs`, `bind.rs`, `resolve.rs`, `keys.rs` or `plane.rs`. That is not aesthetics — a `cdylib` GDExtension cannot instantiate Godot objects under `cargo test`, which is exactly why this repo already tests the pure `translate_key(keycode, physical, unicode, ctrl, alt, shift, meta)` with 123 tests instead of constructing `Gd<InputEventKey>`. So surface probes, the forest walk, the barrier/seal lattice, capability gating, the candidate fold, declination, consumption policy, prefix reservation and timeout resolution are all table-driven unit tests with no runtime.

**Live editor only.** There is **no Control/Tree/ItemList harness in this repo and this plan does not build one.** Deferred `grab_focus` timing, `set_input_as_handled()` on a floated window's viewport, IME preedit in P9, GDExtension hot-reload teardown, `Tree::set_allow_search` restoration after a forced panic, and the Godot 4.5 shortcut-API degradation path all get a written manual matrix in `docs/`, run against 4.5 and 4.8-dev. The forced-panic case additionally gets an automated test, because it is the one with silent editor-wide blast radius. Anyone reading this plan should size the manual QA accordingly: the safety net for the executors themselves is that they are verbatim moves a reviewer can diff, plus the `RecordingCtx` proving *which* executor ran with *what* arguments and *what* it returned.

### 11.4 Layer 3 — property tests where combinatorics beat examples

`proptest = "1.4"` is already a dev-dependency (`Cargo.toml:26`), currently used only in `src/bridge/codec.rs`.

**(a) Config round-trip — the property must be document-level, not text-level.** The obvious `parse(serialize(parse(x))) == parse(x)` over *text* is the wrong property and would pass even with the parser bug present: an enabled `panelmap` line becomes `ConfigLine::Other(raw_line)` (`src/config/parser.rs:99`) and a `" disabled: panelmap` line becomes `ConfigLine::Comment(raw_line)` (`:68`), and the writer re-emits both verbatim (`src/config/writer.rs:22-25`, `:58-61`). The line round-trips as text while silently losing its identity as a `PanelMap`. The real property is **document identity**: `parse_config(serialize(&doc)) == doc`, generated from typed `ConfigDocument`s, which needs `PartialEq, Eq` derived on `ConfigLine` and `MappingPayload` (`ParsedMapping` already has them at `src/config/types.rs:141`). Backed by one hand-written unit test, `disabled_panelmap_roundtrip`, structurally copied from the existing `disabled_user_mapping_roundtrip` (`parser.rs:372-404`) — that is the test that actually catches the missing `" disabled: ` branch. Keep the text-level fixpoint as a weaker second property over interleaved documents, because it catches writer/parser drift on the other variants.

**(b) Key normalization — the original property was specified wrong.** As written it was `canonicalize(from_vim_notation(x)) == canonicalize(translate_key(godot_event_for(x)))` over all printable ASCII. That fails for every shifted digit and symbol: `<S-1>` parses to `Char('1') + SHIFT`, while `translate_key` for Shift+1 yields `Char('!') + NONE` on US and `Char('+') + NONE` on DE (SHIFT is stripped for printables with no CTRL/ALT/META at `src/bridge/input.rs:404-406`). Folding is *impossible* without a layout table, and `physical_to_ascii` is hardcoded US-QWERTY (`src/bridge/input.rs:97-99`). **Corrected property:** for all `x` that pass `validate_lhs_key`, `canonicalize(from_vim_notation(x)) == canonicalize(translate_key(godot_event_for(x)))` — with shifted non-alphabetic `Char` LHS *rejected at load* with a diagnostic telling the user to write the character literally (`!`, not `<S-1>`), which is also what Vim requires. That removes the counterexample from the domain by construction rather than pretending to handle it, and prevents a silently-dead binding. The alphabetic case is pinned separately and must hold in all three spellings: `<S-r>`, `<S-R>` and `R` all intern as `Char('R') + NONE`.

**(c) Sandbox anti-drift.** `panel_line_is_safe` deliberately duplicates the panel token grammar rather than calling `parse_panel_line`, mirroring the repo's own documented duplication of vim-core's abbreviation logic (`src/config/sandbox.rs:260-261`) — the discipline is that raw text is sanitized *before* any structured parse. The duplication is protected by a proptest, not a fixed table: every line `parse_panel_line` accepts with an `Action` or `Native` target must pass `panel_line_is_safe`, and every line it rejects must fail it.

### 11.5 Layer 4 — registration-time validation

Every guard that moved from dispatch time to registration time gets a test, because a registration-time rule that silently stops firing is invisible:

- `starts_vim_grammar_sequence` returns **true** for `<C-w>` and `<C-\>`, **false** for `<C-h>`, `<C-j>`, `<C-k>`, `<C-l>`, and **true** for bare digits (conservative in the safe direction: `panelmap panel 3 …` must not break `3j`). This is a vim-core-version-pinned test — if it disagrees after a future bump, the guard's shape holds but its exclusion set needs re-derivation.
- Multi-key LHS is rejected on any surface that is an **ancestor-or-self** of an `editor.*` surface, which includes `panel` — not merely on `editor.*` itself.
- A nine-key LHS is rejected with `LhsError::TooLong`; an unknown action id is rejected with a diagnostic and inserts nothing; `<Shortcut>(path)` is rejected when `has_shortcut_api()` is false, with an explicit "unavailable on Godot < 4.6" line rather than a silently omitted section.
- **Referential integrity over the whole index:** every `RuleTarget::Action(id)` resolves in `ActionRegistry`, every `Rule.surface` is a declared `SurfaceId`, and **no rule requires a `Caps` bit that no surface on its own forest path can ever grant** — a rule that can never fire is a registration error, not a mystery.

### 11.6 Layer 5 — the surface-partition audit

Replacing an exclusive `if/else` classifier with N independent probes structurally loses mutual exclusivity. Three mitigations, all in P4:

1. The classifier stays an **ordered total function**: probes run in `PROVIDERS` order — `[editor, prompt, searchbox, filesystem, dock, foreign, unknown, panel]` — and the first match wins. `unknown` holds the only total probe and is therefore the last *probing* entry; `foreign` sits immediately before it, because behind a total probe it is unreachable and Ctrl+hjkl would be consumed in a Project Settings `LineEdit` (`src/plugin/input.rs:90`), while ahead of `prompt`/`searchbox`/`filesystem`/`dock`/`editor` it would steal their first refusal. `panel` is last and never probes. The golden table pins the resulting order, and V4 rejects any array that violates it.
2. A **golden fixture table of ~40 literal `FocusChain`s** covering the nine real editor chains (FileSystem Tree, FileSystem list, Scene tree, Inspector, Script list, dock filter box, FS create prompt, attached CodeEdit, foreign Project-Settings LineEdit) plus EditorHelp `class_desc`, EditorLog `log`, GraphEdit, a focused Button inside a dock (must sample to `unknown`, **not** `dock` — if the `dock` probe is ever widened beyond `Tree|ItemList|RichTextLabel`, the currently-dead `find_best_nav_target` recursion at `src/navigation/dock_nav.rs:120-121` wakes up and `j`/`k` moves a Tree the user is not focused on), and a `nodes: vec![]` no-focus-owner fixture. Each asserts exact surface path, caps and seal.
3. A **partition audit** that fails the suite if two surfaces claim the same fixture without an ancestor relation, plus the **editor partition tautology**: for every `Option<Mode>` including `None` and every recognized variant, exactly one of `editor.nav` / `editor.insert` claims a focused attached CodeEdit (`nav ^ ins`). This is a tautology by construction — `editor.insert` is written as a negation — and is asserted anyway so a future probe edit fails the suite rather than a user's Ctrl+H.
4. The **restated focus-trap invariant**, existentially quantified: for every control that `find_window_candidates` or `find_cycle_candidates` can return, there *exists* a reachable editor state in which `sample(C)` yields a non-`Barrier` path containing `panel`. The existential is required and correct — the attached CodeEdit in Insert mode is a `Barrier` by design (Ctrl+H is backspace) and is not a trap, because Esc returns to Normal. A foreign `CodeEdit` **does** have such a state — focusing it attaches it (§1.3) — so it satisfies the invariant and must stay reachable. A plain `TextEdit` does not, which is why it is absent from `is_navigable_control` and why `handle_escape_from_dock`'s fallback to one is the invariant's only live violation.
5. A **shadow `debug_assert!`** running the new classification alongside `classify_focus` over a scripted keystroke corpus, asserting a divergence count of exactly zero — live throughout P4 and P5, removed in P6 when `classify_focus` dies.

### 11.7 Layer 6 — "the full shipped default set loaded"

This is P5's acceptance gate and the reason P5 exists as its own phase. Without it, a `Caps` rename, an action-id rename, a surface rename or an LHS notation change makes the warn-and-skip loader silently drop a binding and the plugin ships with, say, thirteen of fourteen defaults. The repo has the identical hole today for engine presets: `default_config_has_presets` (`src/config/mapping_service.rs:485-490`) asserts only `assert!(!presets.is_empty())` at `:489`, so one of nineteen `PRESETS` surviving would pass. Asserted three ways:

1. **Count and provenance.** After registering all `PROVIDERS`, `plane.diagnostics` must be **empty**, and `plane.index.rules().count()` must equal the expected table's length. A builtin default that fails to parse is a build failure, not a warning — warn-and-skip is the right policy for user text and the wrong policy for shipped text, which is what `Provenance::{Builtin, User}` separates.
2. **Exact table.** A `const SHIPPED_DEFAULTS` transcribed from the migration table (§12), compared field by field: surface, LHS as `Vec<KeyEvent>`, target, `consume`, `repeat`, `physical`, owner. A silent field change — `Void` quietly becoming `Elastic` on `godotvim.focus.left` — fails.
3. **Referential integrity**, as in §11.5.

While here, close the pre-existing engine-side hole with the same shape: `assert_eq!(svc.preset_mappings().len(), PRESETS.len())` — a one-line change to an existing test.

### 11.8 Layer 7 — the P6 resolver table

Table-driven over `(FocusChain, key, pending) -> Resolution`, one row per precedence rule: Ctrl+hjkl consumed with no focus owner *and* no target found (`Anchor::Rootless` + `Void`); Ctrl+H **not** intercepted in Insert/Replace/Select (`Barrier`); `:nnoremap <C-h> x` beating panel nav via `editor.nav`'s `yields_to_engine`, evaluated on the *same* `KeyEvent` that produced the winning candidate so it still wins on a Cyrillic layout; FS `d` beating dock nav; FS `j` falling through to `dock`; `h` inert on an ItemList (capability); `j` at list end declining to Godot; bare `j` reaching a filter box (`Sealed`); Ctrl+L escaping a filter box (`Sealed` + modifier); `native` terminating the walk so a shallower `panel` rule is never consulted, versus `panelunmap` letting it continue. Plus a floating-script-editor test asserting `set_input_as_handled()` hits the transport's own viewport, and golden-file snapshots of `:panelmap <lhs>` output for six fixtures — stable only because the index iterates in registration order.

---

## 12. Migration & Compatibility

### 12.1 Zero-config day one

Zero-config migration is achieved **by construction, not by a compatibility shim**. Today's hardcoded bindings ship as provider defaults tagged `MappingOwner::Host(tag)`, authored in the same `panelmap` text a user would type and parsed by the same parser, so a default can never drift from the documented syntax. A user who upgrades and never opens `.godot-vimrc` sees identical behaviour.

**Nineteen rules, five surfaces, twenty-one actions, two non-action targets.** Fourteen rules carry `<physical>` — that is the corrected count: thirteen is the number of distinct physical *keys*, but `R`/refresh also goes through `resolve_key` (`src/navigation/filesystem_explorer.rs:88,95` → `:363-378`, whose `is_fs_key` covers `A|D|R|Y`), so the *rule* count is fourteen.

| Surface | LHS | Target | Flags | Reproduces |
|---|---|---|---|---|
| `panel` | `<C-h>` | `godotvim.focus.left` | `<physical> <void> <norepeat>` | `src/plugin/input.rs:124-135` — result discarded, `set_input_as_handled()` fires with no focus owner and no target found |
| `panel` | `<C-j>` | `godotvim.focus.down` | same | same |
| `panel` | `<C-k>` | `godotvim.focus.up` | same | same |
| `panel` | `<C-l>` | `godotvim.focus.right` | same | same |
| `dock` | `j` | `godotvim.item.next` | `<physical>` | `dock.rs:113-119`; declines at end-of-list |
| `dock` | `k` | `godotvim.item.prev` | `<physical>` | `dock.rs:120-126` |
| `dock` | `h` | `godotvim.item.collapse` | `<physical>` | `dock.rs:128` — the `DockKind::Tree` gate becomes `requires: Caps::HIERARCHY` |
| `dock` | `l` | `godotvim.item.expand` | `<physical>` | `dock.rs:136` |
| `dock` | `/` | `godotvim.dock.search` | `<physical>` | `dock.rs:151`, `:130` |
| `dock` | `<CR>` | `godotvim.item.activate` | — | `dock.rs:152`; `requires: Caps::ACTIVATE` reproduces `RichTextLabel => Ignored` at `:200` |
| `dock` | `<Esc>` | `godotvim.focus.editor` | — | `dock.rs:153` |
| `dock.filesystem` | `a` | `godotvim.fs.create` | `<physical>` | `filesystem_explorer.rs:91` |
| `dock.filesystem` | `d` | `godotvim.fs.delete` | `<physical>` | `:92` — keeps `trigger_dock_shortcut` verbatim |
| `dock.filesystem` | `r` | `godotvim.fs.rename` | `<physical>` | `:93` |
| `dock.filesystem` | `y` | `godotvim.fs.yank_path` | `<physical>` | `:94` |
| `dock.filesystem` | `R` | `godotvim.fs.refresh` | `<physical>` | `:95` — `<S-r>` and `R` fold to one key, so the shipped default is spelled `R` |
| `searchbox` | `<CR>` | `godotvim.search.accept` | `<shift>` | `dock.rs:166,147` — rejects ctrl/alt/meta, tolerates shift |
| `searchbox` | `<Esc>` | `godotvim.search.cancel` | `<shift>` | same |
| `prompt` | `<Esc>` | `godotvim.prompt.dismiss` | — | `src/plugin/mod.rs:795` |

**`editor.nav` carries zero rules.** It exists only to declare `yields_to_engine: true`, and the gate runs once on the anchor surface after resolution has produced a winner: if the anchor yields and `vim_claims(matched_key)` holds, dispatch is abandoned and the key flows to `gui_input`. That is a faithful transcription of `should_intercept_hjkl` (`input.rs:88-123`), which is likewise computed from the *context*, before any binding is consulted. The consequence for migration is that the `<C-h>` rule exists exactly **once**, on `panel` — duplication between `panel` and `editor.nav` is unrepresentable rather than asserted, and a user who rebinds to `panelmap panel <M-h> …` inherits the `:map` escape hatch automatically.

Three further fidelity notes:

- **Insert-like modes.** `editor.insert` is a `Barrier` with zero bindings, so "never intercept in Insert/Replace/VirtualReplace/CommandLine/Select" is structural rather than conditional. Select stays in the insert-like set per `input.rs:95-100`.
- **FileSystem-first refusal.** `dock.filesystem` is deeper in the declared forest than `dock`, so `a/d/r/y/R` get first refusal while `j/k` fall through. The hardcoded `if fs_result.is_consumed() { … } else { … }` at `input.rs:140-150` becomes precedence, not a branch.
- **The FS prompt.** `prompt` is `Sealed` and is classified *before* `searchbox` **and before `foreign`** — the latter matters because `foreign`'s predicate includes "LineEdit with no sibling nav control", and if it claimed the prompt first the surface would become a `Barrier`, `Esc` would never dismiss through `input()`, and Ctrl+hjkl would be blocked there. So bare `<CR>` — which is unbound on `prompt` — reaches `text_submitted` through the LineEdit's own `gui_input`. This replaces both the hardcoded `Key::ESCAPE` at `plugin/mod.rs:789-798` and the `is_prompt_active` special case at `input.rs:156-159`.

**Cycle focus ships no default key**, and that is not a regression: today there is no dock key for cycle either — `CycleNext`/`CyclePrev` reach `handle_cycle_focus` only as vim-core effects from `<C-w>w`/`<C-w>W` in the attached editor (`src/controller/process.rs:516-535` → `src/navigation/cycle.rs:35-37`). Every candidate default is worse than nothing: a bare letter steals Tree type-to-search, a Ctrl combo violates "unbound keys are never consumed" and collides with Godot's own shortcuts (`<C-w>` is `script_editor/close_file`, `<C-n>` is `editor/new_scene`), and a multi-key LHS is illegal on `panel` because `panel` is an ancestor of `editor.nav`. The affordance ships three other ways: both ids are registered and reachable from `:action`, `<Action>()`, `panelmap` and the dialog tab; `:checkhealth` prints a note when nothing binds them; and commented recipe lines (`<M-]>` / `<M-[>`) are appended to `.godot-vimrc.sample`, which is already an entirely-commented opt-in file.

### 12.2 A user with an existing `.godot-vimrc`

**Nothing in their file changes and nothing in their file is required.** `generate_default_config` emits no `panelmap` lines, so the file is byte-identical after upgrade and diffs cleanly in git. On load, `ActionPlane::rebuild` finds zero panel lines in layer 2, and layer 1 alone reproduces §12.1.

Four cases that previously would have broken and now do not, each with a regression test:

1. **`ProjectVimrc::Disabled` with a `res://.godot-vimrc` present.** `apply_vimrc_policy` returns `None` (`sandbox.rs:378-381`); `rebuild(None)` still installs the builtin defaults, so `<C-h>` still resolves to `godotvim.focus.left`. Under the original draft — which put the rebuild inside `if let Some(text)` — a *security setting* would have destroyed cross-panel navigation.
2. **No config file at all.** `source_config_from_disk` returns early at `src/plugin/mod.rs:1176-1178`; the shell-plane rebuild is hoisted above that return, so a fresh project with no vimrc still gets the full default keyset.
3. **A `res://` file whose every panel line is stripped by the sandbox.** The stripped lines become comments in the sanitized *string*; the file on disk is untouched and the builtin defaults are unaffected.
4. **The controller in `ControllerPhase::Detached`.** The rebuild is outside the controller borrow entirely, so panel bindings load and hot-reload with no script open. (`self.controller` is `Some` from `enter_tree` at `plugin/mod.rs:128` until `exit_tree` at `:173` and `recover_controller_from_panic` never nulls it — so "controller is `None` when no script is open" is not the failure mode; *detached* is.)

**Adjacent pre-existing bug, flagged and deliberately not fixed:** because of the `return false` at `plugin/mod.rs:1176-1178`, when no config file exists `reload_config` is never called, so the shipped engine-side multi-cursor defaults at `src/controller/mod.rs:707-722` (`<C-S-Down>`, `<leader>m*`) never load on a fresh project. The restructure preserves that behaviour for the engine plane to keep the diff honest. Someone should decide whether to fix it; the shell plane must not copy it.

Two smaller compatibility properties. A user who hand-writes `panelmap` lines and then opens the MappingDialog **before** the parser arm exists does not lose them: an unrecognized line is `ConfigLine::Other(raw_line)` (`parser.rs:99`) and the writer re-emits it verbatim (`writer.rs:58-61`). And the existing Presets/User tabs already display lines the runtime strips, with no badge, because `open_with_config` reads the raw file via `writer::read_file` (`mapping_dialog.rs:601`) and never calls `apply_vimrc_policy`. The new Panel Keys tab *does* call it, which makes it more honest than the two tabs beside it — either extend the badging to all tabs or accept a documented inconsistency.

### 12.3 The Godot version floor

`addons/godot_vim/godot_vim.gdextension:3` declares `compatibility_minimum = "4.5"`. `EditorSettings::get_shortcut` and `get_shortcut_list` were bound to ClassDB only by commit `8806036528`; `git tag --contains 8806036528` gives `4.6-stable` as the earliest containing tag, and `4.5-stable`, `4.5.1-stable`, `4.5.2-stable` all exist and lack it.

**This is a live defect today, not a new risk.** `src/bridge/godot_calls.rs:102-108` does an unguarded `settings.call("get_shortcut", &[path.to_variant()])`. gdext v0.4.5 generates vararg `call()` as `try_call(..).unwrap_or_else(|e| panic!("{e}"))`, and `Object::callp` sets `CALL_ERROR_INVALID_METHOD` for an unknown method — so on 4.5 the call **panics**; it does not return nil. `begin_delete` and `begin_rename` (`src/navigation/filesystem_explorer.rs:136-144`) route FS `d` and `r` through `trigger_dock_shortcut(SHORTCUT_FS_DELETE / SHORTCUT_FS_RENAME)`. Therefore, on Godot 4.5 today, **pressing `d` or `r` in the FileSystem dock aborts `handle_input_impl` inside `panic_guard("input", …)` with no recovery**, and `:action filesystem_dock/delete` panics through `handle_gui_input` into `recover_controller_from_panic()`. Two more equally live unguarded sites exist at `src/host/dispatch.rs:295` and `:498`.

**Ruling: keep `compatibility_minimum = "4.5"`. Add a cached `has_method` capability gate and switch every dynamic shortcut call from `call` to `try_call`.**

- Bumping to `"4.6"` would make the extension refuse to *load* on three shipped patch releases in order to protect one optional binding target. The plugin's core value — vim emulation — has zero 4.6 dependencies. Disproportionate.
- Dropping `<Shortcut>(path)` targets is not available: it is a binding graft from two of three judges.
- So: gate, plus a **declared** fallback that is genuinely good. `run_editor_shortcut` returns `Outcome::Declined` when the API is absent, so the key is **not consumed** and falls through to Godot's own handling — on the FileSystem dock, Godot's native Delete and F2 accelerators still work. Honest degradation, not a black hole.

```rust
// src/bridge/godot_calls.rs
thread_local! { static SHORTCUT_API: Cell<Option<bool>> = const { Cell::new(None) }; }

pub(crate) fn has_shortcut_api(settings: &mut Gd<EditorSettings>) -> bool {
    SHORTCUT_API.with(|c| c.get().unwrap_or_else(|| {
        let v = settings.has_method("get_shortcut");
        c.set(Some(v));
        if !v {
            // godot_warn!, NOT log::warn!: the default log level is "Off"
            // (src/settings/defaults.rs:14), so the facade would swallow this.
            godot_warn!(
                "GodotVim: EditorSettings.get_shortcut is unavailable on this Godot build \
                 (requires 4.6+). <Shortcut>(...) bindings are disabled and will decline; \
                 see :checkhealth godotvim."
            );
        }
        v
    }))
}
```

`try_call` is defence in depth beyond the gate: even with `has_method` true, a future signature change must never panic the input handler. At load time, `<Shortcut>(path)` rules are rejected with per-line warn-and-skip when the API is absent, and `:checkhealth godotvim` prints an explicit **"unavailable on Godot < 4.6"** line — never a silently omitted section, because a silently missing section teaches users to trust a check that did not run. This is a **prerequisite bugfix that lands in P0/P1**, before any characterization test runs on 4.5.

The same version gate covers `:checkhealth`'s conflict cross-reference against `EditorSettings.get_shortcut_list()`. Note that pinned gdext v0.4.5 generates **zero** shortcut methods on `EditorSettings`, which is why all of this goes through the dynamic-call shim rather than the typed API.

### 12.4 The vim-core version story

vim-core stays pinned at `tag = "v0.7.1"` (`Cargo.toml:19`). No fork, no v0.8.0, no cross-repo release coupling. **The Cargo.toml delta is zero** — no `ahash` (not a dependency at any tier; a `Vec<SurfaceBindings>` scanned linearly is faster at ~9-16 entries, needs no dependency, and gives the introspector a deterministic iteration order that a randomly-seeded hash map cannot) and no `smallvec` promotion (it is genuinely dev-only today, at `Cargo.toml:27`; every design site that wanted it builds at most ~20 tiny allocations per second at human key-repeat rates). If profiling ever justifies it, promoting `smallvec` is a one-line manifest change and a type-alias swap with no API impact.

**`.godot-vimrc` lives at `res://` and is committed, so the teammate-on-an-older-build case is the one that decides the version story.** A teammate who opens the same project with an older godot-vim gets a **silent no-op, in both planes**:

- **The plugin's own parser** has no `PanelMap` arm, so a `panelmap` line becomes `ConfigLine::Other(raw_line)` (`parser.rs:99`) — preserved verbatim and in order, re-emitted unchanged by the writer (`writer.rs:58-61`). Their dialog does not mangle it and their save does not drop it.
- **The pinned engine** drops it. The mechanism matters and the intuitive version of it is wrong: `parse_ex_command` does **not** error on an unknown command. `parse_named_command`'s terminal else returns `Ok(ExCommand::Custom { command })` (`vim-core/src/grammar/ex_parser.rs:818-821`), and `source_config_text` then discards it through the `_ => {}` catch-all at `source.rs:90` — *not* through the `Err`-continue at `:40-41`. Same effect, different mechanism, and the difference carries a security conclusion: because the line parses to `ExCommand::Custom` — which is precisely godot-vim's own custom-ex-command channel — its inertness depends on a vim-core catch-all that could change in any future tag (supporting host ex-commands in a sourced vimrc is a plausible feature). **The sandbox whitelist must therefore be the authoritative gate on its own, never justified by "the engine ignores it anyway."**
- **At the untrusted tier on an older build**, the line hits `sandbox_config_text`'s terminal `else` (`sandbox.rs:94-102`) and is rewritten to `" [sandbox] stripped: …` — in the sanitized *string* only. The file on disk is untouched, so nothing is lost when the teammate upgrades.

Contrast the rejected alternative: had the context been carried inside the mapping notation (`nnoremap <surface=dock> a <Action>(…)`), a teammate on the old tag would parse `<surface=dock>` as an **LHS** and install a junk mapping, silently, with no diagnostic. That failure has no fix, and it is the decisive reason the shell plane owns its own directive rather than extending `:map`.

**What is consumed from vim-core, all verified public on the checked-out tag**, and what would break on a bump:

- `MappingTrie` / `MappingEntry` / `MappingOwner` / `TrieLookup` / `NameRegistry` / `KeyEvent::action` / `Key::Action` / `Modifiers` / `LangmapTable` — all re-exported at `vim-core/src/keymap/mod.rs:36-51`, in a layer architecturally forbidden from importing `commands`, `grammar`, `effects` or `execution` (`keymap/mod.rs:17-20`). Using it drags in no engine.
- `MAX_KEY_SEQUENCE_LEN = 8` (`keymap/keymap.rs:140`, re-exported at `keymap/mod.rs:41-44`). The shell caps LHS at the same value deliberately.
- `TrieLookup` is `#[non_exhaustive]` (`keymap/trie.rs:264-265`), so `resolve.rs` must keep a `_ =>` arm and a bump cannot break compilation there — but a *new variant* would silently take the fall-through path. Worth an explicit `#[deny]`-adjacent review note on any bump.
- `vim_core::execution::parse_keys_from_string` — re-exported at `execution/mod.rs:112` but defined in `execution/engine/macro_replay.rs:325`. It is a re-export of an internal module and is the item most likely to move. Its behaviour on garbage is also the sharpest edge (§6.8): unknown `<…>` notation degrades to literal characters rather than failing, which is why `parse_lhs` validates before calling it.
- `vim_core::grammar::Parser` + `Keymap::new()` for `starts_vim_grammar_sequence` (`grammar/mod.rs:113-114`, `keymap/keymap.rs:296-300`). This is the one place the shell asks vim-core a *semantic* question, so it is pinned by a **vim-core-version canary test**: the probe must return `true` for `<C-w>` and `<C-\>` and `false` for `<C-h>/<C-j>/<C-k>/<C-l>`. If a bump flips any of those six, the guard's shape holds but its exclusion set needs re-deriving — and the canary is what turns that from a shipped `<C-w>s`-becomes-`s` regression into a red test.

### 12.5 The behaviour changes users will notice

Four, all improvements, all pre-registered as tests that are deliberately RED at P0 and turn GREEN at the cutover — because a refactor that fixes a bug it did not intend to fix is suspicious, and making the fix an explicit prior assertion is what distinguishes it from a regression:

1. **`/` works on a physical-J layout.** Today the hjkl block at `dock.rs:111-146` precedes the `Key::SLASH` arm at `:127`, so logical `/` is shadowed. The global probe order applies to the whole key, once, so it cannot recur.
2. **Numpad Enter works in docks.** `get_named_key` already maps `KP_ENTER → Key::Enter` (`src/bridge/input.rs:23`); routing through `parse_godot_key` picks it up for free.
3. **Ctrl+hjkl uses the physical fallback from the editor too.** Today it works from a dock but not from the editor, because the escape-hatch block at `input.rs:110-116` matches the *logical* keycode only and returns before `direction_from_hjkl`'s physical fallback at `:126` is reached. The follow-on correctness requirement is that the arbitration gate is evaluated on the **same `KeyEvent` that produced the winning candidate**, so `:nnoremap <C-h> x` still wins on a Cyrillic layout — which is why `resolve` returns the matched key alongside the candidate list.
4. **Holding Ctrl+J no longer fires a ~20/s storm of deferred `grab_focus` calls**, because `<norepeat>` drops echo events on the four focus rules. Held `j`/`k` in a dock still auto-repeats, because `Repeat` is per-binding rather than a global `is_echo` filter.

And one change that is a behaviour *removal* rather than a fix, stated here rather than discovered: opening the FS create prompt and then pressing Ctrl+L today jumps focus to another panel while the prompt stays visible with a stale `active_control`, so that whenever `dismiss_prompt` eventually runs it steals focus back to the old Tree. The stale-prompt auto-dismiss hook now runs before resolution, on modified keys too, so the orphan is dismissed and the focus-steal disappears. Today's ordering is a latent bug and is deliberately not preserved.

---

## 13. Known Limitations & Open Questions

### 13.1 What the remediation could not close

**The `Parser::process` grammar probe has never been executed.** The `starts_vim_grammar_sequence` guard — the mechanism that closes the `<C-w>` and `<C-\>` holes and replaces a rotting denylist — was derived by reading vim-core v0.7.1 source (`grammar/handlers/ready.rs:20-75`, `:141-150`, `:358-386`; `grammar/parser.rs:107-133`; `grammar/result.rs:60-62`), not by running it. The very first implementation task in P5 must be the unit test asserting the six expected verdicts. If any disagrees, the guard's *shape* holds but its exclusion set needs re-derivation.

**The grammar guard probes a default `Keymap`.** A `<Leader>` remapped to a Ctrl key, or a future `:set`-driven grammar variant not covered by the two `set_sneak_mode` passes, could in principle create a grammar prefix the registration guard does not see. Dispatch-time `could_start_mapping` catches every *user*-created prefix, so the residual is limited to future vim-core **options** that alter core grammar arity — pin it with a vim-core-version test and re-run it on every tag bump.

**Moving arbitration onto `SurfaceSpec` removes per-rule override.** `editor.nav` carries zero rules and exists only to gate. If a later phase wants an editor-surface rule that must win *despite* the engine claiming the key, there is no override left. This is judged correct — no such case exists today, and per-rule `Yield` is exactly what the judge panel fatally rejected — but it is a genuine expressiveness ceiling and belongs in `:checkhealth` output rather than being discovered.

**`Consumption::Void` is a hard key sink.** It is a verbatim transcription of `src/plugin/input.rs:124-135`, where `set_input_as_handled()` fires whether or not `handle_window_nav` succeeded and whether or not a focus owner exists. It therefore consumes even when `run` short-circuited on `target == None`. The paternalistic "`panelunmap` refuses on `Void` builtins" invariant is **deleted** (it contradicted the primary rebinding recipe); the replacement is a `:checkhealth` warning when no rule anywhere reaches `godotvim.focus.{left,right,up,down}`.

**`Caps` is a closed `bitflags` vocabulary.** A third-party provider needing a genuinely new affordance must edit `src/actions/caps.rs`, which breaks the "one new file plus one manifest line" claim *for that case*. Bits 5..15 are reserved and documented. The mitigation is that a provider can almost always express the constraint in its surface **probe** instead, self-restricting to the widget classes it handles — which is what the debugger example does.

**Every dock binding is already dead when a dock is floated, and this refactor neither fixes nor worsens it.** `_input` is registered per-viewport, and Godot reparents a floating dock's subtree into a separate `Window`. The `is_primary_viewport` fallback pattern generalizes to every dock, but P6 wires it only for the FS `prompt`; a floated FileSystem or Scene dock keeps today's behaviour (no `j`/`k`/`a`/`d`/`r`/`y`). That is a deliberate scope choice and must be stated in `:checkhealth` and in the manual matrix, or a user who floats the FileSystem dock will report `j`/`k` as broken.

**The `ActionCtx` borrow discipline is enforced by review, not by the compiler.** `ActionPlane` is a plain field on `GodotVimCore` rather than `Rc<RefCell<…>>` specifically so that no `RefCell` guard can span `(spec.run)(&mut cx)` — that class of runtime panic is designed out. What is left is an ordering rule with no compile-time check behind it: every shared handle must leave `self` in its own statement (`Rc::clone(&self.focus_chain)`, the `&'static ActionSpec` copied out of `registry.specs`, the owned `viewport`) *before* `ActionCtx { plugin: self, … }` is constructed. Get the order wrong and it is a borrow-check error at the one call site, which is the mitigation — but the reason re-entrancy is possible at all is real: the `input()` transport *is* re-entered by `Input::parse_input_event` injection on the `run_editor_shortcut` path, so `run_action_now` must stay the single place an `ActionSpec` runs.

**`Provenance::Builtin` degrades silently in release.** A shipped default that fails to parse is a `debug_assert!` plus `log::error!`, so a debug build fails loudly and a release build ships one binding short. That is the right trade for a `cdylib` the editor `dlopen`s — a release panic is worse than a missing binding — but it means the P5 golden test is the *only* thing standing between a rename and a silently dropped default. It must run in CI, not just locally.

**Three narrower residuals, stated so they are not discovered.** (a) The two-frame injection-suppression window can swallow a genuine user press of the same accelerator within ~33 ms of a delegated injection; judged negligible because the rename dialog grabs focus first, but it is a real keystroke loss. (b) `audit_shortcut_cycles` resolves shortcut event arrays at index-build time, so a user who rebinds an editor shortcut in Editor Settings *afterwards* can create a runtime cycle whose only backstop is the per-frame injection budget. (c) On Godot 4.5, `run_editor_shortcut` declines and the raw key reaches Godot; the reasoning is that `FileSystemDock`'s own Delete/F2 accelerators then fire, but this was **not** verified end-to-end and deserves one manual check before the 4.5 fallback is described as honest degradation.

**Behaviour removals users may notice.** None to cross-panel navigation: the planned exclusion of non-attached `CodeEdit`s was withdrawn (§1.3), so every `Ctrl+hjkl` target reachable today stays reachable. Separately, `godotvim.focus.cycle_next/prev` ship **unbound** by deliberate choice: no safe key was available (Ctrl+W and Ctrl+N are both taken by the editor), so discovery depends entirely on `:checkhealth` and a commented recipe in `.godot-vimrc.sample`.

**Two adjacent pre-existing bugs, flagged and not fixed.** `source_config_from_disk` returns early at `src/plugin/mod.rs:1176-1178` when the config file does not exist, so `reload_config` is never called and the shipped multi-cursor defaults at `src/controller/mod.rs:707-722` never load on a fresh project. This plan deliberately preserves that for the engine plane, to keep the P7 diff honest — and the shell plane must **not** copy it. And the MappingDialog reads the raw config via `writer::read_file` in `open_with_config` (`src/ui/mapping_dialog.rs:601`) and never calls `apply_vimrc_policy`, so the existing Presets and User tabs already display lines the runtime strips, unbadged. The new Panel Keys tab will be *more* honest than the two tabs beside it.

**Unverified assumption.** `SettingsSnapshot.timeoutlen` (`src/settings/snapshot.rs:150`) is specified as the shell timer's timeout source when detached. The field exists and feeds `VimOptions`, but it was not verified to be refreshed on every path that changes the underlying EditorSettings value. P8 must confirm the refresh path before relying on it.

**A convention doing a type's job.** P3 splits registry ids from EditorSettings shortcut paths with `name.contains('.') && !name.contains('/')`. That is deterministic for every name in `list_all_commands()` today and for every id in the registry, and both are asserted — but it is a naming convention, not a type. A future action id without a dot, or a Godot shortcut path without a slash, breaks it silently. The registration-time "every action id contains a dot" assertion is the guard; keep it.

### 13.2 Open questions for the maintainer

1. **Should `Consumption::Transparent` (act, but do not consume, so Godot's own handling also runs) be a first-class policy?** P9's completion popup needs exactly this and could express it by acting and returning `Declined` — but that overloads "declined" to mean two different things in `:panelmap` output: "I refused" versus "I acted and yielded". The introspector's clarity may be worth the extra variant. Decide before P9, not during it.
2. **Is the `DeadPrefix` divergence from Vim acceptable?** When a reserved prefix is followed by a non-matching key, this design consumes **both**, so the terminating key cannot leak into Tree incremental search. Vim would flush both as literals. Since `set_allow_search(false)` is applied to exactly those controls anyway, leaking is arguably harmless — in which case flushing both to Godot is strictly more Vim-faithful and should replace it. Either way the semantics must be stated in `:checkhealth` and visible in the reservation listing.
3. **Should the Editor Settings ▸ Shortcuts mirror ever be *written*?** This design says read-only forever, and that is defensible: the store is per-user, project-global, and 4.6+ only. But a one-way mirror of the four context-free `panel` bindings would give Godot users a familiar rebinding surface and free conflict visibility. The risk is two stores with `.godot-vimrc` authoritative — exactly the confusion that sank the godot-native camp. This decision deserves an explicit owner.
4. **Should config layering (user + project) ever be built?** Today `config::path::resolve` returns exactly one file, and because `res://` is tested before `user://` (`src/config/path.rs:38-59`), any project that ships a vimrc **silently and completely shadows the user's personal config**, with only a `log::debug!` as evidence. This design ships a `:checkhealth` diagnostic rather than a merge semantic, because layering forces the engine plane, `apply_vimrc_policy`'s single `is_project_level` bool, per-line provenance in `ConfigDocument`, and the whole-file-backed MappingDialog to change together — roughly a phase of its own. That is a product decision, not a technical one. If users report the shadowing as a bug rather than a surprise, revisit it and cost the layering phase.
5. **Should `compatibility_minimum` move to 4.6?** Keeping "4.5" preserves three shipped patch releases at the cost of `<Shortcut>(path)` targets degrading to a decline there. Bumping would simplify the gate and delete the `try_call` defence-in-depth, but it makes the extension refuse to load on 4.5 to protect one optional binding target. Current answer: keep 4.5. Revisit when 4.6 adoption is measurable.
6. **Should the floated-dock `is_primary_viewport` fallback be wired for *all* docks in P6, or left prompt-only?** The pattern generalizes; the cost is a second transport per dock and a second place for consumption to go wrong. Leaving it unwired preserves today's behaviour exactly, which is the conservative choice — but it is a known-broken case that now has an obvious fix, and shipping with it unfixed is a decision, not an omission.
7. **Should the `is_navigable_control` foreign-CodeEdit exclusion ship immediately, ahead of everything else?** It is a five-file mechanical edit that fixes a live one-way focus trap and is fully independent of this design. The argument for shipping it now is that it is a bug fix users benefit from today; the argument against is that it removes a navigation target and is therefore a behaviour change that wants a release note. Recommend: ship it separately, with the note.