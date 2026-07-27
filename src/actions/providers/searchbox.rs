//! `searchbox` — a dock's filter `LineEdit`.
//!
//! The discriminant is not the class and not the placeholder text: it is
//! whether the `LineEdit` has a *sibling navigable control* inside the same
//! dock boundary (`src/navigation/focus.rs:70-82`). A filter box always does,
//! because filtering is filtering *something*; a Project Settings field never
//! does. `FocusChain::sibling_nav_control` records the answer at sample time,
//! so the depth-8 climb over a depth-20 DFS runs once per focus change instead
//! of once per keystroke.
//!
//! `Seal::Sealed`, for the same reason as `prompt`: bare keys must reach the
//! control or the box cannot be typed in, while Ctrl+hjkl must still escape it.
#![allow(
    dead_code,
    reason = "surfaces are registered by P5's `Registrar` and classified by P6's dispatcher"
)]

use crate::actions::caps::Caps;
use crate::actions::surface::{Anchor, Seal, SurfaceSpec};

use super::Provider;

pub(crate) static SEARCHBOX: SurfaceSpec = SurfaceSpec {
    id: "searchbox",
    parent: Some("panel"),
    seal: Seal::Sealed,
    grants: |_| Caps::empty(),
    // `LineEdit` and not merely "accepts text": a `TextEdit` with a sibling
    // Tree is a foreign multi-line editor, not a filter box, and
    // `classify_focus` reaches this arm only for `LineEdit` (focus.rs:73).
    probe: |chain| {
        (chain.focus_is("LineEdit") && chain.sibling_nav_control.is_some())
            .then_some(Anchor::Node(0))
    },
    on_key: None,
    yields_to_engine: false,
};

pub(crate) const PROVIDER: Provider = Provider {
    tag: "godotvim.searchbox",
    surfaces: &[&SEARCHBOX],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::surface::fixtures::*;
    use crate::actions::surface::FocusChain;

    fn filter_box(sibling: Option<i64>) -> FocusChain {
        FocusChain {
            nodes: vec![
                line_edit(50),
                plain("VBoxContainer", 51),
                plain("SceneTreeDock", 52),
            ],
            sibling_nav_control: sibling.map(id),
            ..Default::default()
        }
    }

    #[test]
    fn a_line_edit_with_a_sibling_nav_control_is_a_filter_box() {
        assert_eq!(
            (SEARCHBOX.probe)(&filter_box(Some(53))),
            Some(Anchor::Node(0))
        );
    }

    #[test]
    fn a_line_edit_with_no_sibling_is_not_ours() {
        // The Project Settings field. It falls through to `foreign`, where the
        // Barrier keeps Ctrl+hjkl out of the user's typing.
        assert_eq!((SEARCHBOX.probe)(&filter_box(None)), None);
    }

    #[test]
    fn a_text_edit_is_never_a_filter_box() {
        // Both are TEXTENTRY, so a capability test could not tell them apart.
        let multi_line = FocusChain {
            nodes: vec![text_edit(54)],
            sibling_nav_control: Some(id(55)),
            ..Default::default()
        };
        assert_eq!((SEARCHBOX.probe)(&multi_line), None);
    }

    #[test]
    fn a_focused_tree_is_not_a_filter_box_even_beside_one() {
        let tree_focused = FocusChain {
            nodes: vec![tree("SceneTreeEditor", 56)],
            sibling_nav_control: Some(id(57)),
            ..Default::default()
        };
        assert_eq!((SEARCHBOX.probe)(&tree_focused), None);
    }
}
