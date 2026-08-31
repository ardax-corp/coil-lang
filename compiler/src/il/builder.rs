//! IL stream builder with symbolic label allocation.

use std::collections::{BTreeMap, BTreeSet};

use common::{Byte, DebugLoc};

use super::op::{EntryKind, FuseHint, IlJumpKind, IlOp, Label};

/// Error from IL finalize / lower.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IlError {
    /// A jump or entry targeted a label that was never bound.
    UnboundLabel(Label),
}

impl std::fmt::Display for IlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IlError::UnboundLabel(label) => {
                write!(f, "label {:?} was never bound", label)
            }
        }
    }
}

impl std::error::Error for IlError {}

/// Accumulates stack IL with symbolic jump/entry targets.
#[derive(Clone, Default)]
pub struct IlBuilder {
    ops: Vec<IlOp>,
    next_label_id: u32,
    /// Labels that were targeted by a jump/entry.
    targeted: BTreeSet<u32>,
    /// Labels that have been bound at least once.
    bound: BTreeSet<u32>,
}

// Public IL API retained for opts/tests and future emit paths.
#[allow(dead_code)]
impl IlBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ops(&self) -> &[IlOp] {
        &self.ops
    }

    pub fn ops_mut(&mut self) -> &mut Vec<IlOp> {
        &mut self.ops
    }

    pub fn into_ops(self) -> Vec<IlOp> {
        self.ops
    }

    pub fn clear(&mut self) {
        self.ops.clear();
        self.next_label_id = 0;
        self.targeted.clear();
        self.bound.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Number of code-emitting ops (labels excluded). Useful for spans.
    pub fn code_len(&self) -> usize {
        self.ops.iter().filter(|o| o.emits_code()).count()
    }

    /// Total IL items including label markers.
    pub fn raw_len(&self) -> usize {
        self.ops.len()
    }

    pub fn fresh_label(&mut self) -> Label {
        let id = self.next_label_id;
        self.next_label_id += 1;
        Label(id)
    }

    /// Bind `label` at the current stream position (next emitting op).
    /// Idempotent: a later bind wins at lower time.
    pub fn bind_label(&mut self, label: Label) {
        self.bound.insert(label.0);
        self.ops.push(IlOp::Label(label));
    }

    /// Bind `label` as a value-producing join (match / `?` end).
    pub fn bind_join_label(&mut self, label: Label) {
        self.bound.insert(label.0);
        self.ops.push(IlOp::JoinLabel(label));
    }

    /// Insert a bound label marker at raw op index `raw_idx` (does not append).
    pub fn insert_bound_label_at(&mut self, raw_idx: usize, label: Label) {
        self.bound.insert(label.0);
        self.ops.insert(raw_idx, IlOp::Label(label));
    }

    pub fn emit_jump(&mut self, kind: IlJumpKind, target: Label) {
        self.emit_jump_at(kind, target, DebugLoc::unknown());
    }

    pub fn emit_jump_at(&mut self, kind: IlJumpKind, target: Label, loc: DebugLoc) {
        self.emit_jump_hinted(kind, target, loc, FuseHint::default());
    }

    pub fn emit_jump_hinted(
        &mut self,
        kind: IlJumpKind,
        target: Label,
        loc: DebugLoc,
        hint: FuseHint,
    ) {
        self.targeted.insert(target.0);
        self.ops.push(IlOp::jump_hinted(kind, target, loc, hint));
    }

    pub fn emit_entry(&mut self, kind: EntryKind, arity: u32, target: Label) {
        self.emit_entry_at(kind, arity, target, DebugLoc::unknown());
    }

    pub fn emit_entry_at(&mut self, kind: EntryKind, arity: u32, target: Label, loc: DebugLoc) {
        self.targeted.insert(target.0);
        self.ops.push(IlOp::Entry {
            kind,
            arity,
            target,
            loc,
        });
    }

    pub fn push_byte(&mut self, byte: Byte) {
        self.ops.push(IlOp::byte(byte));
    }

    pub fn push_byte_at(&mut self, byte: Byte, loc: DebugLoc) {
        self.ops.push(IlOp::byte_at(byte, loc));
    }

    /// Append a typed IL op (prefer over [`Self::push_byte`] for hot-set ops).
    pub fn push_op(&mut self, op: IlOp) {
        self.ops.push(op);
    }

    pub fn push_const(&mut self, imm: i32) {
        self.push_op(IlOp::Const {
            imm,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_const_at(&mut self, imm: i32, loc: DebugLoc) {
        self.push_op(IlOp::Const { imm, loc });
    }

    pub fn push_return(&mut self) {
        self.push_op(IlOp::Return {
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_return_at(&mut self, loc: DebugLoc) {
        self.push_op(IlOp::Return { loc });
    }

    pub fn push_load(&mut self, slot: u32) {
        self.push_op(IlOp::Load {
            slot,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_store_pop(&mut self, slot: u32) {
        self.push_op(IlOp::StorePop {
            slot,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_pop(&mut self) {
        self.push_op(IlOp::Pop {
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_index(&mut self) {
        self.push_op(IlOp::Index {
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_make_tuple(&mut self, arity: u32) {
        self.push_op(IlOp::MakeTuple {
            arity,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_make_array(&mut self, arity: u32) {
        self.push_op(IlOp::MakeArray {
            arity,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_make_enum(&mut self, tag: u16, arity: u16) {
        self.push_op(IlOp::MakeEnum {
            tag,
            arity,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_box_value(&mut self, tag: u32) {
        self.push_op(IlOp::BoxValue {
            tag,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_unbox_value(&mut self, tag: u32) {
        self.push_op(IlOp::UnboxValue {
            tag,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_load_field(&mut self, index: u32) {
        self.push_op(IlOp::LoadField {
            index,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_get_field(&mut self) {
        self.push_op(IlOp::GetField {
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_set_field(&mut self) {
        self.push_op(IlOp::SetField {
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_host_invoke(&mut self, arity: u32) {
        self.push_op(IlOp::HostInvoke {
            arity,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_print(&mut self) {
        self.push_op(IlOp::Print {
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_const_pool(&mut self, idx: u32) {
        self.push_op(IlOp::ConstPool {
            idx,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_string(&mut self, idx: u32) {
        self.push_op(IlOp::String {
            idx,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn extend_bytes<I: IntoIterator<Item = Byte>>(&mut self, bytes: I) {
        for b in bytes {
            self.push_byte(b);
        }
    }

    pub fn extend_bytes_at(&mut self, bytes: &[Byte], loc: DebugLoc) {
        for &b in bytes {
            self.push_byte_at(b, loc);
        }
    }

    pub fn append(&mut self, other: &mut IlBuilder) -> BTreeMap<u32, u32> {
        // Merge label id spaces: remap other's labels to fresh ids.
        if other.ops.is_empty() {
            return BTreeMap::new();
        }
        let mut remap: BTreeMap<u32, u32> = BTreeMap::new();
        let mut map_label = |id: u32, me: &mut Self| -> u32 {
            *remap.entry(id).or_insert_with(|| {
                let n = me.next_label_id;
                me.next_label_id += 1;
                n
            })
        };
        for op in other.ops.drain(..) {
            match op {
                IlOp::Label(Label(id)) => {
                    let nid = map_label(id, self);
                    self.bound.insert(nid);
                    self.ops.push(IlOp::Label(Label(nid)));
                }
                IlOp::JoinLabel(Label(id)) => {
                    let nid = map_label(id, self);
                    self.bound.insert(nid);
                    self.ops.push(IlOp::JoinLabel(Label(nid)));
                }
                IlOp::Jump {
                    kind,
                    target,
                    loc,
                    hint,
                } => {
                    let nid = map_label(target.0, self);
                    self.targeted.insert(nid);
                    self.ops.push(IlOp::Jump {
                        kind,
                        target: Label(nid),
                        loc,
                        hint,
                    });
                }
                IlOp::Entry {
                    kind,
                    arity,
                    target,
                    loc,
                } => {
                    let nid = map_label(target.0, self);
                    self.targeted.insert(nid);
                    self.ops.push(IlOp::Entry {
                        kind,
                        arity,
                        target: Label(nid),
                        loc,
                    });
                }
                other_op => self.ops.push(other_op),
            }
        }
        other.clear();
        remap
    }

    /// Append another builder that shares this builder's label namespace
    /// (no remapping). Used when fragments were emitted against the same
    /// parent label allocator.
    pub fn append_shared(&mut self, other: &mut IlBuilder) {
        self.targeted.extend(other.targeted.iter().copied());
        self.bound.extend(other.bound.iter().copied());
        self.next_label_id = self.next_label_id.max(other.next_label_id);
        self.ops.append(&mut other.ops);
        other.targeted.clear();
        other.bound.clear();
    }

    pub fn push_prologue_jmp(&mut self) {
        self.ops.push(IlOp::PrologueJmp {
            loc: DebugLoc::unknown(),
        });
    }

    /// Ensure every targeted label was bound.
    pub fn finalize_labels(&self) -> Result<(), IlError> {
        for id in &self.targeted {
            if !self.bound.contains(id) {
                return Err(IlError::UnboundLabel(Label(*id)));
            }
        }
        Ok(())
    }

    /// Splice `inserted` before the first op at logical code index `code_pos`
    /// (counting only emitting ops). Used for static-init insertion.
    pub fn splice_code_at(&mut self, code_pos: usize, mut inserted: IlBuilder) {
        let mut emitting = 0usize;
        let mut raw_idx = self.ops.len();
        for (i, op) in self.ops.iter().enumerate() {
            if emitting == code_pos {
                raw_idx = i;
                break;
            }
            if op.emits_code() {
                emitting += 1;
            }
        }
        if emitting < code_pos {
            raw_idx = self.ops.len();
        }
        let mut chunk = std::mem::take(&mut inserted.ops);
        // Remap labels from inserted into our namespace.
        let mut remap: BTreeMap<u32, u32> = BTreeMap::new();
        for op in &mut chunk {
            match op {
                IlOp::Label(Label(id))
                | IlOp::JoinLabel(Label(id))
                | IlOp::Jump {
                    target: Label(id), ..
                }
                | IlOp::Entry {
                    target: Label(id), ..
                } => {
                    let nid = *remap.entry(*id).or_insert_with(|| {
                        let n = self.next_label_id;
                        self.next_label_id += 1;
                        n
                    });
                    *id = nid;
                }
                _ => {}
            }
        }
        for op in &chunk {
            if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
                self.bound.insert(*id);
            }
            if let IlOp::Jump {
                target: Label(id), ..
            }
            | IlOp::Entry {
                target: Label(id), ..
            } = op
            {
                self.targeted.insert(*id);
            }
        }
        self.ops.splice(raw_idx..raw_idx, chunk);
    }
}
