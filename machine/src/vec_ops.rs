//! Host natives for builtin `Vec<T>` methods (pop/insert/remove/clear/…).
//!
//! Push uses the existing `ArrayPush` opcode; construction uses `MakeArray`.
//! Runtime representation is [`crate::memory::ObjArray`] (same as fixed
//! arrays until stack multi-slot lands).

use common::Value;

use crate::host_enum::pack_option_edge;
use crate::memory::{Heap, ObjArray, Object};

fn pack_vec_option(heap: &mut Heap, value: Option<Value>) -> Value {
    pack_option_edge(heap, value).unwrap_or_else(|e| {
        panic!("{e}");
    })
}

/// `Vec::with_capacity(n) -> Vec<T>` — empty growable array with reserved capacity.
pub fn host_vec_with_capacity(heap: &mut Heap, args: &[Value]) -> Value {
    let n = args.first().map(|v| v.as_int()).unwrap_or(0).max(0) as usize;
    let elements = Vec::with_capacity(n);
    let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
    Value::from(obj.addr())
}

/// `v.capacity() -> int`
pub fn host_vec_capacity(heap: &mut Heap, args: &[Value]) -> Value {
    let handle = args.first().copied().unwrap_or(Value::from(0i64));
    let cap = match heap.find_object_by_addr(handle.raw() as u64) {
        Some(Object::Array(gc)) => gc.as_ref().elements.capacity(),
        _ => 0,
    };
    Value::from(cap as i64)
}

/// `v.reserve(additional) -> ()` — ensure capacity for `len + additional`.
pub fn host_vec_reserve(heap: &mut Heap, args: &[Value]) -> Value {
    let handle = args.first().copied().unwrap_or(Value::from(0i64));
    let extra = args.get(1).map(|v| v.as_int()).unwrap_or(0).max(0) as usize;
    if let Some(Object::Array(mut gc)) = heap.find_object_by_addr(handle.raw() as u64) {
        let old_bytes = gc.as_ref().elements.capacity() * std::mem::size_of::<Value>();
        gc.as_mut().elements.reserve(extra);
        let new_bytes = gc.as_ref().elements.capacity() * std::mem::size_of::<Value>();
        if old_bytes != new_bytes {
            heap.account_resize(old_bytes, new_bytes);
        }
    }
    Value::from(0i64)
}

/// `v.clear() -> ()`
pub fn host_vec_clear(heap: &mut Heap, args: &[Value]) -> Value {
    let handle = args.first().copied().unwrap_or(Value::from(0i64));
    if let Some(Object::Array(mut gc)) = heap.find_object_by_addr(handle.raw() as u64) {
        gc.as_mut().elements.clear();
    }
    Value::from(0i64)
}

/// `v.pop() -> Option<T>`
pub fn host_vec_pop(heap: &mut Heap, args: &[Value]) -> Value {
    let handle = args.first().copied().unwrap_or(Value::from(0i64));
    match heap.find_object_by_addr(handle.raw() as u64) {
        Some(Object::Array(mut gc)) => match gc.as_mut().elements.pop() {
            Some(v) => pack_vec_option(heap, Some(v)),
            None => pack_vec_option(heap, None),
        },
        _ => pack_vec_option(heap, None),
    }
}

/// `v.insert(i, x) -> ()` — panics when `i` is out of range (not clamped).
pub fn host_vec_insert(heap: &mut Heap, args: &[Value]) -> Result<Value, &'static str> {
    let handle = args.first().copied().unwrap_or(Value::from(0i64));
    let index = args.get(1).map(|v| v.as_int()).unwrap_or(0);
    let value = args.get(2).copied().unwrap_or(Value::from(0i64));
    let Some(Object::Array(mut gc)) = heap.find_object_by_addr(handle.raw() as u64) else {
        return Err("Vec::insert on non-array");
    };
    let old_bytes = gc.as_ref().elements.capacity() * std::mem::size_of::<Value>();
    let len = gc.as_ref().elements.len();
    if index < 0 || (index as usize) > len {
        return Err("index out of bounds");
    }
    gc.as_mut().elements.insert(index as usize, value);
    let new_bytes = gc.as_ref().elements.capacity() * std::mem::size_of::<Value>();
    if old_bytes != new_bytes {
        heap.account_resize(old_bytes, new_bytes);
    }
    Ok(Value::from(0i64))
}

