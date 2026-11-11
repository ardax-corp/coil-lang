//! Structural equality for heap values used by `EQ` / `NEQ`.
//!
//! Immediates and non-aggregate heap objects compare by machine word.
//! Arrays (and nested arrays) compare by length and element-wise recursion.
//! Strings compare by UTF-8 content (interned or not). A product compares
//! slot by slot ([`product_slots`]): a boxed `ObjEnum` of the same tag (so
//! `Result::Ok(x) == Result::Ok(x)` holds when the constructs still box; a
//! tuple is one of `PRODUCT_TAG`) or a record (an instance whose type id
//! [`common::is_record_type_id`]). Class instances compare by identity.
//! Heap-heap Result `Err` (`pointer | 1`) never equals an `Ok`; two `Err`s
//! compare their payloads.
//!
//! Cyclic graphs use a bijection of the addresses on the walk's current
//! path: a back edge from `a` must reach the `b` it is paired with there
//! (and vice versa), so a 1-cycle is not equal to a 2-cycle. An object
//! reached again off the path (shared, not cyclic: `[t, t]` holding one
//! tuple twice) is compared again, so it equals two equal copies (#2681);
//! a pair already found equal is not walked twice.

use std::collections::{HashMap, HashSet};

use common::Value;

use crate::memory::{Heap, Object};

/// Deep / structural equality for values.
pub fn values_eq(heap: &Heap, a: Value, b: Value) -> bool {
    if let Some(eq) = flat_eq(heap, a, b) {
        return eq;
    }
    values_eq_rec(heap, a, b, &mut Walk::default())
}

/// [`values_eq_rec`]'s state: the pairs on the current path, both ways,
/// and the pairs found equal. Every comparison is a conjunction, so a
/// pair found equal under a path pair's assumption stays equal: were the
/// assumption wrong, the whole answer is `false`.
#[derive(Default)]
struct Walk {
    fwd: HashMap<u64, u64>,
    rev: HashMap<u64, u64>,
    equal: HashSet<(u64, u64)>,
}

/// Answer without the cycle maps when no address can be visited twice:
/// identical words, non-heap words, strings, and products of at most one
/// slot that is itself flat (`Some(x)`, `Ok("s")`, `Err(e)`).
/// `None` means the value needs the full walk.
fn flat_eq(heap: &Heap, a: Value, b: Value) -> Option<bool> {
    if a.bits() == b.bits() {
        return Some(true);
    }
    let aa = a.bits();
    let bb = b.bits();
    if aa == 0 || bb == 0 || (aa & 1) != (bb & 1) {
        return Some(false);
    }
    if (aa & 1) != 0 {
        return flat_eq(heap, Value::from(aa & !1), Value::from(bb & !1));
    }
    let (Some(oa), Some(ob)) = (heap.find_object_by_addr(aa), heap.find_object_by_addr(bb)) else {
        return Some(false);
    };
    match (&oa, &ob) {
        (Object::String(ga), Object::String(gb)) => Some(ga.as_ref().data == gb.as_ref().data),
        (Object::Array(_), _) => None,
        _ => match product_slots(&oa, &ob) {
            Some(([], _)) => Some(true),
            Some(([x], [y])) => flat_eq(heap, *x, *y),
            Some(_) => None,
            None => Some(false),
        },
    }
}

fn values_eq_rec(heap: &Heap, a: Value, b: Value, walk: &mut Walk) -> bool {
    if a.bits() == b.bits() {
        return true;
    }
    let aa = a.bits();
    let bb = b.bits();
    if aa == 0 || bb == 0 {
        return false;
    }
    // Heap-heap Result: `Ok` is aligned, `Err` is `pointer | 1`. An `Ok`
    // never equals an `Err`; two `Err`s compare their payloads (runtime
    // strings are not interned, so equal payloads need not share a word).
    if (aa & 1) != (bb & 1) {
        return false;
    }
    if (aa & 1) != 0 {
        return values_eq_rec(heap, Value::from(aa & !1), Value::from(bb & !1), walk);
    }
    if let Some(&mapped) = walk.fwd.get(&aa) {
        return mapped == bb;
    }
    if let Some(&mapped) = walk.rev.get(&bb) {
        return mapped == aa;
    }
    if walk.equal.contains(&(aa, bb)) {
        return true;
    }
    let Some(oa) = heap.find_object_by_addr(aa) else {
        return false;
    };
    let Some(ob) = heap.find_object_by_addr(bb) else {
        return false;
    };
    walk.fwd.insert(aa, bb);
    walk.rev.insert(bb, aa);
    let eq = match (&oa, &ob) {
        (Object::Array(ga), Object::Array(gb)) => {
            let (ea, eb) = (ga.as_ref(), gb.as_ref());
            if ea.len() != eb.len() {
                false
            } else if let (Some(x), Some(y)) = (ea.bytes(), eb.bytes()) {
                x == y
            } else {
                ea.iter().zip(eb.iter()).all(|(x, y)| values_eq_rec(heap, x, y, walk))
            }
        }
        (Object::String(ga), Object::String(gb)) => ga.as_ref().data == gb.as_ref().data,
        _ => match product_slots(&oa, &ob) {
            Some((sa, sb)) => sa.iter().zip(sb).all(|(x, y)| values_eq_rec(heap, *x, *y, walk)),
            None => false,
        },
    };
    walk.fwd.remove(&aa);
    walk.rev.remove(&bb);
    if eq {
        walk.equal.insert((aa, bb));
    }
    eq
}

