//! Cross-panel navigation (`Ctrl+hjkl`).
//!
//! Maps Vim's `Ctrl-W h/j/k/l` window-movement commands to Godot's flat
//! dock/editor layout. Unlike Vim's window grid, Godot panels are
//! arbitrarily positioned, so we use a spatial cone + distance scoring
//! algorithm (~63-degree half-angle) to pick the nearest candidate in the
//! desired direction.

use godot::classes::{Control, EditorInterface, Node};
use godot::global::Key;
use godot::prelude::*;

use crate::bridge::godot_calls;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowNavDirection {
    Down,
    Up,
    Left,
    Right,
}

/// Try logical keycode first (respects key remapping), fall back to physical
/// keycode (layout-independent, US-QWERTY positions). This ensures Ctrl+hjkl
/// panel navigation works on non-Latin layouts (Russian, Greek, etc.) where
/// `get_keycode()` may not return the Latin H/J/K/L equivalents.
pub(crate) fn direction_from_hjkl(logical: Key, physical: Key) -> Option<WindowNavDirection> {
    hjkl_direction(logical).or_else(|| hjkl_direction(physical))
}

fn hjkl_direction(key: Key) -> Option<WindowNavDirection> {
    match key {
        Key::J => Some(WindowNavDirection::Down),
        Key::K => Some(WindowNavDirection::Up),
        Key::H => Some(WindowNavDirection::Left),
        Key::L => Some(WindowNavDirection::Right),
        _ => None,
    }
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

    // ── The Ctrl+hjkl direction table ────────────────────────────────

    #[test]
    fn hjkl_maps_to_vim_directions() {
        assert_eq!(hjkl_direction(Key::H), Some(WindowNavDirection::Left));
        assert_eq!(hjkl_direction(Key::J), Some(WindowNavDirection::Down));
        assert_eq!(hjkl_direction(Key::K), Some(WindowNavDirection::Up));
        assert_eq!(hjkl_direction(Key::L), Some(WindowNavDirection::Right));
    }

    #[test]
    fn non_hjkl_keys_are_not_directions() {
        for key in [Key::A, Key::Z, Key::ENTER, Key::ESCAPE, Key::SLASH, Key::F2] {
            assert_eq!(hjkl_direction(key), None, "{key:?} should not navigate");
        }
    }

    #[test]
    fn logical_keycode_wins_when_it_is_hjkl() {
        // Latin layout: logical and physical agree.
        assert_eq!(
            direction_from_hjkl(Key::J, Key::J),
            Some(WindowNavDirection::Down)
        );
        // Logical is authoritative even when physical would mean something
        // else — this is what makes a user's OS-level remap take effect.
        assert_eq!(
            direction_from_hjkl(Key::J, Key::A),
            Some(WindowNavDirection::Down)
        );
    }

    #[test]
    fn physical_keycode_is_the_fallback_for_non_latin_layouts() {
        // Cyrillic: the QWERTY-J position emits logical Key::O (Cyrillic о).
        // Without the physical fallback, Ctrl+hjkl would be dead on this
        // layout — this is the whole reason the fallback exists.
        assert_eq!(
            direction_from_hjkl(Key::O, Key::J),
            Some(WindowNavDirection::Down)
        );
    }

    #[test]
    fn neither_logical_nor_physical_hjkl_yields_nothing() {
        assert_eq!(direction_from_hjkl(Key::A, Key::B), None);
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
