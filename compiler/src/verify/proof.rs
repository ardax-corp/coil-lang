//! `foo.hy.proof`: what `coil verify` proved about `foo.hy`, for builds to
//! drop the checks it covers.
//!
//! The file names the compiler version and a hash of the source it was
//! proved from; a build uses it only while both match, so an edit to the
//! file (or a new compiler) makes it stale rather than wrong.
//!
//! - A proved `ensures` or `invariant` of the entry module's own functions
//!   is dropped. Its proof assumes the function's `requires` and the
//!   `ensures` of the callees it calls, which are still checked or proved
//!   themselves, so it holds wherever the program gets past those.
//! - A proved index is compiled unchecked only when every contract goal of
//!   the module was proved (`complete`): with `--contracts=requires` the
//!   callee `ensures` its proof assumed are not checked at run time.
//!
//! Fields after the function name are tab-separated (`␉` below):
//!
//! ```text
//! coil-proof 1
//! compiler 0.1.0
//! source 9f3a…
//! complete true
//! check clamp␉120␉160␉ensures result >= lo
//! bounds last␉210␉222
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::{FnCheck, encode::BOUNDS};

const MAGIC: &str = "coil-proof 1";

/// A clause or index site: the function, its span, and (for a clause) its
/// text as the check reports it.
pub type Site = (String, usize, usize, String);

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Proof {
    /// Every contract goal of the module was proved.
    pub complete: bool,
    /// Proved `ensures` / `invariant` clauses.
    pub checks: HashSet<Site>,
    /// Proved index sites (empty clause text).
    pub bounds: HashSet<Site>,
}

/// What a goal turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Proved,
    Failed,
    Unknown,
    Skipped,
}

impl Proof {
    /// The proof of a module whose goals came out as `outcome(fn, goal)`.
    pub fn from_outcomes(checks: &[FnCheck], outcome: impl Fn(usize, usize) -> Outcome) -> Self {
        let mut proof = Proof { complete: true, ..Proof::default() };
        for (i, check) in checks.iter().enumerate() {
            for (j, goal) in check.goals.iter().enumerate() {
                let result = outcome(i, j);
                let bounds = goal.keyword == BOUNDS;
                if !bounds && result != Outcome::Proved && result != Outcome::Skipped {
                    proof.complete = false;
                }
                if result != Outcome::Proved || goal.callee.is_some() {
                    continue;
                }
                let (start, end) = goal.span;
                if bounds {
                    proof.bounds.insert((check.name.clone(), start, end, String::new()));
                } else if goal.keyword == "ensures" || goal.keyword == "invariant" {
                    proof.checks.insert((check.name.clone(), start, end, goal.clause.clone()));
                }
            }
        }
        proof
    }

    pub fn render(&self, source: &str) -> String {
        let mut out = format!("{MAGIC}\ncompiler {}\nsource {:016x}\ncomplete {}\n", version(), hash(source), self.complete);
        let mut checks: Vec<_> = self.checks.iter().collect();
        checks.sort();
        for (f, s, e, clause) in checks {
            out.push_str(&format!("check {f}\t{s}\t{e}\t{clause}\n"));
        }
        let mut bounds: Vec<_> = self.bounds.iter().collect();
        bounds.sort();
        for (f, s, e, _) in bounds {
            out.push_str(&format!("bounds {f}\t{s}\t{e}\n"));
        }
        out
    }

    /// The proof in `text`, when it was made from `source` by this compiler.
    pub fn parse(text: &str, source: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != MAGIC {
            return None;
        }
        if lines.next()?.strip_prefix("compiler ")? != version().as_str() {
            return None;
        }
        if lines.next()?.strip_prefix("source ")? != format!("{:016x}", hash(source)) {
            return None;
        }
        let complete = lines.next()?.strip_prefix("complete ")? == "true";
        let mut proof = Proof { complete, ..Proof::default() };
        for line in lines {
            if let Some(rest) = line.strip_prefix("check ") {
                let mut parts = rest.splitn(4, '\t');
                let (f, s, e, clause) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
                proof.checks.insert((f.to_string(), s.parse().ok()?, e.parse().ok()?, clause.to_string()));
            } else if let Some(rest) = line.strip_prefix("bounds ") {
                let mut parts = rest.splitn(3, '\t');
                let (f, s, e) = (parts.next()?, parts.next()?, parts.next()?);
                proof.bounds.insert((f.to_string(), s.parse().ok()?, e.parse().ok()?, String::new()));
            } else if !line.is_empty() {
                return None;
            }
        }
        Some(proof)
    }

    /// The proof next to `entry` that still matches it.
    pub fn load(entry: &Path) -> Option<Self> {
        let source = std::fs::read_to_string(entry).ok()?;
        let text = std::fs::read_to_string(path_for(entry)).ok()?;
        Self::parse(&text, &source)
    }
}

/// `foo.hy` → `foo.hy.proof`.
pub fn path_for(entry: &Path) -> PathBuf {
    let mut name = entry.as_os_str().to_owned();
    name.push(".proof");
    PathBuf::from(name)
}

/// Bumped when what a proof says about a source changes: the encoder's
/// model, or how spans are numbered.
const FORMAT: u32 = 1;

fn version() -> String {
    format!("{}+{FORMAT}", env!("CARGO_PKG_VERSION"))
}

/// FNV-1a: stable across builds and platforms.
fn hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

thread_local! {
    static PROOF: std::cell::RefCell<Option<Proof>> = const { std::cell::RefCell::new(None) };
}

/// The proof HIR building applies to the entry module on this thread,
/// until the scope ends.
pub(crate) struct ProofScope;

impl ProofScope {
    pub(crate) fn set(proof: Option<Proof>) -> Self {
        PROOF.with(|p| *p.borrow_mut() = proof);
        ProofScope
    }
}

impl Drop for ProofScope {
    fn drop(&mut self) {
        PROOF.with(|p| *p.borrow_mut() = None);
    }
}

pub(crate) fn with_proof<R>(f: impl FnOnce(Option<&Proof>) -> R) -> R {
    PROOF.with(|p| f(p.borrow().as_ref()))
}

#[cfg(test)]
#[path = "proof.tests.rs"]
mod tests;
