//! Cross-panel navigation (`Ctrl+hjkl`).
//!
//! Maps Vim's `Ctrl-W h/j/k/l` window-movement commands to Godot's flat
//! dock/editor layout. Unlike Vim's window grid, Godot panels are
//! arbitrarily positioned, so we use a spatial cone + distance scoring
//! algorithm (~63-degree half-angle) to pick the nearest candidate in the
//! desired direction.

use godot::classes::{Control, EditorInterface, Node};
use godot::prelude::*;
use vim_core::keymap::{Key as VimKey, KeyEvent, Modifiers};

use crate::actions::keys::Probes;

use crate::bridge::godot_calls;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowNavDirection {
    Down,
    Up,
    Left,
    Right,
}

/// The cross-panel direction bound to a single key interpretation.
///
/// Requires Ctrl and nothing else: `Ctrl+hjkl` is the panel chord, while
/// bare `hjkl` belongs to whichever dock has focus.
#[allow(
    dead_code,
    reason = "the live table is the binding index; this is P0's characterization oracle"
)]
fn direction_for(key: KeyEvent) -> Option<WindowNavDirection> {
    if key.modifiers() != Modifiers::CTRL {
        return None;
    }
    match key.key() {
        VimKey::Char('j') => Some(WindowNavDirection::Down),
        VimKey::Char('k') => Some(WindowNavDirection::Up),
        VimKey::Char('h') => Some(WindowNavDirection::Left),
        VimKey::Char('l') => Some(WindowNavDirection::Right),
        _ => None,
    }
}

/// Resolve a keystroke to a cross-panel direction and the interpretation that
/// matched.
///
/// Each probe is tried against the whole hjkl set before the next probe is
/// tried against any of it, so a lower-priority interpretation can never
/// outrank what the user actually typed. See `crate::actions::keys`.
///
/// The matched `KeyEvent` is returned because the dispatcher must ask the
/// engine about *that* key — not the raw logical keycode — when deciding
/// whether a user mapping claims it. Asking about the wrong one is what used
/// to deny Cyrillic users panel navigation from inside the editor.
#[allow(
    dead_code,
    reason = "the live table is the binding index; this is P0's characterization oracle"
)]
pub(crate) fn resolve_panel_key(probes: &Probes) -> Option<(KeyEvent, WindowNavDirection)> {
    probes.resolve(|k| direction_for(k).map(|dir| (k, dir)))
}

#[derive(Debug)]
pub(crate) enum WindowNavResult {
    Ignored,
    /// A target was found; `grab_focus()` has been deferred to it.
    Focused,
}

/// Whether a candidate displaces the current best.
///
/// Direction gates proximity: an out-of-cone candidate never wins, however
/// close it is. `dist < best` is **strict**, so on an exact tie the earlier
/// candidate in traversal order keeps the win — and traversal order is
/// `collect_descendants`' DFS order, not anything spatial.
fn beats_incumbent(in_cone: bool, dist: f32, best: f32) -> bool {
    in_cone && dist < best
}

/// Whether `diff` (candidate centre minus current centre) lies inside the
/// directional cone.
///
/// The secondary axis may be up to 2x the primary, i.e. a half-angle of
/// `atan(2)` ~= 63.4 deg. Deliberately wider than 45 deg so panels slightly
/// off-axis (a dock whose centre is diagonal from the editor) stay reachable.
/// Exactly on the boundary is OUT: the comparison is strict.
fn in_cone(diff: Vector2, direction: WindowNavDirection) -> bool {
    match direction {
        WindowNavDirection::Down => diff.y > 0.0 && diff.y.abs() > diff.x.abs() * 0.5,
        WindowNavDirection::Up => diff.y < 0.0 && diff.y.abs() > diff.x.abs() * 0.5,
        WindowNavDirection::Left => diff.x < 0.0 && diff.x.abs() > diff.y.abs() * 0.5,
        WindowNavDirection::Right => diff.x > 0.0 && diff.x.abs() > diff.y.abs() * 0.5,
    }
}

