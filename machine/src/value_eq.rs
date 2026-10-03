//! Structural equality for heap values used by `EQ` / `NEQ`.
//!
//! Immediates and non-aggregate heap objects compare by machine word.
//! Arrays (and nested arrays) compare by length and element-wise recursion.
//! Strings compare by UTF-8 content (interned or not). Tuples compare
//! element-wise like arrays. Boxed `ObjEnum` values compare by tag and payload
//! (so `Result::Ok(x) == Result::Ok(x)` holds when the constructs still box).
//! Heap-heap Result `Err` (`pointer | 1`) never equals an `Ok`; two `Err`s
//! compare their payloads.
//!
//! Cyclic graphs use a bijection of addresses already assumed equal: revisiting
//! `a` must pair with the same `b` (and vice versa). A 1-cycle is therefore not
//! equal to a 2-cycle.

use std::collections::HashMap;

use common::Value;

use crate::memory::{Heap, Member, Object};

/// Deep / structural equality for VM values.
pub fn values_eq(heap: &Heap, a: Value, b: Value) -> bool {
    if let Some(eq) = flat_eq(heap, a, b) {
        return eq;
    }
    let mut fwd = HashMap::new();
    let mut rev = HashMap::new();
    values_eq_rec(heap, a, b, &mut fwd, &mut rev)
}

/// Answer without the cycle maps when no address can be visited twice:
/// identical words, non-heap words, strings, and enums whose payload is at
/// most one word that is itself flat (`Some(x)`, `Ok("s")`, `Err(e)`).
/// `None` means the value needs the full walk.
fn flat_eq(heap: &Heap, a: Value, b: Value) -> Option<bool> {
    if a.raw() == b.raw() {
        return Some(true);
    }
    let aa = a.raw() as u64;
    let bb = b.raw() as u64;
    if aa == 0 || bb == 0 || (aa & 1) != (bb & 1) {
        return Some(false);
    }
    if (aa & 1) != 0 {
        return flat_eq(heap, Value::from(aa & !1), Value::from(bb & !1));
    }
    let (Some(oa), Some(ob)) = (heap.find_object_by_addr(aa), heap.find_object_by_addr(bb)) else {
        return Some(false);
    };
    match (oa, ob) {
        (Object::String(ga), Object::String(gb)) => Some(ga.as_ref().data == gb.as_ref().data),
        (Object::Enum(ga), Object::Enum(gb)) => {
            let (ea, eb) = (ga.as_ref(), gb.as_ref());
            if ea.tag != eb.tag || ea.payload.len() != eb.payload.len() {
                return Some(false);
            }
            match ea.payload.len() {
                0 => Some(true),
                1 => flat_eq(heap, ea.payload[0], eb.payload[0]),
                _ => None,
            }
        }
        (Object::Array(_) | Object::Tuple(_) | Object::Boxed(_), _) => None,
        _ => Some(false),
    }
}

fn values_eq_rec(
    heap: &Heap,
    a: Value,
    b: Value,
    fwd: &mut HashMap<u64, u64>,
    rev: &mut HashMap<u64, u64>,
) -> bool {
    if a.raw() == b.raw() {
        return true;
    }
    let aa = a.raw() as u64;
    let bb = b.raw() as u64;
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
        return values_eq_rec(heap, Value::from(aa & !1), Value::from(bb & !1), fwd, rev);
    }
    if let Some(&mapped) = fwd.get(&aa) {
        return mapped == bb;
    }
    if let Some(&mapped) = rev.get(&bb) {
        return mapped == aa;
    }
    fwd.insert(aa, bb);
    rev.insert(bb, aa);
    let Some(oa) = heap.find_object_by_addr(aa) else {
        return false;
    };
    let Some(ob) = heap.find_object_by_addr(bb) else {
        return false;
    };
    match (oa, ob) {
        (Object::Array(ga), Object::Array(gb)) => {
            let ea = &ga.as_ref().elements();
            let eb = &gb.as_ref().elements();
            if ea.len() != eb.len() {
                return false;
            }
            ea.iter()
                .zip(eb.iter())
                .all(|(x, y)| values_eq_rec(heap, *x, *y, fwd, rev))
        }
        (Object::Tuple(ga), Object::Tuple(gb)) => {
            let ea = &ga.as_ref().elements();
            let eb = &gb.as_ref().elements();
            if ea.len() != eb.len() {
                return false;
            }
            ea.iter()
                .zip(eb.iter())
                .all(|(x, y)| values_eq_rec(heap, *x, *y, fwd, rev))
        }
        (Object::String(ga), Object::String(gb)) => ga.as_ref().data == gb.as_ref().data,
        (Object::Enum(ga), Object::Enum(gb)) => {
            let ea = ga.as_ref();
            let eb = gb.as_ref();
            if ea.tag != eb.tag || ea.payload.len() != eb.payload.len() {
                return false;
            }
            ea.payload
                .iter()
                .zip(eb.payload.iter())
                .all(|(a, b)| values_eq_rec(heap, *a, *b, fwd, rev))
        }
        (Object::Boxed(ga), Object::Boxed(gb)) => {
            members_eq(heap, &ga.as_ref().payload, &gb.as_ref().payload, fwd, rev)
        }
        _ => false,
    }
}

fn members_eq(
    heap: &Heap,
    a: &Member,
    b: &Member,
    fwd: &mut HashMap<u64, u64>,
    rev: &mut HashMap<u64, u64>,
) -> bool {
    match (a, b) {
        (Member::Value(va), Member::Value(vb)) => values_eq_rec(heap, *va, *vb, fwd, rev),
        (Member::Object(oa), Member::Object(ob)) => {
            values_eq_rec(heap, Value::from(oa.addr()), Value::from(ob.addr()), fwd, rev)
        }
        (Member::Value(va), Member::Object(ob)) => {
            values_eq_rec(heap, *va, Value::from(ob.addr()), fwd, rev)
        }
        (Member::Object(oa), Member::Value(vb)) => {
            values_eq_rec(heap, Value::from(oa.addr()), *vb, fwd, rev)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{EnumPayload, ObjArray, ObjEnum, ObjString, ObjTuple, Object};

    fn set_array_elem(heap: &Heap, addr: u64, index: usize, value: Value) {
        let Some(Object::Array(gc)) = heap.find_object_by_addr(addr) else {
            panic!("expected array at {addr:#x}");
        };
        gc.payload_mut().elements_mut()[index] = value;
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
        let (ta, _) = heap.alloc(
            ObjTuple::new(vec![Value::from(3_i64), Value::from(4_i64)]),
            Object::Tuple,
        );
        let (tb, _) = heap.alloc(
            ObjTuple::new(vec![Value::from(3_i64), Value::from(4_i64)]),
            Object::Tuple,
        );
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
