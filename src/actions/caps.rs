//! What a focused control can *do*, as opposed to what class it is.
//!
//! The dispatcher used to gate behaviour on widget identity — `h`/`l` worked
//! only when `matches!(dock_kind, DockKind::Tree)`. That reads fine for three
//! widget kinds and becomes a maintenance tax at ten: every new dock means new
//! match arms in code that has no business knowing about docks.
//!
//! Capabilities invert it. A control contributes the set of affordances it
//! offers; an action declares what it needs; the resolver skips candidates
//! whose needs are not met. Adding a dock means declaring its capabilities,
//! and nothing in the dispatcher changes.
//!
//! # Why the vocabulary is what it is
//!
//! The bits are chosen so that today's behaviour is expressible *exactly*.
//! [`VNAV`](Caps::VNAV) is the sharp case: `j`/`k` currently work on `Tree`,
//! `ItemList` **and** `RichTextLabel` — `handle_navigation` has no widget gate
//! at all, and on a `RichTextLabel` it scrolls 50px and reports `Handled`
//! (`src/navigation/dock_nav.rs`). A capability named "has a selectable list"
//! would silently drop the built-in docs panel and the Output log, both of
//! which are focusable `RichTextLabel`s. So the bit is named for the
//! affordance — *can move a vertical cursor* — not for the data structure.

use bitflags::bitflags;

bitflags! {
    /// Affordances a focused control offers, or an action requires.
    ///
    /// A control's set is contributed by its class; an action's set is
    /// declared on its `ActionSpec`. See [`Caps::satisfies`] for the gate.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub(crate) struct Caps: u8 {
        /// Move a vertical cursor: next/previous item, or scroll.
        ///
        /// Held by `Tree`, `ItemList` **and** `RichTextLabel` — see the module
        /// docs for why this is not "has a list".
        const VNAV = 1 << 0;
        /// Expand and collapse nested items. `Tree` only.
        const HIERARCHY = 1 << 1;
        /// Open or activate the current item (`Enter`).
        const ACTIVATE = 1 << 2;
        /// Accept typed text — a filter box or a prompt. Bare keys must reach
        /// the control rather than being claimed as commands.
        const TEXTENTRY = 1 << 3;
        /// Create / delete / rename / yank paths. The FileSystem dock.
        const FILEOPS = 1 << 4;
    }
}

impl Caps {
    /// Whether a control offering `self` can host an action requiring `needs`.
    ///
    /// Subset test: every required bit must be present. An action requiring
    /// nothing runs anywhere, which is what makes cross-panel focus movement
    /// work with no focus owner at all.
    pub(crate) const fn satisfies(self, needs: Self) -> bool {
        self.contains(needs)
    }

    /// Capabilities of a Godot control, by class name.
    ///
    /// String-based because Godot's hierarchy is only known at runtime, and
    /// walking it is what catches subclasses — `FileSystemTree` is a `Tree`,
    /// `FileSystemList` is an `ItemList`. Callers pass an `is_class` probe so
    /// this stays testable without a Godot runtime.
    pub(crate) fn of_control(is_class: impl Fn(&str) -> bool) -> Self {
        let mut caps = Self::empty();
        if is_class("Tree") {
            caps |= Self::VNAV | Self::HIERARCHY | Self::ACTIVATE;
        } else if is_class("ItemList") {
            caps |= Self::VNAV | Self::ACTIVATE;
        } else if is_class("RichTextLabel") {
            // Scrolls rather than selecting, but j/k work and are expected to.
            caps |= Self::VNAV;
        }
        if is_class("LineEdit") || is_class("TextEdit") {
            caps |= Self::TEXTENTRY;
        }
        caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in for Godot's `Node::is_class`, which walks the inheritance
    /// chain. `("FileSystemTree", &["Tree"])` models a subclass.
    fn classes(names: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |q| names.contains(&q)
    }

    #[test]
    fn a_tree_offers_navigation_hierarchy_and_activation() {
        let caps = Caps::of_control(classes(&["Tree"]));
        assert_eq!(caps, Caps::VNAV | Caps::HIERARCHY | Caps::ACTIVATE);
    }

    #[test]
    fn an_item_list_offers_no_hierarchy() {
        // This is what makes `h`/`l` inert on an ItemList — a capability miss,
        // not a `DockKind::Tree` match arm in the dispatcher.
        let caps = Caps::of_control(classes(&["ItemList"]));
        assert_eq!(caps, Caps::VNAV | Caps::ACTIVATE);
        assert!(!caps.satisfies(Caps::HIERARCHY));
    }

    #[test]
    fn a_rich_text_label_still_offers_vertical_navigation() {
        // The regression this vocabulary exists to prevent: j/k scroll the
        // built-in docs panel and the Output log today. A bit meaning "has a
        // selectable list" would have dropped both.
        let caps = Caps::of_control(classes(&["RichTextLabel"]));
        assert!(caps.satisfies(Caps::VNAV));
        assert!(!caps.satisfies(Caps::ACTIVATE));
        assert!(!caps.satisfies(Caps::HIERARCHY));
    }

    #[test]
    fn all_three_dock_widgets_share_vnav() {
        // `handle_navigation` has no widget gate; j/k work on all three.
        for names in [
            &["Tree"] as &[&str],
            &["ItemList"] as &[&str],
            &["RichTextLabel"] as &[&str],
        ] {
            let caps = Caps::of_control(|q| names.contains(&q));
            assert!(caps.satisfies(Caps::VNAV), "{names:?} must keep j/k");
        }
    }

    #[test]
    fn subclasses_inherit_through_the_is_class_probe() {
        // Godot's is_class walks the chain, so FileSystemTree answers "Tree".
        let caps = Caps::of_control(classes(&["FileSystemTree", "Tree", "Control"]));
        assert!(caps.satisfies(Caps::HIERARCHY));
    }

    #[test]
    fn text_inputs_are_text_entry() {
        assert!(Caps::of_control(classes(&["LineEdit"])).satisfies(Caps::TEXTENTRY));
        assert!(Caps::of_control(classes(&["TextEdit"])).satisfies(Caps::TEXTENTRY));
        assert!(!Caps::of_control(classes(&["Tree"])).satisfies(Caps::TEXTENTRY));
    }

    #[test]
    fn an_unknown_control_offers_nothing() {
        assert_eq!(Caps::of_control(classes(&["GraphEdit"])), Caps::empty());
    }

    #[test]
    fn requiring_nothing_is_satisfied_by_anything() {
        // Cross-panel focus movement requires no capability, which is how it
        // still works when there is no focus owner at all.
        assert!(Caps::empty().satisfies(Caps::empty()));
        assert!(Caps::VNAV.satisfies(Caps::empty()));
    }

    #[test]
    fn satisfies_needs_every_required_bit() {
        let tree = Caps::VNAV | Caps::HIERARCHY | Caps::ACTIVATE;
        assert!(tree.satisfies(Caps::VNAV | Caps::HIERARCHY));
        assert!(!tree.satisfies(Caps::VNAV | Caps::FILEOPS));
    }

    // Compile-time proof that `satisfies` stays const-evaluable, so a provider
    // can assert its own capability invariants without a runtime test.
    const _: () = assert!(Caps::VNAV.union(Caps::ACTIVATE).satisfies(Caps::ACTIVATE));
    const _: () = assert!(!Caps::VNAV.satisfies(Caps::HIERARCHY));
}
