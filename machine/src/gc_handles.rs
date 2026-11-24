//! Host natives for virtual `gc` (`Root<T>` / `Weak<T>`).
//!
//! See [`GC_WIRING`] for pipeline `HostInvoke` registry names and arities.

use std::cell::Cell;

use common::Value;

use crate::host_enum::pack_option_edge;
use crate::memory::{Heap, Member, ObjRoot, ObjWeak, Object};

fn pack_gc_option(heap: &mut Heap, value: Option<Value>) -> Value {
    pack_option_edge(heap, value).unwrap_or_else(|e| {
        panic!("{e}");
    })
}

fn member_from_value(heap: &Heap, value: Value) -> Member {
    if !value.is_zero()
        && let Some(obj) = heap.find_object_by_addr(value.bits())
    {
        Member::Object(obj)
    } else {
        Member::Value(value)
    }
}

fn member_to_value(m: &Member) -> Value {
    match m {
        Member::Value(v) => *v,
        Member::Object(o) => Value::from(o.addr()),
    }
}

/// `gc::root(v) -> Root<T>` — allocate a strong pin around `v`.
pub fn host_gc_root(heap: &mut Heap, args: &[Value]) -> Value {
    let v = args.first().copied().unwrap_or(Value::from(0i64));
    let (obj, _) = heap.alloc(
        ObjRoot {
            payload: Some(member_from_value(heap, v)),
        },
        Object::Root,
    );
    Value::from(obj.addr())
}

/// `gc::get(root) -> Option<T>` — read the pinned value without releasing the pin.
pub fn host_gc_get(heap: &mut Heap, args: &[Value]) -> Value {
    let handle = args.first().copied().unwrap_or(Value::from(0i64));
    match heap.find_object_by_addr(handle.bits()) {
        Some(Object::Root(gc)) => match &gc.as_ref().payload {
            Some(m) => pack_gc_option(heap, Some(member_to_value(m))),
            None => pack_gc_option(heap, None),
        },
        _ => pack_gc_option(heap, None),
    }
}

/// `gc::unroot(root) -> Option<T>` — take the payload and clear the pin.
pub fn host_gc_unroot(heap: &mut Heap, args: &[Value]) -> Value {
    let handle = args.first().copied().unwrap_or(Value::from(0i64));
    match heap.find_object_by_addr(handle.bits()) {
        Some(Object::Root(gc)) => match gc.payload_mut().payload.take() {
            Some(m) => pack_gc_option(heap, Some(member_to_value(&m))),
            None => pack_gc_option(heap, None),
        },
        _ => pack_gc_option(heap, None),
    }
}

/// `gc::weak(v) -> Weak<T>` — non-rooting handle to `v`.
pub fn host_gc_weak(heap: &mut Heap, args: &[Value]) -> Value {
    let v = args.first().copied().unwrap_or(Value::from(0i64));
    // Its release clears the handle once counts free it (plan step 7).
    if let Member::Object(o) = member_from_value(heap, v) {
        o.set_weak_target();
    }
    let (obj, _) = heap.alloc(
        ObjWeak {
            target: Cell::new(v),
            cleared: Cell::new(false),
        },
        Object::Weak,
    );
    Value::from(obj.addr())
}

/// `gc::upgrade(weak) -> Option<T>` — `Some` while the referent is live.
pub fn host_gc_upgrade(heap: &mut Heap, args: &[Value]) -> Value {
    let handle = args.first().copied().unwrap_or(Value::from(0i64));
    let target = match heap.find_object_by_addr(handle.bits()) {
        Some(Object::Weak(gc)) => {
            let weak = gc.as_ref();
            if weak.cleared.get() {
                None
            } else {
                Some(weak.target.get())
            }
        }
        _ => None,
    };
    match target {
        Some(v) => pack_gc_option(heap, Some(v)),
        None => pack_gc_option(heap, None),
    }
}

/// `gc::heap_bytes() -> int` — managed heap size in bytes (`Heap::size`).
pub fn host_gc_heap_bytes(heap: &mut Heap, _args: &[Value]) -> Value {
    Value::from(heap.size() as i64)
}

/// Registry name for `gc::collect`. HostInvoke runs a full collect via
/// [`crate::HostOp::Collect`], not a heap-only stub.
pub const GC_COLLECT_NATIVE: &str = "gc_collect";

/// Registry name for `gc::register_finalizer`. HostInvoke records
/// `type_id → drop PC` via [`crate::HostOp::RegisterFinalizer`].
pub const GC_REGISTER_FINALIZER_NATIVE: &str = "gc_register_finalizer";

