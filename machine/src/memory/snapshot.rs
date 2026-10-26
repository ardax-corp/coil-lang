//! Capture a heap object graph as a [`HeapSnapshot`] and rebuild it.
//!
//! Capture runs on the compile-time VM after a `const` initializer returns;
//! restore runs once at startup (`RestoreSnapshot`). A word is a reference
//! exactly when the collector would treat it as one: pointer kinds always,
//! unknown kinds when they name a live object (bit 0 masked, as
//! [`Heap::mark_value`] does), scalar kinds never. So a snapshot keeps
//! everything the collector keeps alive and nothing else.

use common::{HeapSnapshot, SnapObject, SnapStep, SnapWord, SnapshotError, Value};

use super::super::AddrHashBuilder;
use super::{EnumPayload, Heap, Member, ObjArray, ObjBoxed, ObjEnum, ObjInstance, ObjString, ObjTuple, Object};

/// A root to capture: the static slot it is stored to, its value, and the
/// slot's word kind (`common::WORD_*`).
#[derive(Clone, Copy)]
pub struct SnapRoot {
    pub slot: u32,
    pub value: Value,
    pub kind: u8,
}

type AddrIndex = std::collections::HashMap<u64, u32, AddrHashBuilder>;

struct Capture<'h> {
    heap: &'h Heap,
    index: AddrIndex,
    /// Objects in table order, with their (parent object or root, step).
    found: Vec<(Object, Parent)>,
    objects: Vec<SnapObject>,
}

#[derive(Clone, Copy)]
enum Parent {
    Root(usize),
    Object(u32, SnapStep),
}

impl Heap {
    /// Copy everything `roots` reach into a snapshot.
    pub fn capture_snapshot(&self, roots: &[SnapRoot]) -> Result<HeapSnapshot, SnapshotError> {
        let mut c = Capture {
            heap: self,
            index: AddrIndex::default(),
            found: Vec::new(),
            objects: Vec::new(),
        };
        let mut statics = Vec::with_capacity(roots.len());
        for (i, root) in roots.iter().enumerate() {
            let word = c.word(root.value, root.kind, Parent::Root(i))?;
            statics.push((root.slot, word));
        }
        // `found` grows while it is walked: each object is visited once.
        let mut next = 0;
        while next < c.found.len() {
            let object = c.object(next as u32)?;
            c.objects.push(object);
            next += 1;
        }
        Ok(HeapSnapshot {
            objects: c.objects,
            statics,
        })
    }

    /// Rebuild `snapshot` and return each static slot's value. Every object
    /// is allocated before any word is written, so cycles and sharing come
    /// back as they were. Runs inside one instruction: no safepoint can
    /// collect a half-built graph, and objects allocated during a mark are
    /// black.
    pub fn restore_snapshot(&mut self, snapshot: &HeapSnapshot) -> Vec<(u32, Value)> {
        let objects: Vec<Object> = snapshot
            .objects
            .iter()
            .map(|object| self.alloc_snapshot_shell(object))
            .collect();
        for (object, snap) in objects.iter().zip(&snapshot.objects) {
            match (object, snap) {
                (Object::Array(gc), SnapObject::Array { words, .. }) => {
                    let values: Vec<Value> = words.iter().map(|w| self.snapshot_value(&objects, *w)).collect();
                    gc.payload_mut().words_mut().copy_from_slice(&values);
                }
                (Object::Tuple(gc), SnapObject::Tuple { words, .. }) => {
                    let values: Vec<Value> = words.iter().map(|w| self.snapshot_value(&objects, *w)).collect();
                    gc.payload_mut().elements.as_mut_slice().copy_from_slice(&values);
                }
                (Object::Enum(gc), SnapObject::Enum { words, .. }) => {
                    let values: Vec<Value> = words.iter().map(|w| self.snapshot_value(&objects, *w)).collect();
                    gc.payload_mut().payload.as_mut_slice().copy_from_slice(&values);
                }
                (Object::Instance(gc), SnapObject::Instance { words, .. }) => {
                    for (i, w) in words.iter().enumerate() {
                        let v = self.snapshot_value(&objects, *w);
                        gc.payload_mut().set_slot(i, v);
                    }
                }
                (Object::Boxed(gc), SnapObject::Boxed { word, .. }) => {
                    gc.payload_mut().payload = match *word {
                        SnapWord::Ref { index, err_bit: false } => Member::Object(objects[index as usize]),
                        w => Member::Value(self.snapshot_value(&objects, w)),
                    };
                }
                _ => {}
            }
        }
        snapshot
            .statics
            .iter()
            .map(|(slot, w)| (*slot, self.snapshot_value(&objects, *w)))
            .collect()
    }

