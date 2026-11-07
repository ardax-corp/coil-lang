//! The bodies of a compile: what each function lowered to, and how the rest
//! of the compiler names it.

use std::collections::HashMap;

/// A body's entry, as MIR calls name it. A label can be reserved before its
/// body compiles, so a method can call one defined later in its `impl`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Label(pub u32);

/// One compiled body.
#[derive(Clone, Debug)]
pub struct Body {
    /// Fully-qualified / overload key used by the compiler.
    pub name: String,
    pub entry: Option<Label>,
    /// What the body lowered to.
    pub mir: Option<Box<crate::mir::MirFunc>>,
    /// Why it did not lower (`--opt-stats`).
    pub mir_why: Option<String>,
}

/// The bodies compiled so far. Each takes a position as it starts; the
/// position names it until finalize turns it into its entry label.
#[derive(Clone)]
pub struct BodyTable {
    position: usize,
    next_label: u32,
    entry_at: HashMap<usize, Label>,
    bodies: Vec<Body>,
}

impl Default for BodyTable {
    fn default() -> Self {
        Self { position: Self::START, next_label: 0, entry_at: HashMap::new(), bodies: Vec::new() }
    }
}

impl BodyTable {
    /// Position 0 names nothing.
    const START: usize = 1;

    pub fn new() -> Self {
        Self::default()
    }

    /// Whether no body has taken a position yet.
    pub fn is_fresh(&self) -> bool {
        self.position == Self::START
    }

    pub fn position(&self) -> usize {
        self.position
    }

    /// End the body at the current position: the next one gets a new one.
    pub fn advance(&mut self) {
        self.position += 1;
    }

    pub fn fresh_label(&mut self) -> Label {
        let label = Label(self.next_label);
        self.next_label += 1;
        label
    }

    /// Give the current position a new entry label.
    pub fn bind_fresh_entry(&mut self) -> Label {
        let label = self.fresh_label();
        self.bind_reserved_entry(label);
        label
    }

    /// Give the current position a label [`Self::fresh_label`] reserved.
    pub fn bind_reserved_entry(&mut self, label: Label) {
        self.entry_at.insert(self.position, label);
    }

    pub fn entry_label_for_offset(&self, position: usize) -> Option<Label> {
        self.entry_at.get(&position).copied()
    }

    pub fn record(&mut self, name: impl Into<String>, entry: Option<Label>) {
        self.bodies.push(Body { name: name.into(), entry, mir: None, mir_why: None });
    }

    /// Attach the MIR of the last recorded body.
    pub fn set_last_mir(&mut self, func: crate::mir::MirFunc) {
        if let Some(body) = self.bodies.last_mut() {
            body.mir = Some(Box::new(func));
        }
    }

    /// Record why the last recorded body did not lower.
    pub fn set_last_mir_why(&mut self, why: String) {
        if let Some(body) = self.bodies.last_mut() {
            body.mir_why = Some(why);
        }
    }

    /// Mark the body entered at `entry` as having the two-word twin at
    /// `twin` ([`crate::mir::MirFunc::pair_twin`]).
    /// `false`, changing nothing, when the twin has no MIR returning two
    /// words.
    pub fn set_pair_twin(&mut self, entry: Label, twin: Label) -> bool {
        let two = self.bodies.iter().rev().find(|b| b.entry == Some(twin)).and_then(|b| b.mir.as_ref()).is_some_and(|m| m.ret_hi_ty.is_some());
        if !two {
            return false;
        }
        if let Some(mir) = self.bodies.iter_mut().rev().find(|b| b.entry == Some(entry)).and_then(|b| b.mir.as_mut()) {
            mir.pair_twin = Some(twin);
            return true;
        }
        false
    }

    pub fn funcs(&self) -> &[Body] {
        &self.bodies
    }

    pub fn retain_funcs(&mut self, mut keep: impl FnMut(&Body) -> bool) {
        self.bodies.retain(|body| keep(body));
    }
}