/// The slots of two products of one shape: enum objects of one tag
/// (variants, or tuples) or records, with the same number of slots. Both
/// instances' type ids must name a record; `==` compares a value only with
/// one of its own static type, so equal counts mean one record type (a
/// host-built record and a compiled one can differ in id alone).
fn product_slots<'a>(a: &'a Object, b: &'a Object) -> Option<(&'a [Value], &'a [Value])> {
    let (sa, sb) = match (a, b) {
        (Object::Enum(ga), Object::Enum(gb)) => {
            let (ea, eb) = (ga.as_ref(), gb.as_ref());
            if ea.tag != eb.tag {
                return None;
            }
            (&ea.payload[..], &eb.payload[..])
        }
        (Object::Instance(ga), Object::Instance(gb)) => {
            let (ia, ib) = (ga.as_ref(), gb.as_ref());
            if !common::is_record_type_id(ia.type_id) || !common::is_record_type_id(ib.type_id) {
                return None;
            }
            (ia.slots(), ib.slots())
        }
        _ => return None,
    };
    (sa.len() == sb.len()).then_some((sa, sb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{EnumPayload, ObjArray, ObjEnum, ObjInstance, ObjString, Object};

    fn set_array_elem(heap: &Heap, addr: u64, index: usize, value: Value) {
        let Some(Object::Array(gc)) = heap.find_object_by_addr(addr) else {
            panic!("expected array at {addr:#x}");
        };
        gc.payload_mut().words_mut()[index] = value;
    }

    #[test]
    fn array_deep_eq_same_contents() {
        let mut heap = Heap::default();
        let (oa, _) = heap.alloc(
            ObjArray::new(vec![Value::from(1_i64), Value::from(2_i64)]),
            Object::Array,
        );
        let (ob, _) = heap.alloc(
            ObjArray::new(vec![Value::from(1_i64), Value::from(2_i64)]),
            Object::Array,
        );
        assert!(values_eq(
            &heap,
            Value::from(oa.addr()),
            Value::from(ob.addr())
        ));
    }

    #[test]
    fn array_deep_ne_different_len() {
        let mut heap = Heap::default();
        let (oa, _) = heap.alloc(
            ObjArray::new(vec![Value::from(1_i64)]),
            Object::Array,
        );
        let (ob, _) = heap.alloc(
            ObjArray::new(vec![Value::from(1_i64), Value::from(2_i64)]),
            Object::Array,
        );
        assert!(!values_eq(
            &heap,
            Value::from(oa.addr()),
            Value::from(ob.addr())
        ));
    }

    /// Heap-heap `Result::Err` words (`pointer | 1`) compare payloads, and
    /// never equal an `Ok` of the same payload.
    #[test]
    fn heap_heap_err_compares_payload() {
        let mut heap = Heap::default();
        let (sa, _) = heap.alloc(ObjString::from("e/x"), Object::String);
        let (sb, _) = heap.alloc(ObjString::from("e/x"), Object::String);
        let (sc, _) = heap.alloc(ObjString::from("other"), Object::String);
        let err = |addr: u64| Value::from(addr | 1);
        assert!(values_eq(&heap, err(sa.addr()), err(sb.addr())));
        assert!(!values_eq(&heap, err(sa.addr()), err(sc.addr())));
        assert!(!values_eq(&heap, err(sa.addr()), Value::from(sb.addr())));
    }

    #[test]
    fn flat_enum_with_array_payload_takes_full_walk() {
        let mut heap = Heap::default();
        let arr = |heap: &mut Heap, v: i64| {
            let (a, _) = heap.alloc(ObjArray::new(vec![Value::from(v)]), Object::Array);
            a.addr()
        };
        let (a1, a2, a3) = (arr(&mut heap, 1), arr(&mut heap, 1), arr(&mut heap, 2));
        let some = |heap: &mut Heap, addr: u64| {
            heap.alloc_enum_value(0, EnumPayload::one(Value::from(addr)))
        };
        let (e1, e2, e3) = (some(&mut heap, a1), some(&mut heap, a2), some(&mut heap, a3));
        assert_eq!(flat_eq(&heap, e1, e2), None, "array payload needs the cycle-aware walk");
        assert!(values_eq(&heap, e1, e2));
        assert!(!values_eq(&heap, e1, e3));

        let (sa, _) = heap.alloc(ObjString::from("x"), Object::String);
        let (sb, _) = heap.alloc(ObjString::from("x"), Object::String);
        let (sa, sb) = (sa.addr(), sb.addr());
        let (oa, ob) = (some(&mut heap, sa), some(&mut heap, sb));
        assert_eq!(flat_eq(&heap, oa, ob), Some(true));
    }

    /// Records compare slot by slot; class instances by identity (#1618).
    #[test]
    fn records_compare_slots_and_classes_identity() {
        let mut heap = Heap::default();
        let (sa, _) = heap.alloc(ObjString::from("q"), Object::String);
        let (sb, _) = heap.alloc(ObjString::from("q"), Object::String);
        let inst = |heap: &mut Heap, id: u32, s: u64, n: i64| {
            let (o, _) = heap.alloc(ObjInstance::with_slots(id, vec![Value::from(n), Value::from(s)]), Object::Instance);
            Value::from(o.addr())
        };
        let rec = common::RECORD_TYPE_ID_BASE;
        let (r1, r2, r3) = (inst(&mut heap, rec, sa.addr(), 1), inst(&mut heap, rec, sb.addr(), 1), inst(&mut heap, rec, sb.addr(), 2));
        assert!(values_eq(&heap, r1, r2));
        assert!(!values_eq(&heap, r1, r3));
        let (c1, c2) = (inst(&mut heap, 1, sa.addr(), 1), inst(&mut heap, 1, sa.addr(), 1));
        assert!(!values_eq(&heap, c1, c2));
        assert!(values_eq(&heap, c1, c1));
    }

    #[test]
    fn string_content_eq() {
        let mut heap = Heap::default();
        let (sa, _) = heap.alloc(ObjString::from("hi"), Object::String);
        let (sb, _) = heap.alloc(ObjString::from("hi"), Object::String);
        assert_ne!(sa.addr(), sb.addr());
        assert!(values_eq(
            &heap,
            Value::from(sa.addr()),
            Value::from(sb.addr())
        ));
    }

    #[test]
    fn tuple_deep_eq() {
        let mut heap = Heap::default();
        let (ta, _) = heap.alloc(ObjEnum::tuple(&[Value::from(3_i64), Value::from(4_i64)], 0), Object::Enum);
        let (tb, _) = heap.alloc(ObjEnum::tuple(&[Value::from(3_i64), Value::from(4_i64)], 0), Object::Enum);
        assert!(values_eq(
            &heap,
            Value::from(ta.addr()),
            Value::from(tb.addr())
        ));
    }

    #[test]
    fn self_loop_arrays_are_equal() {
        let mut heap = Heap::default();
        let (oa, _) = heap.alloc(
            ObjArray::new(vec![Value::from(0_i64)]),
            Object::Array,
        );
        let (ob, _) = heap.alloc(
            ObjArray::new(vec![Value::from(0_i64)]),
            Object::Array,
        );
        set_array_elem(&heap, oa.addr(), 0, Value::from(oa.addr()));
        set_array_elem(&heap, ob.addr(), 0, Value::from(ob.addr()));
        assert!(values_eq(
            &heap,
            Value::from(oa.addr()),
            Value::from(ob.addr())
        ));
    }

    #[test]
    fn one_cycle_ne_two_cycle() {
        // a = [a]  vs  b = [c], c = [b]
        let mut heap = Heap::default();
        let (oa, _) = heap.alloc(
            ObjArray::new(vec![Value::from(0_i64)]),
            Object::Array,
        );
        let (ob, _) = heap.alloc(
            ObjArray::new(vec![Value::from(0_i64)]),
            Object::Array,
        );
        let (oc, _) = heap.alloc(
            ObjArray::new(vec![Value::from(0_i64)]),
            Object::Array,
        );
        set_array_elem(&heap, oa.addr(), 0, Value::from(oa.addr()));
        set_array_elem(&heap, ob.addr(), 0, Value::from(oc.addr()));
        set_array_elem(&heap, oc.addr(), 0, Value::from(ob.addr()));
        assert!(!values_eq(
            &heap,
            Value::from(oa.addr()),
            Value::from(ob.addr())
        ));
    }

    /// One object held twice equals two equal copies: sharing is not a
    /// cycle (#2681).
    #[test]
    fn shared_element_eq_two_copies() {
        let mut heap = Heap::default();
        let (inner, _) = heap.alloc(ObjArray::new(vec![Value::from(7_i64)]), Object::Array);
        let (c1, _) = heap.alloc(ObjArray::new(vec![Value::from(7_i64)]), Object::Array);
        let (c2, _) = heap.alloc(ObjArray::new(vec![Value::from(7_i64)]), Object::Array);
        let shared = Value::from(inner.addr());
        let (oa, _) = heap.alloc(ObjArray::new(vec![shared, shared]), Object::Array);
        let (ob, _) = heap.alloc(ObjArray::new(vec![Value::from(c1.addr()), Value::from(c2.addr())]), Object::Array);
        assert!(values_eq(&heap, Value::from(oa.addr()), Value::from(ob.addr())));
        assert!(values_eq(&heap, Value::from(ob.addr()), Value::from(oa.addr())));
        set_array_elem(&heap, c2.addr(), 0, Value::from(8_i64));
        assert!(!values_eq(&heap, Value::from(oa.addr()), Value::from(ob.addr())));
    }

    #[test]
    fn matching_two_cycles_are_equal() {
        // a=[b], b=[a]  vs  c=[d], d=[c]
        let mut heap = Heap::default();
        let (oa, _) = heap.alloc(
            ObjArray::new(vec![Value::from(0_i64)]),
            Object::Array,
        );
        let (ob, _) = heap.alloc(
            ObjArray::new(vec![Value::from(0_i64)]),
            Object::Array,
        );
        let (oc, _) = heap.alloc(
            ObjArray::new(vec![Value::from(0_i64)]),
            Object::Array,
        );
        let (od, _) = heap.alloc(
            ObjArray::new(vec![Value::from(0_i64)]),
            Object::Array,
        );
        set_array_elem(&heap, oa.addr(), 0, Value::from(ob.addr()));
        set_array_elem(&heap, ob.addr(), 0, Value::from(oa.addr()));
        set_array_elem(&heap, oc.addr(), 0, Value::from(od.addr()));
        set_array_elem(&heap, od.addr(), 0, Value::from(oc.addr()));
        assert!(values_eq(
            &heap,
            Value::from(oa.addr()),
            Value::from(oc.addr())
        ));
    }

    #[test]
    fn boxed_enum_eq_same_tag_and_payload() {
        let mut heap = Heap::default();
        let (payload, _) = heap.alloc(ObjString::from("n"), Object::String);
        let (ok_a, _) = heap.alloc(
            ObjEnum::new(0, EnumPayload::one(Value::from(payload.addr()))),
            Object::Enum,
        );
        let (ok_b, _) = heap.alloc(
            ObjEnum::new(0, EnumPayload::one(Value::from(payload.addr()))),
            Object::Enum,
        );
        assert_ne!(ok_a.addr(), ok_b.addr());
        assert!(values_eq(
            &heap,
            Value::from(ok_a.addr()),
            Value::from(ok_b.addr())
        ));
    }

    #[test]
    fn boxed_enum_ne_ok_vs_err_same_payload() {
        let mut heap = Heap::default();
        let (payload, _) = heap.alloc(ObjString::from("n"), Object::String);
        let member = Value::from(payload.addr());
        let (ok, _) = heap.alloc(
            ObjEnum::new(0, EnumPayload::one(member)),
            Object::Enum,
        );
        let (err, _) = heap.alloc(
            ObjEnum::new(1, EnumPayload::one(member)),
            Object::Enum,
        );
        assert!(!values_eq(
            &heap,
            Value::from(ok.addr()),
            Value::from(err.addr())
        ));
    }

    #[test]
    fn niche_result_ok_ne_err_same_object() {
        let mut heap = Heap::default();
        let (obj, _) = heap.alloc(ObjString::from("n"), Object::String);
        let ok = Value::from(obj.addr());
        let err = Value::from(obj.addr() | 1);
        assert!(values_eq(&heap, ok, ok));
        assert!(values_eq(&heap, err, err));
        assert!(!values_eq(&heap, ok, err));
    }
}