    /// An object of the right kind and size with zero words.
    fn alloc_snapshot_shell(&mut self, snap: &SnapObject) -> Object {
        let zeros = |n: usize| vec![Value::from(0u64); n];
        match snap {
            SnapObject::String(s) => self.alloc(ObjString::new(s.clone()), Object::String).0,
            SnapObject::Bytes(b) => self.alloc(ObjArray::from_bytes(b.clone()), Object::Array).0,
            SnapObject::Array { elem_kind, words } => {
                let mut array = ObjArray::new(zeros(words.len()));
                if *elem_kind == common::WORD_POINTER {
                    array.set_elem_kind(common::WORD_POINTER);
                }
                self.alloc(array, Object::Array).0
            }
            SnapObject::Tuple { kinds, words } => {
                self.alloc(ObjTuple::new(zeros(words.len())).with_kinds(*kinds), Object::Tuple).0
            }
            SnapObject::Enum { tag, type_id, kinds, words } => {
                let mut e = ObjEnum::new(*tag, EnumPayload::from_vec(zeros(words.len())).with_kinds(*kinds));
                e.type_id = *type_id;
                self.alloc(e, Object::Enum).0
            }
            SnapObject::Instance { type_id, words } => {
                self.alloc(ObjInstance::with_slots(*type_id, zeros(words.len())), Object::Instance).0
            }
            SnapObject::Boxed { tag, .. } => {
                let boxed = ObjBoxed {
                    tag: *tag,
                    payload: Member::Value(Value::from(0u64)),
                };
                self.alloc(boxed, Object::Boxed).0
            }
        }
    }

    fn snapshot_value(&mut self, objects: &[Object], word: SnapWord) -> Value {
        match word {
            SnapWord::Raw(raw) => Value::from(raw),
            SnapWord::Ref { index, err_bit } => Value::from(objects[index as usize].addr() | u64::from(err_bit)),
            SnapWord::UnitEnum { tag, err_bit } => Value::from(self.immortal_unit_enum(tag).addr() | u64::from(err_bit)),
            SnapWord::EmptyTuple { err_bit } => Value::from(self.immortal_empty_tuple().addr() | u64::from(err_bit)),
        }
    }
}

