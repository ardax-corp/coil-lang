//! Where MIR lowering boxes a value (`coil dissect --boxes`).
//!
//! A box is a heap object made only so that a value fits in one word: a
//! two-word pair (`Option<int>`, `(int, float)`) or a niche word turned into
//! its enum object. While a capture is open, lowering records each place it
//! makes one, so a test or `coil dissect --boxes` can say which positions
//! still box and why (`docs/internals/layout.md`).

use std::cell::RefCell;

/// What was boxed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoxKind {
    /// A two-word pair made one word (`box_pair_val`).
    Pair,
    /// A two-word enum variant made boxed in place of its pair.
    Variant,
    /// A niche `Option` / `Result` word made its enum object.
    Niche,
    /// A host native's two-word result, which the runtime returns boxed.
    HostResult,
    /// A lane array turned into one boxed word per element in place
    /// (`LanesToWords`), for code that reads one word per element.
    LaneArray,
}

impl BoxKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pair => "pair",
            Self::Variant => "variant",
            Self::Niche => "niche",
            Self::HostResult => "host result",
            Self::LaneArray => "lane array",
        }
    }
}

/// One place a body boxes a value.
#[derive(Clone, Debug)]
pub struct BoxSite {
    /// The body that boxes.
    pub func: String,
    pub kind: BoxKind,
    /// The boxed value's type (or the host native and its result type).
    pub ty: String,
    /// The lowering code that asked for the box (`from_hir.rs:LINE`): which
    /// position it is, for whoever removes it.
    pub site: String,
}

thread_local! {
    static CAPTURE: RefCell<Option<Vec<BoxSite>>> = const { RefCell::new(None) };
}

/// Start recording box sites (drops an earlier capture).
pub fn start_box_capture() {
    CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
}

/// The sites recorded since [`start_box_capture`], each once, in the
/// order lowering met them.
pub fn take_box_capture() -> Vec<BoxSite> {
    let sites = CAPTURE.with(|c| c.borrow_mut().take().unwrap_or_default());
    let mut seen = std::collections::HashSet::new();
    sites.into_iter().filter(|s| seen.insert((s.func.clone(), s.kind, s.ty.clone(), s.site.clone()))).collect()
}

/// Record a box made in `func`; the site is the first caller outside the
/// `#[track_caller]` chain.
#[track_caller]
pub(super) fn note(func: &str, kind: BoxKind, ty: impl FnOnce() -> String) {
    let site = std::panic::Location::caller();
    CAPTURE.with(|c| {
        if let Some(out) = c.borrow_mut().as_mut() {
            let file = site.file().rsplit(['/', '\\']).next().unwrap_or(site.file());
            out.push(BoxSite { func: func.to_string(), kind, ty: ty(), site: format!("{file}:{}", site.line()) });
        }
    });
}
