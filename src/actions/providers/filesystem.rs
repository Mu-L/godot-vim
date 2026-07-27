//! `dock.filesystem` — the FileSystem dock's own Tree and file list.
//!
//! The surface that carries the nvim-tree keyset (`a` create, `d` delete,
//! `r` rename, `y` yank path, `R` refresh) and the one place in the design
//! where a *surface* grants a capability.
//!
//! # `FILEOPS` is dock membership, not a widget class
//!
//! `Caps::of_control` can never contribute [`Caps::FILEOPS`], because no
//! widget class implies it: the FileSystem dock's Tree and the Scene tree are
//! both a `Tree`. Granting it here is what stops
//! `panelmap dock a godotvim.fs.create` from creating files at `res://` root
//! from a focused Scene tree — `get_selected_path` returns `None` for a non-FS
//! Tree and `begin_create` falls back to `"res://"`
//! (`src/navigation/filesystem_explorer.rs:122-126`).
//!
//! # Why depth replaces the hardcoded FileSystem-first branch
//!
//! `src/plugin/input.rs:170-180` hardcodes "ask the FileSystem explorer first,
//! then fall back to generic dock input". Here that is just the forest:
//! `dock.filesystem` names `dock` as its parent, so it is deeper, so its
//! bindings are consulted first and `j` — which it does not bind — falls
//! through to `dock` by the ordinary upward walk. The branch disappears with
//! no replacement.
//!
//! # Why the probe is not `chain.in_filesystem_dock` alone
//!
//! Because a focused `Button` in the FileSystem dock would then anchor here
//! and be granted `FILEOPS`, and `a` would open the create prompt from a
//! toolbar button — behaviour today's `classify_focus` cannot produce, since
//! it answers `Unknown` for a Button and never reaches the FS explorer at all.
//! The probe is the conjunction: in the dock **and** on a widget the dock
//! surfaces know how to drive.
#![allow(
    dead_code,
    reason = "surfaces are registered by P5's `Registrar` and classified by P6's dispatcher"
)]

use crate::actions::caps::Caps;
use crate::actions::surface::{Anchor, Seal, SurfaceSpec};

use super::dock::focuses_nav_widget;
use super::Provider;

pub(crate) static DOCK_FILESYSTEM: SurfaceSpec = SurfaceSpec {
    id: "dock.filesystem",
    parent: Some("dock"),
    seal: Seal::Open,
    grants: |_| Caps::FILEOPS,
    probe: |chain| {
        (chain.in_filesystem_dock && focuses_nav_widget(chain)).then_some(Anchor::Node(0))
    },
    on_key: None,
    yields_to_engine: false,
};

/// The nvim-tree-flavoured file operations of `resolve_fs_action`
/// (`filesystem_explorer.rs:374-386`).
///
/// `R` and `r` are two *keys*, not one key with a modifier: `bridge::input`
/// folds Shift into the character itself, so the discriminant is carried by
/// the char. Writing them as `R` and `r` here is therefore both the shipped
/// behaviour and the only spelling that can match.
///
/// `dock.filesystem` is deeper in the forest than `dock`, so these five get
/// first refusal while `j`/`k` still fall through to the parent. That depth is
/// the entire replacement for the hardcoded branch at
/// `src/plugin/input.rs:140-150`.
const DEFAULTS: &str = "\
panelmap <physical> dock.filesystem a godotvim.fs.create
panelmap <physical> dock.filesystem d godotvim.fs.delete
panelmap <physical> dock.filesystem r godotvim.fs.rename
panelmap <physical> dock.filesystem y godotvim.fs.yank_path
panelmap <physical> dock.filesystem R godotvim.fs.refresh
";

pub(crate) const PROVIDER: Provider = Provider {
    tag: "godotvim.filesystem",
    surfaces: &[&DOCK_FILESYSTEM],
    defaults: DEFAULTS,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::surface::fixtures::*;
    use crate::actions::surface::{ChainNode, FocusChain};

    fn in_fs_dock(node: ChainNode) -> FocusChain {
        FocusChain {
            nodes: vec![
                node,
                plain("SplitContainer", 60),
                plain("FileSystemDock", 61),
            ],
            in_filesystem_dock: true,
            ..Default::default()
        }
    }

    #[test]
    fn the_filesystem_tree_and_list_both_anchor_here() {
        for node in [tree("FileSystemTree", 1), item_list("FileSystemList", 2)] {
            assert_eq!(
                (DOCK_FILESYSTEM.probe)(&in_fs_dock(node)),
                Some(Anchor::Node(0))
            );
        }
    }

    #[test]
    fn the_same_widget_outside_the_dock_does_not() {
        let elsewhere = FocusChain {
            nodes: vec![tree("SceneTreeEditor", 1), plain("SceneTreeDock", 62)],
            in_filesystem_dock: false,
            ..Default::default()
        };
        assert_eq!((DOCK_FILESYSTEM.probe)(&elsewhere), None);
    }

    #[test]
    fn a_button_inside_the_filesystem_dock_does_not_get_fileops() {
        // `in_filesystem_dock` alone would claim this, and `a` would open the
        // create prompt from a focused toolbar button.
        assert_eq!(
            (DOCK_FILESYSTEM.probe)(&in_fs_dock(plain("Button", 3))),
            None
        );
        assert_eq!((DOCK_FILESYSTEM.probe)(&in_fs_dock(line_edit(4))), None);
    }

    #[test]
    fn the_surface_grants_fileops_and_the_widget_never_does() {
        let chain = in_fs_dock(tree("FileSystemTree", 1));
        assert_eq!((DOCK_FILESYSTEM.grants)(&chain), Caps::FILEOPS);
        assert!(
            !chain.widget_caps().contains(Caps::FILEOPS),
            "FILEOPS is dock membership, not a widget affordance"
        );
    }

    #[test]
    fn it_is_declared_deeper_than_the_generic_dock() {
        // The whole FileSystem-first refusal, as one parent link.
        assert_eq!(DOCK_FILESYSTEM.parent, Some("dock"));
        assert_eq!(DOCK_FILESYSTEM.seal, Seal::Open);
    }
}
