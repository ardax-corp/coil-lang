//! Structural equality for heap values used by `EQ` / `NEQ`.
//!
//! Immediates and non-aggregate heap objects compare by machine word.
//! Arrays (and nested arrays) compare by length and element-wise recursion.
//! Strings compare by UTF-8 content (interned or not). Tuples compare
//! element-wise like arrays. Boxed `ObjEnum` values compare by tag and payload
//! (so `Result::Ok(x) == Result::Ok(x)` holds when the constructs still box).
//! Heap-heap Result `Err` (`pointer | 1`) is not the same word as `Ok`.
//!
//! Cyclic graphs use a bijection of addresses already assumed equal: revisiting
//! `a` must pair with the same `b` (and vice versa). A 1-cycle is therefore not
//! equal to a 2-cycle.

use std::collections::HashMap;

use common::Value;

use crate::memory::{Heap, Member, Object};

/// Deep / structural equality for VM values.
pub fn values_eq(heap: &Heap, a: Value, b: Value) -> bool {
    let mut fwd = HashMap::new();
    let mut rev = HashMap::new();
    values_eq_rec(heap, a, b, &mut fwd, &mut rev)
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
    // Heap-heap Result: `Ok` is aligned, `Err` is `pointer | 1`.
    // Tagged words are equal only when the raw words match (handled above).
    if (aa & 1) != (bb & 1) || (aa & 1) != 0 {
        return false;
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
            let ea = &ga.as_ref().elements;
            let eb = &gb.as_ref().elements;
            if ea.len() != eb.len() {
                return false;
            }
            ea.iter()
                .zip(eb.iter())
                .all(|(x, y)| values_eq_rec(heap, *x, *y, fwd, rev))
        }
        (Object::Tuple(ga), Object::Tuple(gb)) => {
            let ea = &ga.as_ref().elements;
            let eb = &gb.as_ref().elements;
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
                .all(|(ma, mb)| members_eq(heap, ma, mb, fwd, rev))
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
    use crate::memory::{EnumPayload, Member, ObjArray, ObjEnum, ObjString, ObjTuple, Object};

    fn set_array_elem(heap: &Heap, addr: u64, index: usize, value: Value) {
        let Some(Object::Array(gc)) = heap.find_object_by_addr(addr) else {
            panic!("expected array at {addr:#x}");
        };
        gc.payload_mut().elements[index] = value;
    }

    #[test]
    fn array_deep_eq_same_contents() {
        let mut heap = Heap::default();
        let (oa, _) = heap.alloc(
            ObjArray {
                elements: vec![Value::from(1_i64), Value::from(2_i64)],
            },
            Object::Array,
        );
        let (ob, _) = heap.alloc(
            ObjArray {
                elements: vec![Value::from(1_i64), Value::from(2_i64)],
            },
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
            ObjArray {
                elements: vec![Value::from(1_i64)],
            },
            Object::Array,
        );
        let (ob, _) = heap.alloc(
            ObjArray {
                elements: vec![Value::from(1_i64), Value::from(2_i64)],
            },
            Object::Array,
        );
        assert!(!values_eq(
            &heap,
            Value::from(oa.addr()),
            Value::from(ob.addr())
        ));
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
            ObjTuple {
                elements: vec![Value::from(3_i64), Value::from(4_i64)],
            },
            Object::Tuple,
        );
        let (tb, _) = heap.alloc(
            ObjTuple {
                elements: vec![Value::from(3_i64), Value::from(4_i64)],
            },
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
            ObjArray {
                elements: vec![Value::from(0_i64)],
            },
            Object::Array,
        );
        let (ob, _) = heap.alloc(
            ObjArray {
                elements: vec![Value::from(0_i64)],
            },
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
            ObjArray {
                elements: vec![Value::from(0_i64)],
            },
            Object::Array,
        );
        let (ob, _) = heap.alloc(
            ObjArray {
                elements: vec![Value::from(0_i64)],
            },
            Object::Array,
        );
        let (oc, _) = heap.alloc(
            ObjArray {
                elements: vec![Value::from(0_i64)],
            },
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
            ObjArray {
                elements: vec![Value::from(0_i64)],
            },
            Object::Array,
        );
        let (ob, _) = heap.alloc(
            ObjArray {
                elements: vec![Value::from(0_i64)],
            },
            Object::Array,
        );
        let (oc, _) = heap.alloc(
            ObjArray {
                elements: vec![Value::from(0_i64)],
            },
            Object::Array,
        );
        let (od, _) = heap.alloc(
            ObjArray {
                elements: vec![Value::from(0_i64)],
            },
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
            ObjEnum {
                tag: 0,
                payload: EnumPayload::one(Member::Value(Value::from(payload.addr()))),
            },
            Object::Enum,
        );
        let (ok_b, _) = heap.alloc(
            ObjEnum {
                tag: 0,
                payload: EnumPayload::one(Member::Value(Value::from(payload.addr()))),
            },
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
        let member = Member::Value(Value::from(payload.addr()));
        let (ok, _) = heap.alloc(
            ObjEnum {
                tag: 0,
                payload: EnumPayload::one(member),
            },
            Object::Enum,
        );
        let (err, _) = heap.alloc(
            ObjEnum {
                tag: 1,
                payload: EnumPayload::one(member),
            },
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
