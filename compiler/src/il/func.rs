//! Lightweight per-function IL metadata.
//!
//! Recorded at function finalize on the flat [`super::CodeBuf`]. At lower time
//! [`super::IlModule::from_flat`] takes ownership of each body's ops for scoped
//! opts / CFG GVN; emitting spans are the split keys until then.

use super::Label;

/// Span of one compiled function inside the shared IL buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IlFunc {
    /// Fully-qualified / overload key used by the compiler.
    pub name: String,
    /// Entry label bound at the function start (if recorded).
    pub entry: Option<Label>,
    /// Inclusive-exclusive emitting-op indices in [`super::CodeBuf`].
    pub code_start: usize,
    pub code_end: usize,
    /// Stack height at body entry (args + `self` + dict slots). SP analysis for
    /// per-func opts must start here — locals and the operand stack share memory.
    pub entry_sp: u32,
    /// Non-escaping unboxed class field ranges `(base, n)` from the
    /// local_escape sidecar (I3). Empty when the body has no such locals.
    pub unboxed_fields: Vec<(u32, u32)>,
}

impl IlFunc {
    #[cfg(test)]
    pub fn new(
        name: impl Into<String>,
        entry: Option<Label>,
        code_start: usize,
        code_end: usize,
    ) -> Self {
        Self::with_entry_sp(name, entry, code_start, code_end, 0)
    }

    pub fn with_entry_sp(
        name: impl Into<String>,
        entry: Option<Label>,
        code_start: usize,
        code_end: usize,
        entry_sp: u32,
    ) -> Self {
        Self {
            name: name.into(),
            entry,
            code_start,
            code_end,
            entry_sp,
            unboxed_fields: Vec::new(),
        }
    }
}