pub(crate) fn handle_window_nav(
    current: &Gd<Control>,
    direction: WindowNavDirection,
) -> WindowNavResult {
    let interface = EditorInterface::singleton();
    let Some(base) = interface.get_base_control() else {
        return WindowNavResult::Ignored;
    };

    let current_rect = current.get_global_rect();
    let current_center = current_rect.center();

    let candidates = find_window_candidates(&base);

    let mut best_candidate: Option<Gd<Control>> = None;
    let mut min_score = f32::MAX;

    log::debug!(
        "window_nav: direction={:?} current_center=({:.0},{:.0}) candidates={}",
        direction,
        current_center.x,
        current_center.y,
        candidates.len()
    );

    for candidate in candidates {
        if candidate.instance_id() == current.instance_id() {
            continue;
        }

        let cand_rect = candidate.get_global_rect();
        let cand_center = cand_rect.center();
        let diff = cand_center - current_center;

        let in_cone = in_cone(diff, direction);

        let class = candidate.clone().upcast::<Node>().get_class().to_string();
        let dist = current_center.distance_squared_to(cand_center);
        log::trace!(
            "  candidate: {} center=({:.0},{:.0}) diff=({:.0},{:.0}) in_cone={} dist={:.0}",
            class,
            cand_center.x,
            cand_center.y,
            diff.x,
            diff.y,
            in_cone,
            dist
        );

        if beats_incumbent(in_cone, dist, min_score) {
            min_score = dist;
            best_candidate = Some(candidate);
        }
    }

    if let Some(target) = best_candidate {
        log::debug!(
            "window_nav: {:?} -> focused #{}",
            direction,
            target.instance_id().to_i64()
        );
        target
            .clone()
            .upcast::<Node>()
            .call_deferred("grab_focus", &[]);
        WindowNavResult::Focused
    } else {
        log::debug!("window_nav: {:?} -> no target", direction);
        WindowNavResult::Ignored
    }
}

const MAX_DISCOVERY_DEPTH: u32 = crate::scene_tree::MAX_DISCOVERY_DEPTH;

/// Excludes tiny focusable controls (buttons, checkboxes, toolbars) that
/// would be confusing cross-panel navigation targets.
const MIN_WINDOW_CANDIDATE_SIZE: f32 = 50.0;

fn find_window_candidates(root: &Gd<Control>) -> Vec<Gd<Control>> {
    let mut candidates = Vec::new();
    crate::scene_tree::collect_descendants(
        &root.clone().upcast::<Node>(),
        MAX_DISCOVERY_DEPTH,
        &mut candidates,
        &|node| {
            let control = node.clone().try_cast::<Control>().ok()?;
            if !control.is_visible_in_tree() {
                return None;
            }
            is_window_candidate(&control).then_some(control)
        },
    );
    candidates
}

fn is_window_candidate(control: &Gd<Control>) -> bool {
    if !control.is_visible_in_tree() {
        return false;
    }

    if control.get_focus_mode() == godot::classes::control::FocusMode::NONE {
        return false;
    }

    let size = control.get_size();
    if size.x < MIN_WINDOW_CANDIDATE_SIZE || size.y < MIN_WINDOW_CANDIDATE_SIZE {
        return false;
    }

    // Uses is_class() to walk the inheritance chain (catches FileSystemTree,
    // FileSystemList, etc.). TextEdit is intentionally excluded because
    // classify_focus() treats non-CodeEdit TextEdits as Foreign, which would
    // block Ctrl+hjkl FROM that control — creating a one-way navigation trap.
    let node = control.clone().upcast::<Node>();
    let is_known_type = crate::scene_tree::is_navigable_control(&node);

    if !is_known_type {
        return false;
    }

    // Walk ancestors (up to 6 levels) looking for a known editor container.
    // Godot wraps dock contents in variable-depth layout containers
    // (MarginContainer/VBoxContainer/SplitContainer), so parent-only checks
    // miss deeply nested controls like FileSystemDock's Tree.
    let mut ancestor = control.get_parent();
    for _ in 0..6 {
        let Some(node) = ancestor else { break };
        let class_name = node.get_class().to_string();

        // COMPAT: Internal editor classes, not part of public Godot API.
        if node.is_class(godot_calls::CLASS_CODE_TEXT_EDITOR)
            || node.is_class(godot_calls::CLASS_SHADER_TEXT_EDITOR)
            || node.is_class(godot_calls::CLASS_SCENE_TREE_EDITOR)
            || node.is_class(godot_calls::CLASS_EDITOR_HELP)
        {
            return true;
        }

        // COMPAT: Heuristic — substring match on dynamic class name to catch
        // all editor docks without a hardcoded allowlist.
        if class_name.contains("Dock") {
            return true;
        }

        ancestor = node.get_parent();
    }

    // No recognized ancestor within 6 levels — accept anyway. The type
    // allowlist + size/focus/visibility filters already exclude most false
    // positives. Rejecting would miss legitimate panels like the Scripts List
    // (nested under ScriptEditor → HSplitContainer → VBoxContainer, none of
    // which contain "Dock" or match known editor container classes).
    true
}