impl Capture<'_> {
    /// The snapshot form of word `v` of kind `kind`, recording a newly
    /// reached object under `parent`.
    fn word(&mut self, v: Value, kind: u8, parent: Parent) -> Result<SnapWord, SnapshotError> {
        if kind == common::WORD_SCALAR || kind == common::WORD_BYTE {
            return Ok(SnapWord::Raw(v.bits()));
        }
        let addr = v.heap_addr();
        let Some(object) = (addr != 0).then(|| self.heap.find_object_by_addr(addr)).flatten() else {
            return Ok(SnapWord::Raw(v.bits()));
        };
        self.object_word(object, v.bits() & 1 != 0, parent)
    }

    fn object_word(&mut self, object: Object, err_bit: bool, parent: Parent) -> Result<SnapWord, SnapshotError> {
        // Shared singletons are rebuilt from the restoring heap's own.
        match &object {
            Object::Enum(gc) if gc.as_ref().payload.is_empty() => {
                return Ok(SnapWord::UnitEnum {
                    tag: gc.as_ref().tag,
                    err_bit,
                });
            }
            Object::Tuple(gc) if gc.as_ref().elements().is_empty() => return Ok(SnapWord::EmptyTuple { err_bit }),
            _ => {}
        }
        if let Some(what) = unsupported(&object) {
            return Err(self.error(parent, what));
        }
        let index = match self.index.get(&object.addr()) {
            Some(&i) => i,
            None => {
                let i = self.found.len() as u32;
                self.index.insert(object.addr(), i);
                self.found.push((object, parent));
                i
            }
        };
        Ok(SnapWord::Ref { index, err_bit })
    }

    /// Snapshot object `at`, recording the objects its words reach.
    fn object(&mut self, at: u32) -> Result<SnapObject, SnapshotError> {
        let (object, _) = self.found[at as usize];
        let parent = |step| Parent::Object(at, step);
        Ok(match object {
            Object::String(gc) => SnapObject::String(gc.as_ref().data.to_string()),
            Object::Array(gc) if gc.as_ref().is_packed() => SnapObject::Bytes(gc.as_ref().to_byte_vec()),
            Object::Array(gc) => {
                let a = gc.as_ref();
                let kind = a.elem_kind();
                let words = a
                    .words()
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                    .map(|(i, v)| self.word(*v, kind, parent(SnapStep::Element(i))))
                    .collect::<Result<_, _>>()?;
                SnapObject::Array {
                    elem_kind: if kind == common::WORD_POINTER { kind } else { common::WORD_UNKNOWN },
                    words,
                }
            }
            Object::Tuple(gc) => {
                let t = gc.as_ref();
                let kinds = t.kinds();
                let words = self.kinded(t.elements(), kinds, |i| parent(SnapStep::TupleElement(i)))?;
                SnapObject::Tuple { kinds, words }
            }
            Object::Enum(gc) => {
                let e = gc.as_ref();
                let kinds = e.payload.kinds();
                let words = self.kinded(&e.payload, kinds, |i| parent(SnapStep::Payload(i)))?;
                SnapObject::Enum {
                    tag: e.tag,
                    type_id: e.type_id,
                    kinds,
                    words,
                }
            }
            Object::Instance(gc) => {
                let inst = gc.as_ref();
                let Some(slots) = inst.slots() else {
                    return Err(self.error_at(at, "an untyped instance"));
                };
                let kinds = self.heap.class_field_kinds(inst.type_id);
                let words = slots
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let kind = kinds.get(i).copied().unwrap_or(common::WORD_UNKNOWN);
                        self.word(*v, kind, parent(SnapStep::Field { type_id: inst.type_id, index: i }))
                    })
                    .collect::<Result<_, _>>()?;
                SnapObject::Instance {
                    type_id: inst.type_id,
                    words,
                }
            }
            Object::Boxed(gc) => {
                let b = gc.as_ref();
                let word = match b.payload {
                    Member::Object(o) => self.object_word(o, false, parent(SnapStep::Boxed))?,
                    Member::Value(v) => self.word(v, common::WORD_UNKNOWN, parent(SnapStep::Boxed))?,
                };
                SnapObject::Boxed { tag: b.tag, word }
            }
            _ => unreachable!("unsupported objects are refused before they are recorded"),
        })
    }

    fn kinded(
        &mut self,
        words: &[Value],
        kinds: u8,
        step: impl Fn(usize) -> Parent,
    ) -> Result<Vec<SnapWord>, SnapshotError> {
        words
            .iter()
            .enumerate()
            .map(|(i, v)| self.word(*v, common::packed_word_kind(kinds, i), step(i)))
            .collect()
    }

    fn error(&self, parent: Parent, what: &'static str) -> SnapshotError {
        let mut path = Vec::new();
        let mut at = parent;
        loop {
            match at {
                Parent::Root(root) => {
                    path.reverse();
                    return SnapshotError { root, path, what };
                }
                Parent::Object(object, step) => {
                    path.push(step);
                    at = self.found[object as usize].1;
                }
            }
        }
    }

    fn error_at(&self, object: u32, what: &'static str) -> SnapshotError {
        self.error(self.found[object as usize].1, what)
    }
}

/// Why `object` cannot be part of a snapshot, if it can't.
fn unsupported(object: &Object) -> Option<&'static str> {
    Some(match object {
        Object::String(_)
        | Object::Array(_)
        | Object::Tuple(_)
        | Object::Enum(_)
        | Object::Instance(_)
        | Object::Boxed(_) => return None,
        Object::Fn(_) => "a function value",
        Object::Coroutine(_) => "a coroutine",
        Object::Stream(_) => "a stream",
        Object::Thread(_) => "a thread handle",
        Object::Sender(_) => "a channel sender",
        Object::Receiver(_) => "a channel receiver",
        Object::Mutex(_) => "a mutex",
        Object::RwLock(_) => "a read-write lock",
        Object::Library(_) => "a native library handle",
        Object::Root(_) => "a GC root handle",
        Object::Weak(_) => "a weak reference",
    })
}

#[cfg(test)]
#[path = "snapshot.tests.rs"]
mod tests;