/// `v.remove(i) -> Option<T>`
pub fn host_vec_remove(heap: &mut Heap, args: &[Value]) -> Value {
    let handle = args.first().copied().unwrap_or(Value::from(0i64));
    let index = args.get(1).map(|v| v.as_int()).unwrap_or(-1);
    match heap.find_object_by_addr(handle.raw() as u64) {
        Some(Object::Array(mut gc)) => {
            let len = gc.as_ref().elements.len();
            if index < 0 || (index as usize) >= len {
                pack_vec_option(heap, None)
            } else {
                let v = gc.as_mut().elements.remove(index as usize);
                pack_vec_option(heap, Some(v))
            }
        }
        _ => pack_vec_option(heap, None),
    }
}

/// Copy a fixed array into a fresh growable vec (`Vec::from`).
pub fn host_vec_from_array(heap: &mut Heap, args: &[Value]) -> Value {
    let handle = args.first().copied().unwrap_or(Value::from(0i64));
    let elements = match heap.find_object_by_addr(handle.raw() as u64) {
        Some(Object::Array(gc)) => gc.as_ref().elements.clone(),
        _ => Vec::new(),
    };
    let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
    Value::from(obj.addr())
}

/// Append-only HostInvoke wiring for Vec helpers.
pub const VEC_WIRING: &[(&str, usize, fn(&mut Heap, &[Value]) -> Value)] = &[
    ("vec_with_capacity", 1, host_vec_with_capacity),
    ("vec_capacity", 1, host_vec_capacity),
    ("vec_reserve", 2, host_vec_reserve),
    ("vec_clear", 1, host_vec_clear),
    ("vec_pop", 1, host_vec_pop),
    ("vec_insert", 3, host_vec_insert_unused),
    ("vec_remove", 2, host_vec_remove),
    ("vec_from_array", 1, host_vec_from_array),
];

