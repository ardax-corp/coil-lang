use std::cell::Cell;

use common::{HeapSnapshot, SnapStep, SnapWord, SnapshotError, Value};

use super::super::{EnumPayload, Heap, ObjArray, ObjEnum, ObjInstance, ObjString, ObjTuple, ObjWeak, Object};
use super::SnapRoot;

fn string(heap: &mut Heap, s: &str) -> Value {
    Value::from(heap.alloc(ObjString::from(s), Object::String).0.addr())
}

fn root(slot: u32, value: Value) -> SnapRoot {
    SnapRoot {
        slot,
        value,
        kind: common::WORD_UNKNOWN,
    }
}

/// Capture, encode, decode and restore into a fresh heap.
fn transplant(heap: &Heap, roots: &[SnapRoot]) -> (Heap, Vec<(u32, Value)>) {
    let snapshot = heap.capture_snapshot(roots).expect("capture");
    let decoded = HeapSnapshot::decode(&snapshot.encode()).expect("decode");
    assert_eq!(decoded, snapshot);
    let mut fresh = Heap::default();
    let statics = fresh.restore_snapshot(&decoded);
    (fresh, statics)
}

fn object(heap: &Heap, v: Value) -> Object {
    heap.find_object_by_addr(v.heap_addr()).expect("object")
}

fn text(heap: &Heap, v: Value) -> String {
    match object(heap, v) {
        Object::String(gc) => gc.as_ref().data.to_string(),
        _ => panic!("not a string"),
    }
}

fn instance_slots(heap: &Heap, v: Value) -> Vec<Value> {
    match object(heap, v) {
        Object::Instance(gc) => gc.as_ref().slots().expect("typed").to_vec(),
        _ => panic!("not an instance"),
    }
}

fn array_words(heap: &Heap, v: Value) -> Vec<Value> {
    match object(heap, v) {
        Object::Array(gc) => gc.as_ref().words().expect("words").to_vec(),
        _ => panic!("not an array"),
    }
}

/// Instance → (shared string, int array, the same string again); the
/// copy keeps the sharing and the ints.
#[test]
fn restores_instances_arrays_and_shared_strings() {
    let mut heap = Heap::default();
    let name = string(&mut heap, "coil");
    let ints = Value::from(heap.alloc(ObjArray::new(vec![Value::from(1i64), Value::from(-7i64)]), Object::Array).0.addr());
    let inst = ObjInstance::with_slots(5, vec![name, ints, name, Value::from(42i64)]);
    let inst = Value::from(heap.alloc(inst, Object::Instance).0.addr());

    let (fresh, statics) = transplant(&heap, &[root(3, inst)]);
    assert_eq!(statics.len(), 1);
    assert_eq!(statics[0].0, 3);
    let slots = instance_slots(&fresh, statics[0].1);
    assert_eq!(text(&fresh, slots[0]), "coil");
    assert_eq!(slots[0], slots[2], "one string, shared");
    assert_eq!(array_words(&fresh, slots[1]), vec![Value::from(1i64), Value::from(-7i64)]);
    assert_eq!(slots[3], Value::from(42i64));
    assert_ne!(slots[0], name, "a new object in the new heap");
}

/// An array that holds itself comes back holding itself.
#[test]
fn restores_cycles() {
    let mut heap = Heap::default();
    let (obj, gc) = heap.alloc(ObjArray::new(vec![Value::from(0u64)]), Object::Array);
    gc.payload_mut().words_mut()[0] = Value::from(obj.addr());
    let (fresh, statics) = transplant(&heap, &[root(0, Value::from(obj.addr()))]);
    let a = statics[0].1;
    assert_eq!(array_words(&fresh, a), vec![a]);
}

/// Two roots reaching one object share it after restore.
#[test]
fn roots_share_objects() {
    let mut heap = Heap::default();
    let s = string(&mut heap, "shared");
    let pair = Value::from(heap.alloc(ObjTuple::new(vec![s, Value::from(1i64)]), Object::Tuple).0.addr());
    let (fresh, statics) = transplant(&heap, &[root(0, s), root(1, pair)]);
    let Object::Tuple(t) = object(&fresh, statics[1].1) else { panic!("tuple") };
    assert_eq!(t.as_ref().elements()[0], statics[0].1);
}

