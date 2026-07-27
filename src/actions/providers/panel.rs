//! `panel` — the forest root, and the only surface that never probes.
//!
//! Everything reachable by Ctrl+hjkl lives under `panel`: it is the declared
//! parent of `dock`, `searchbox`, `prompt`, `editor.nav` and `unknown`. It is
//! reached **only** by following parent links, never by classification, which
//! is why its probe is `|_| None` and why it sits last in `PROVIDERS` where a
//! total probe could never shadow it.
//!
//! It grants nothing. An earlier draft gave it a `PANEL` capability; that was a
//! tautology — the root is on every non-`Barrier` path, so `requires: PANEL`
//! could never fail — and the `godotvim.focus.*` actions carry
//! `Caps::empty()` instead. Requiring nothing is exactly what lets cross-panel
//! movement still fire when there is no focus owner at all.
#![allow(
    dead_code,
    reason = "surfaces are registered by P5's `Registrar` and classified by P6's dispatcher"
)]

use crate::actions::caps::Caps;
use crate::actions::surface::{Seal, SurfaceSpec};

use super::Provider;

pub(crate) static PANEL: SurfaceSpec = SurfaceSpec {
    id: "panel",
    parent: None,
    seal: Seal::Open,
    grants: |_| Caps::empty(),
    // Never classified directly. A surface with no probe is not a surface with
    // no bindings: the panel keyset (Ctrl+hjkl, Ctrl+w cycling, Esc back to
    // the editor) all lives here, and reaches every descendant through the
    // upward walk.
    probe: |_| None,
    on_key: None,
    yields_to_engine: false,
};

pub(crate) const PROVIDER: Provider = Provider {
    tag: "godotvim.panel",
    surfaces: &[&PANEL],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::surface::fixtures::*;
    use crate::actions::surface::FocusChain;

    #[test]
    fn the_root_never_claims_anything() {
        // Including the empty chain. If `panel` ever probed, it would have to
        // sit ahead of `unknown` to be reachable, and the two would race.
        for chain in [
            no_focus_owner(),
            FocusChain {
                nodes: vec![tree("Tree", 1)],
                ..Default::default()
            },
            FocusChain {
                nodes: vec![code_edit(1)],
                attached_editor: Some(id(1)),
                ..Default::default()
            },
        ] {
            assert!((PANEL.probe)(&chain).is_none());
        }
    }

    #[test]
    fn the_root_is_a_root() {
        assert_eq!(PANEL.parent, None);
        assert_eq!(PANEL.seal, Seal::Open);
        assert!(!PANEL.yields_to_engine);
    }
}