fn host_vec_insert_unused(_heap: &mut Heap, _args: &[Value]) -> Value {
    unreachable!("vec_insert is special-cased in host_natives::push_wiring")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Object;

    fn make_array(heap: &mut Heap, elems: &[i64]) -> Value {
        let elements: Vec<Value> = elems.iter().copied().map(Value::from).collect();
        let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
        Value::from(obj.addr())
    }

    fn array_ints(heap: &Heap, v: Value) -> Vec<i64> {
        match heap.find_object_by_addr(v.raw() as u64) {
            Some(Object::Array(gc)) => gc.as_ref().elements.iter().map(|e| e.as_int()).collect(),
            _ => panic!("expected array"),
        }
    }

    fn option_tag(heap: &Heap, v: Value) -> u32 {
        match heap.find_object_by_addr(v.raw() as u64) {
            Some(Object::Enum(gc)) => gc.as_ref().tag,
            _ => panic!("expected Option enum"),
        }
    }

    fn option_some_int(heap: &Heap, v: Value) -> i64 {
        use crate::memory::Member;
        match heap.find_object_by_addr(v.raw() as u64) {
            Some(Object::Enum(gc)) => {
                assert_eq!(gc.as_ref().tag, 1, "expected Option::Some");
                match gc.as_ref().payload[0] {
                    Member::Value(inner) => inner.as_int(),
                    Member::Object(_) => panic!("expected int Option payload"),
                }
            }
            _ => panic!("expected Option enum"),
        }
    }

    #[test]
    fn with_capacity_and_reserve_raise_capacity() {
        let mut heap = Heap::default();
        let v = host_vec_with_capacity(&mut heap, &[Value::from(8i64)]);
        assert!(host_vec_capacity(&mut heap, &[v]).as_int() >= 8);
        host_vec_reserve(&mut heap, &[v, Value::from(64i64)]);
        assert!(host_vec_capacity(&mut heap, &[v]).as_int() >= 64);
    }

    #[test]
    #[test]
    fn pop_heap_item_under_option_niche_does_not_box() {
        use crate::host_enum::{with_host_enum_layout, HostEnumLayout};
        let mut heap = Heap::default();
        let s = {
            let gc = heap.intern("x".to_string());
            Value::from(gc.as_ptr() as *mut u8 as u64)
        };
        let (obj, _) = heap.alloc(
            ObjArray {
                elements: vec![s],
            },
            Object::Array,
        );
        let handle = Value::from(obj.addr());
        let live = heap.live_object_count();
        let some = with_host_enum_layout(HostEnumLayout::OptionNiche, || {
            host_vec_pop(&mut heap, &[handle])
        });
        assert_eq!(some.raw() as u64, s.raw() as u64);
        assert_eq!(heap.live_object_count(), live, "niche pop must not alloc Option");
        let none = with_host_enum_layout(HostEnumLayout::OptionNiche, || {
            host_vec_pop(&mut heap, &[handle])
        });
        assert_eq!(none.as_int(), 0);
    }

    #[test]
    fn pop_empty_is_none_and_nonempty_is_some() {
        let mut heap = Heap::default();
        let v = make_array(&mut heap, &[7, 8]);
        let some = host_vec_pop(&mut heap, &[v]);
        assert_eq!(option_some_int(&heap, some), 8);
        assert_eq!(array_ints(&heap, v), vec![7]);
        let _ = host_vec_pop(&mut heap, &[v]);
        let none = host_vec_pop(&mut heap, &[v]);
        assert_eq!(option_tag(&heap, none), 0);
    }

    #[test]
    fn insert_in_range_and_rejects_oob() {
        let mut heap = Heap::default();
        let v = make_array(&mut heap, &[2, 3]);
        host_vec_insert(
            &mut heap,
            &[v, Value::from(0i64), Value::from(1i64)],
        )
        .expect("insert at 0");
        host_vec_insert(
            &mut heap,
            &[v, Value::from(3i64), Value::from(4i64)],
        )
        .expect("append at len");
        assert_eq!(array_ints(&heap, v), vec![1, 2, 3, 4]);
        assert!(
            host_vec_insert(&mut heap, &[v, Value::from(-1i64), Value::from(0i64)]).is_err()
        );
        assert!(
            host_vec_insert(&mut heap, &[v, Value::from(99i64), Value::from(0i64)]).is_err()
        );
    }

    #[test]
    fn remove_oob_and_invalid_handle_are_none() {
        let mut heap = Heap::default();
        let v = make_array(&mut heap, &[10]);
        let neg = host_vec_remove(&mut heap, &[v, Value::from(-1i64)]);
        assert_eq!(option_tag(&heap, neg), 0);
        let oob = host_vec_remove(&mut heap, &[v, Value::from(3i64)]);
        assert_eq!(option_tag(&heap, oob), 0);
        let bad = host_vec_remove(&mut heap, &[Value::from(0i64), Value::from(0i64)]);
        assert_eq!(option_tag(&heap, bad), 0);
    }

    #[test]
    fn clear_and_from_array_copy() {
        let mut heap = Heap::default();
        let src = make_array(&mut heap, &[1, 2]);
        let dst = host_vec_from_array(&mut heap, &[src]);
        host_vec_clear(&mut heap, &[dst]);
        assert_eq!(array_ints(&heap, dst), Vec::<i64>::new());
        assert_eq!(array_ints(&heap, src), vec![1, 2]);
        let empty = host_vec_from_array(&mut heap, &[Value::from(0i64)]);
        assert_eq!(array_ints(&heap, empty), Vec::<i64>::new());
    }

    #[test]
    fn wiring_names_are_stable() {
        let names: Vec<&str> = VEC_WIRING.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(
            names,
            [
                "vec_with_capacity",
                "vec_capacity",
                "vec_reserve",
                "vec_clear",
                "vec_pop",
                "vec_insert",
                "vec_remove",
                "vec_from_array",
            ]
        );
    }
}