/// A scalar-kinded word is never followed, even when it equals a live
/// object's address; an unknown-kinded one is.
#[test]
fn scalar_kinds_are_never_references() {
    let mut heap = Heap::default();
    let s = string(&mut heap, "x");
    let kinds = common::pack_word_kinds([common::WORD_SCALAR, common::WORD_UNKNOWN]);
    let t = ObjTuple::new(vec![s, s]).with_kinds(kinds);
    let t = Value::from(heap.alloc(t, Object::Tuple).0.addr());
    let snapshot = heap.capture_snapshot(&[root(0, t)]).expect("capture");
    let common::SnapObject::Tuple { words, .. } = &snapshot.objects[0] else { panic!("tuple") };
    assert_eq!(words[0], SnapWord::Raw(s.bits()));
    assert!(matches!(words[1], SnapWord::Ref { .. }));
    let scalar_root = SnapRoot {
        kind: common::WORD_SCALAR,
        ..root(0, s)
    };
    let snapshot = heap.capture_snapshot(&[scalar_root]).expect("capture");
    assert!(snapshot.objects.is_empty());
}

/// Unit variants and `()` map onto the restoring heap's shared objects;
/// a payload enum keeps its tag, finalizer key and word kinds, and a niche
/// `Err` keeps its low bit.
#[test]
fn singletons_enums_and_niche_bits() {
    let mut heap = Heap::default();
    let none = Value::from(heap.immortal_unit_enum(0).addr());
    let unit = Value::from(heap.immortal_empty_tuple().addr());
    let msg = string(&mut heap, "bad");
    let err = Value::from(msg.heap_addr() | 1);
    let mut some = ObjEnum::new(1, EnumPayload::one(msg).with_kinds(common::pack_word_kinds([common::WORD_POINTER])));
    some.type_id = 77;
    let some = Value::from(heap.alloc(some, Object::Enum).0.addr());
    let roots = [
        root(0, none),
        root(1, unit),
        SnapRoot {
            kind: common::WORD_POINTER,
            ..root(2, err)
        },
        root(3, some),
    ];
    let (mut fresh, statics) = transplant(&heap, &roots);
    assert_eq!(statics[0].1, Value::from(fresh.immortal_unit_enum(0).addr()));
    assert_eq!(statics[1].1, Value::from(fresh.immortal_empty_tuple().addr()));
    let err = statics[2].1;
    assert_eq!(err.bits() & 1, 1);
    assert_eq!(text(&fresh, err), "bad");
    let Object::Enum(e) = object(&fresh, statics[3].1) else { panic!("enum") };
    let e = e.as_ref();
    assert_eq!((e.tag, e.type_id), (1, 77));
    assert_eq!(e.payload.kinds(), common::pack_word_kinds([common::WORD_POINTER]));
    assert_eq!(text(&fresh, e.payload[0]), "bad");
}

/// Packed byte arrays stay packed.
#[test]
fn byte_arrays_stay_packed() {
    let mut heap = Heap::default();
    let bytes = Value::from(heap.alloc(ObjArray::from_bytes(vec![0, 1, 254, 255]), Object::Array).0.addr());
    let (fresh, statics) = transplant(&heap, &[root(0, bytes)]);
    let Object::Array(a) = object(&fresh, statics[0].1) else { panic!("array") };
    assert!(a.as_ref().is_packed());
    assert_eq!(a.as_ref().to_byte_vec(), vec![0, 1, 254, 255]);
}

/// A value with no snapshot form is refused with the path to it.
#[test]
fn refuses_handles_with_their_path() {
    let mut heap = Heap::default();
    let weak = ObjWeak {
        target: Cell::new(Value::from(0u64)),
        cleared: Cell::new(true),
    };
    let weak = Value::from(heap.alloc(weak, Object::Weak).0.addr());
    let inner = Value::from(heap.alloc(ObjArray::new(vec![Value::from(1i64), weak]), Object::Array).0.addr());
    let inst = Value::from(heap.alloc(ObjInstance::with_slots(4, vec![Value::from(0i64), inner]), Object::Instance).0.addr());
    let err = heap
        .capture_snapshot(&[root(9, Value::from(1i64)), root(0, inst)])
        .expect_err("weak is refused");
    assert_eq!(
        err,
        SnapshotError {
            root: 1,
            path: vec![SnapStep::Field { type_id: 4, index: 1 }, SnapStep::Element(1)],
            what: "a weak reference",
        }
    );
}