// ─── Characterization tests (P0) ─────────────────────────────────────────
//
// These pin CURRENT behaviour of cross-panel navigation's pure decision
// layer. They must survive the dispatcher cutover UNMODIFIED — that is the
// acceptance gate for the rebindable-nav work. If a later phase needs to
// change one of these, the behaviour change is intentional and must be
// argued in the commit, not absorbed by editing the test.
#[cfg(test)]
mod characterization {
    use super::*;
    use crate::actions::keys::Probes;
    use vim_core::keymap::Key as VK;

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(VK::Char(c), Modifiers::CTRL)
    }
    fn probes(keys: &[KeyEvent]) -> Probes {
        Probes::from_slice(keys)
    }
    fn direction_of(p: &Probes) -> Option<WindowNavDirection> {
        resolve_panel_key(p).map(|(_, dir)| dir)
    }

    // ── The Ctrl+hjkl direction table ────────────────────────────────

    #[test]
    fn hjkl_maps_to_vim_directions() {
        assert_eq!(direction_for(ctrl('h')), Some(WindowNavDirection::Left));
        assert_eq!(direction_for(ctrl('j')), Some(WindowNavDirection::Down));
        assert_eq!(direction_for(ctrl('k')), Some(WindowNavDirection::Up));
        assert_eq!(direction_for(ctrl('l')), Some(WindowNavDirection::Right));
    }

    #[test]
    fn non_hjkl_keys_are_not_directions() {
        for c in ['a', 'z', '/', '1'] {
            assert_eq!(direction_for(ctrl(c)), None, "{c} should not navigate");
        }
        assert_eq!(
            direction_for(KeyEvent::new(VK::Enter, Modifiers::CTRL)),
            None
        );
    }

    #[test]
    fn panel_navigation_requires_ctrl_and_nothing_else() {
        // Bare hjkl belongs to whichever dock has focus; Ctrl+Shift and
        // Ctrl+Alt are different chords entirely.
        assert_eq!(
            direction_for(KeyEvent::new(VK::Char('j'), Modifiers::NONE)),
            None
        );
        for extra in [Modifiers::SHIFT, Modifiers::ALT, Modifiers::META] {
            assert_eq!(
                direction_for(KeyEvent::new(VK::Char('j'), Modifiers::CTRL | extra)),
                None
            );
        }
    }

    #[test]
    fn the_as_typed_probe_wins_when_it_is_hjkl() {
        assert_eq!(
            direction_of(&probes(&[ctrl('j')])),
            Some(WindowNavDirection::Down)
        );
        // Probe 1 is authoritative even when a later probe would mean
        // something else — that is what makes an OS-level remap take effect.
        assert_eq!(
            direction_of(&probes(&[ctrl('j'), ctrl('l')])),
            Some(WindowNavDirection::Down)
        );
    }

    #[test]
    fn a_later_probe_is_the_fallback_for_non_latin_layouts() {
        // Cyrillic: probe 1 is the Cyrillic char, probe 2/3 recover `j`.
        // Without the fallback, Ctrl+hjkl would be dead on this layout.
        assert_eq!(
            direction_of(&probes(&[ctrl('о'), ctrl('j')])),
            Some(WindowNavDirection::Down)
        );
    }

    #[test]
    fn no_probe_matching_hjkl_yields_nothing() {
        assert_eq!(direction_of(&probes(&[ctrl('a'), ctrl('b')])), None);
        assert_eq!(direction_of(&Probes::default()), None);
    }

    #[test]
    fn resolve_panel_key_reports_the_probe_that_matched() {
        // The dispatcher asks the engine about THIS key, not the raw logical
        // keycode — asking about the wrong one denied Cyrillic users panel
        // navigation from inside the editor.
        let (matched, dir) = resolve_panel_key(&probes(&[ctrl('о'), ctrl('j')])).unwrap();
        assert_eq!(matched, ctrl('j'));
        assert_eq!(dir, WindowNavDirection::Down);
    }

    // ── The spatial cone ─────────────────────────────────────────────

    #[test]
    fn straight_ahead_is_always_in_cone() {
        assert!(in_cone(Vector2::new(0.0, 100.0), WindowNavDirection::Down));
        assert!(in_cone(Vector2::new(0.0, -100.0), WindowNavDirection::Up));
        assert!(in_cone(Vector2::new(-100.0, 0.0), WindowNavDirection::Left));
        assert!(in_cone(Vector2::new(100.0, 0.0), WindowNavDirection::Right));
    }

    #[test]
    fn opposite_direction_is_never_in_cone() {
        assert!(!in_cone(
            Vector2::new(0.0, -100.0),
            WindowNavDirection::Down
        ));
        assert!(!in_cone(Vector2::new(0.0, 100.0), WindowNavDirection::Up));
        assert!(!in_cone(Vector2::new(100.0, 0.0), WindowNavDirection::Left));
        assert!(!in_cone(
            Vector2::new(-100.0, 0.0),
            WindowNavDirection::Right
        ));
    }

    #[test]
    fn cone_half_angle_is_atan2_and_the_boundary_is_exclusive() {
        // Secondary axis may be up to 2x the primary. At exactly 2x the
        // comparison |y| > |x|*0.5 is false, so the boundary is OUT.
        assert!(in_cone(
            Vector2::new(199.0, 100.0),
            WindowNavDirection::Down
        ));
        assert!(!in_cone(
            Vector2::new(200.0, 100.0),
            WindowNavDirection::Down
        ));
        assert!(!in_cone(
            Vector2::new(201.0, 100.0),
            WindowNavDirection::Down
        ));
    }

    #[test]
    fn a_diagonal_panel_is_reachable_on_both_axes() {
        // The deliberate consequence of a >45 deg cone: a panel down-and-right
        // is reachable by BOTH Ctrl+j and Ctrl+l. Overlapping cones are the
        // design's intent, not a defect.
        let diag = Vector2::new(100.0, 100.0);
        assert!(in_cone(diag, WindowNavDirection::Down));
        assert!(in_cone(diag, WindowNavDirection::Right));
        assert!(!in_cone(diag, WindowNavDirection::Up));
        assert!(!in_cone(diag, WindowNavDirection::Left));
    }

    #[test]
    fn exactly_coincident_centres_are_in_no_cone() {
        // Guards the strict-inequality choice: a candidate perfectly on top of
        // the current control must never be selected in any direction.
        for d in [
            WindowNavDirection::Down,
            WindowNavDirection::Up,
            WindowNavDirection::Left,
            WindowNavDirection::Right,
        ] {
            assert!(!in_cone(Vector2::ZERO, d), "{d:?} accepted a zero diff");
        }
    }

    // ── Candidate scoring ────────────────────────────────────────────

    /// Run the real selection fold over candidate centres, mirroring
    /// `handle_window_nav`'s loop, and return the winning index.
    fn select(current: Vector2, centres: &[Vector2], dir: WindowNavDirection) -> Option<usize> {
        let mut best = None;
        let mut min_score = f32::MAX;
        for (i, &c) in centres.iter().enumerate() {
            let dist = current.distance_squared_to(c);
            if beats_incumbent(in_cone(c - current, dir), dist, min_score) {
                min_score = dist;
                best = Some(i);
            }
        }
        best
    }

    #[test]
    fn nearest_in_cone_candidate_wins() {
        let far = Vector2::new(10.0, 500.0);
        let near = Vector2::new(10.0, 50.0);
        // Nearest wins regardless of the order they are encountered.
        assert_eq!(
            select(Vector2::ZERO, &[far, near], WindowNavDirection::Down),
            Some(1)
        );
        assert_eq!(
            select(Vector2::ZERO, &[near, far], WindowNavDirection::Down),
            Some(0)
        );
    }

    #[test]
    fn a_much_nearer_out_of_cone_candidate_loses_to_a_far_in_cone_one() {
        // Direction gates proximity. `sideways` is ~30x closer and still loses.
        let sideways = Vector2::new(30.0, 1.0);
        let straight_down = Vector2::new(0.0, 900.0);
        assert_eq!(
            select(
                Vector2::ZERO,
                &[sideways, straight_down],
                WindowNavDirection::Down
            ),
            Some(1)
        );
    }

    #[test]
    fn no_in_cone_candidate_means_no_target() {
        let behind = Vector2::new(0.0, -100.0);
        assert_eq!(
            select(Vector2::ZERO, &[behind], WindowNavDirection::Down),
            None
        );
        assert_eq!(select(Vector2::ZERO, &[], WindowNavDirection::Down), None);
    }

    #[test]
    fn an_exact_distance_tie_is_broken_by_traversal_order() {
        // `dist < best` is strict, so the FIRST equidistant candidate wins.
        // Traversal order is collect_descendants' DFS order — not spatial —
        // so a resolver rewrite that reorders candidates silently changes
        // which panel gets focus. Pinned deliberately.
        let left = Vector2::new(-40.0, 100.0);
        let right = Vector2::new(40.0, 100.0);
        assert_eq!(
            Vector2::ZERO.distance_squared_to(left),
            Vector2::ZERO.distance_squared_to(right)
        );
        assert_eq!(
            select(Vector2::ZERO, &[left, right], WindowNavDirection::Down),
            Some(0)
        );
        assert_eq!(
            select(Vector2::ZERO, &[right, left], WindowNavDirection::Down),
            Some(0)
        );
    }
}