/// Ordinary `gc_*` natives (collect / register_finalizer are HostOp, not here).
///
/// Append-only: keep prior ids stable. Collect and register_finalizer are
/// registered immediately after this table.
pub const GC_WIRING: &[(&str, usize, crate::HostValueFn)] = &[
    ("gc_root", 1, host_gc_root),
    ("gc_unroot", 1, host_gc_unroot),
    ("gc_get", 1, host_gc_get),
    ("gc_weak", 1, host_gc_weak),
    ("gc_upgrade", 1, host_gc_upgrade),
    ("gc_heap_bytes", 0, host_gc_heap_bytes),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn intern(heap: &mut Heap, s: &str) -> Value {
        let gc = heap.intern(s.to_string());
        Value::from(gc.as_ptr() as *mut u8 as u64)
    }

    fn option_payload(heap: &Heap, opt: Value) -> Option<Value> {
        match heap.find_object_by_addr(opt.bits()) {
            Some(Object::Enum(gc)) if gc.as_ref().tag == 1 => {
                Some(gc.as_ref().payload[0])
            }
            Some(Object::Enum(gc)) if gc.as_ref().tag == 0 => None,
            _ => panic!("expected Option"),
        }
    }

    #[test]
    fn heap_bytes_tracks_alloc() {
        let mut heap = Heap::default();
        let before = host_gc_heap_bytes(&mut heap, &[]).as_int();
        let _ = host_gc_root(&mut heap, &[Value::from(1i64)]);
        let after = host_gc_heap_bytes(&mut heap, &[]).as_int();
        assert!(after > before);
    }

    #[test]
    fn get_and_unroot_reject_non_root_handles() {
        let mut heap = Heap::default();
        let s = intern(&mut heap, "not-a-root");
        let o1 = host_gc_get(&mut heap, &[s]);
        assert!(option_payload(&heap, o1).is_none());
        let o2 = host_gc_unroot(&mut heap, &[s]);
        assert!(option_payload(&heap, o2).is_none());
        let o3 = host_gc_get(&mut heap, &[Value::from(0i64)]);
        assert!(option_payload(&heap, o3).is_none());
        let o4 = host_gc_unroot(&mut heap, &[]);
        assert!(option_payload(&heap, o4).is_none());
    }

    #[test]
    fn upgrade_rejects_non_weak_handles() {
        let mut heap = Heap::default();
        let root = host_gc_root(&mut heap, &[Value::from(1i64)]);
        let up = host_gc_upgrade(&mut heap, &[root]);
        match heap.find_object_by_addr(up.bits()) {
            Some(Object::Enum(gc)) => assert_eq!(gc.as_ref().tag, 0, "expected Option::None"),
            _ => panic!("expected Option"),
        }
        let empty = host_gc_upgrade(&mut heap, &[]);
        match heap.find_object_by_addr(empty.bits()) {
            Some(Object::Enum(gc)) => assert_eq!(gc.as_ref().tag, 0),
            _ => panic!("expected Option::None for missing arg"),
        }
    }

    #[test]
    fn get_after_unroot_returns_none() {
        let mut heap = Heap::default();
        let root = host_gc_root(&mut heap, &[Value::from(99i64)]);
        let g = host_gc_get(&mut heap, &[root]);
        assert_eq!(option_payload(&heap, g).unwrap().as_int(), 99);
        let u = host_gc_unroot(&mut heap, &[root]);
        assert_eq!(option_payload(&heap, u).unwrap().as_int(), 99);
        let g2 = host_gc_get(&mut heap, &[root]);
        assert!(option_payload(&heap, g2).is_none());
        let u2 = host_gc_unroot(&mut heap, &[root]);
        assert!(option_payload(&heap, u2).is_none());
    }

    #[test]
    fn gc_wiring_names_and_arities_are_stable() {
        assert_eq!(GC_WIRING.len(), 6);
        let expected = [
            ("gc_root", 1),
            ("gc_unroot", 1),
            ("gc_get", 1),
            ("gc_weak", 1),
            ("gc_upgrade", 1),
            ("gc_heap_bytes", 0),
        ];
        for (i, (name, arity)) in expected.iter().enumerate() {
            assert_eq!(GC_WIRING[i].0, *name);
            assert_eq!(GC_WIRING[i].1, *arity);
        }
    }

    #[test]
    fn collect_and_register_are_host_op_not_stubs() {
        use crate::HostOp;
        let natives = crate::host_natives::build_standard_host_natives(|_, _| {});
        let collect = natives
            .iter()
            .find(|n| n.name() == GC_COLLECT_NATIVE)
            .expect("gc_collect");
        let register = natives
            .iter()
            .find(|n| n.name() == GC_REGISTER_FINALIZER_NATIVE)
            .expect("gc_register_finalizer");
        assert_eq!(collect.host_op(), HostOp::Collect);
        assert_eq!(register.host_op(), HostOp::RegisterFinalizer);
        let mut heap = Heap::default();
        assert!(collect.invoke(&mut heap, &[]).is_err());
        assert!(register
            .invoke(&mut heap, &[Value::from(0i64), Value::from(0i64)])
            .is_err());
    }
}
