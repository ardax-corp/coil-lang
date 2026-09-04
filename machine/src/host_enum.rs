//! Explicit Option/Result pack/unpack at the HostInvoke edge.
//!
//! Host natives construct **one** representation for a call — boxed `ObjEnum`
//! or a pointer niche — instead of allocating a box and silently unwrapping
//! it to a niche. Tag and payload must agree; mismatches return
//! [`HostEnumMismatch`] (fail closed).
//!
//! The layout for a given invoke is the `HostInvoke` operand bits
//! (`common::host_invoke_enum_layout`); the VM installs it for the duration
//! of `NativeFn::invoke` via [`with_host_enum_layout`].

use std::cell::Cell;

use common::{
    host_invoke_enum_layout, Value, HOST_ENUM_LAYOUT_BOXED, HOST_ENUM_LAYOUT_OPTION_NICHE,
    HOST_ENUM_LAYOUT_RESERVED, HOST_ENUM_LAYOUT_RESULT_NICHE,
};

use crate::io::{alloc_option_none, alloc_option_some, alloc_result_err, alloc_result_ok};
use crate::memory::{Heap, Member, Object};

thread_local! {
    static HOST_ENUM_LAYOUT: Cell<u32> = const { Cell::new(HOST_ENUM_LAYOUT_BOXED) };
}

/// How Option/Result crosses one HostInvoke.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostEnumLayout {
    /// Boxed `ObjEnum` (`Option<int>`, mixed Result, nested, default).
    #[default]
    Boxed,
    /// `None` = `0`, `Some` = heap object address.
    OptionNiche,
    /// `Ok` = aligned heap pointer, `Err` = `pointer | 1`.
    ResultNiche,
}

impl HostEnumLayout {
    pub fn from_operand(operand: u32) -> Self {
        Self::from_u32(host_invoke_enum_layout(operand))
    }

    /// Decode operand bits. `0` is Boxed; `1`/`2` are niches.
    ///
    /// Reserved value `3` (and any other code) maps to [`Self::Boxed`] on
    /// purpose: an unknown layout must not select a niche (immediates would
    /// panic in pack). It is not a third niche ABI.
    pub fn from_u32(bits: u32) -> Self {
        match bits {
            HOST_ENUM_LAYOUT_OPTION_NICHE => Self::OptionNiche,
            HOST_ENUM_LAYOUT_RESULT_NICHE => Self::ResultNiche,
            HOST_ENUM_LAYOUT_BOXED => Self::Boxed,
            _ => Self::Boxed,
        }
    }

    pub fn as_u32(self) -> u32 {
        match self {
            Self::Boxed => HOST_ENUM_LAYOUT_BOXED,
            Self::OptionNiche => HOST_ENUM_LAYOUT_OPTION_NICHE,
            Self::ResultNiche => HOST_ENUM_LAYOUT_RESULT_NICHE,
        }
    }
}

/// Tag/payload disagreement at the host Option/Result edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostEnumMismatch(pub &'static str);

impl std::fmt::Display for HostEnumMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "host Option/Result layout mismatch: {}", self.0)
    }
}

/// Install `layout` for the duration of `f` (HostInvoke body).
pub fn with_host_enum_layout<T>(layout: HostEnumLayout, f: impl FnOnce() -> T) -> T {
    HOST_ENUM_LAYOUT.with(|cell| {
        let prev = cell.replace(layout.as_u32());
        let out = f();
        cell.set(prev);
        out
    })
}

/// Layout the current HostInvoke asked the native to construct.
pub fn current_host_enum_layout() -> HostEnumLayout {
    HOST_ENUM_LAYOUT.with(|cell| HostEnumLayout::from_u32(cell.get()))
}

/// Build an Option in `layout` once. Does not allocate a box then unwrap.
pub fn pack_option(
    heap: &mut Heap,
    layout: HostEnumLayout,
    value: Option<Value>,
) -> Result<Value, HostEnumMismatch> {
    match layout {
        HostEnumLayout::Boxed => Ok(match value {
            None => alloc_option_none(heap),
            Some(payload) => alloc_option_some(heap, payload),
        }),
        HostEnumLayout::OptionNiche => match value {
            None => Ok(Value::from(0i64)),
            Some(payload) => pack_option_niche_some(heap, payload),
        },
        HostEnumLayout::ResultNiche => Err(HostEnumMismatch(
            "pack_option called with ResultNiche layout",
        )),
    }
}

/// Read an Option that must already be in `layout`. Boxed Option at a niche
/// boundary is not unwrapped.
pub fn unpack_option(
    heap: &Heap,
    layout: HostEnumLayout,
    value: Value,
) -> Result<Option<Value>, HostEnumMismatch> {
    match layout {
        HostEnumLayout::Boxed => unpack_boxed_option(heap, value),
        HostEnumLayout::OptionNiche => unpack_option_niche(heap, value),
        HostEnumLayout::ResultNiche => Err(HostEnumMismatch(
            "unpack_option called with ResultNiche layout",
        )),
    }
}

/// Build a Result in `layout` once.
pub fn pack_result(
    heap: &mut Heap,
    layout: HostEnumLayout,
    value: Result<Value, Value>,
) -> Result<Value, HostEnumMismatch> {
    match layout {
        HostEnumLayout::Boxed => Ok(match value {
            Ok(payload) => alloc_result_ok(heap, payload),
            Err(payload) => alloc_result_err(heap, payload),
        }),
        HostEnumLayout::ResultNiche => match value {
            Ok(payload) => pack_result_niche_ok(heap, payload),
            Err(payload) => pack_result_niche_err(heap, payload),
        },
        HostEnumLayout::OptionNiche => Err(HostEnumMismatch(
            "pack_result called with OptionNiche layout",
        )),
    }
}

/// Read a Result that must already be in `layout`.
pub fn unpack_result(
    heap: &Heap,
    layout: HostEnumLayout,
    value: Value,
) -> Result<Result<Value, Value>, HostEnumMismatch> {
    match layout {
        HostEnumLayout::Boxed => unpack_boxed_result(heap, value),
        HostEnumLayout::ResultNiche => unpack_result_niche(heap, value),
        HostEnumLayout::OptionNiche => Err(HostEnumMismatch(
            "unpack_result called with OptionNiche layout",
        )),
    }
}

/// Pack using the layout installed by the current HostInvoke.
pub fn pack_option_edge(heap: &mut Heap, value: Option<Value>) -> Result<Value, HostEnumMismatch> {
    pack_option(heap, current_host_enum_layout(), value)
}

/// Pack using the layout installed by the current HostInvoke.
pub fn pack_result_edge(
    heap: &mut Heap,
    value: Result<Value, Value>,
) -> Result<Value, HostEnumMismatch> {
    pack_result(heap, current_host_enum_layout(), value)
}

fn pack_option_niche_some(heap: &Heap, payload: Value) -> Result<Value, HostEnumMismatch> {
    let raw = payload.raw() as u64;
    if raw == 0 {
        return Err(HostEnumMismatch("OptionNiche Some payload is 0"));
    }
    if raw & 1 != 0 {
        return Err(HostEnumMismatch(
            "OptionNiche Some payload has Result Err bit set",
        ));
    }
    if heap.find_object_by_addr(raw).is_none() {
        return Err(HostEnumMismatch(
            "OptionNiche Some payload is not a heap object",
        ));
    }
    Ok(payload)
}

fn unpack_option_niche(heap: &Heap, value: Value) -> Result<Option<Value>, HostEnumMismatch> {
    let raw = value.raw() as u64;
    if raw == 0 {
        return Ok(None);
    }
    if raw & 1 != 0 {
        return Err(HostEnumMismatch(
            "OptionNiche word has Result Err bit set",
        ));
    }
    if heap.find_object_by_addr(raw).is_none() {
        return Err(HostEnumMismatch(
            "OptionNiche Some word is not a heap object",
        ));
    }
    Ok(Some(value))
}

fn unpack_boxed_option(heap: &Heap, value: Value) -> Result<Option<Value>, HostEnumMismatch> {
    let Some(Object::Enum(gc)) = heap.find_object_by_addr(value.raw() as u64) else {
        return Err(HostEnumMismatch("boxed Option is not an ObjEnum"));
    };
    let e = gc.as_ref();
    match e.tag {
        0 => {
            if !e.payload.is_empty() {
                return Err(HostEnumMismatch("Option::None carries a payload"));
            }
            Ok(None)
        }
        1 => {
            if e.payload.len() != 1 {
                return Err(HostEnumMismatch("Option::Some arity is not 1"));
            }
            Ok(Some(member_to_value(&e.payload[0])))
        }
        _ => Err(HostEnumMismatch("boxed Option tag is not None/Some")),
    }
}

fn pack_result_niche_ok(heap: &Heap, payload: Value) -> Result<Value, HostEnumMismatch> {
    let raw = payload.raw() as u64;
    if raw == 0 || raw & 1 != 0 {
        return Err(HostEnumMismatch(
            "ResultNiche Ok payload is not an aligned heap pointer",
        ));
    }
    if heap.find_object_by_addr(raw).is_none() {
        return Err(HostEnumMismatch("ResultNiche Ok payload is not a heap object"));
    }
    Ok(payload)
}

fn pack_result_niche_err(heap: &Heap, payload: Value) -> Result<Value, HostEnumMismatch> {
    let raw = payload.raw() as u64;
    if raw == 0 || raw & 1 != 0 {
        return Err(HostEnumMismatch(
            "ResultNiche Err payload is not an aligned heap pointer",
        ));
    }
    if heap.find_object_by_addr(raw).is_none() {
        return Err(HostEnumMismatch(
            "ResultNiche Err payload is not a heap object",
        ));
    }
    Ok(Value::from(raw | 1))
}

fn unpack_result_niche(heap: &Heap, value: Value) -> Result<Result<Value, Value>, HostEnumMismatch> {
    let raw = value.raw() as u64;
    let err = raw & 1 != 0;
    let addr = raw & !1;
    if addr == 0 {
        return Err(HostEnumMismatch("ResultNiche word is null"));
    }
    if heap.find_object_by_addr(addr).is_none() {
        return Err(HostEnumMismatch(
            "ResultNiche payload is not a heap object",
        ));
    }
    let payload = Value::from(addr);
    if err {
        Ok(Err(payload))
    } else {
        Ok(Ok(payload))
    }
}

fn unpack_boxed_result(heap: &Heap, value: Value) -> Result<Result<Value, Value>, HostEnumMismatch> {
    let Some(Object::Enum(gc)) = heap.find_object_by_addr(value.raw() as u64) else {
        return Err(HostEnumMismatch("boxed Result is not an ObjEnum"));
    };
    let e = gc.as_ref();
    if e.payload.len() != 1 {
        return Err(HostEnumMismatch("boxed Result arity is not 1"));
    }
    let payload = member_to_value(&e.payload[0]);
    match e.tag {
        0 => Ok(Ok(payload)),
        1 => Ok(Err(payload)),
        _ => Err(HostEnumMismatch("boxed Result tag is not Ok/Err")),
    }
}

fn member_to_value(m: &Member) -> Value {
    match m {
        Member::Value(v) => *v,
        Member::Object(o) => Value::from(o.addr()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{EnumPayload, ObjEnum, Object};

    fn intern(heap: &mut Heap, s: &str) -> Value {
        let gc = heap.intern(s.to_string());
        Value::from(gc.as_ptr() as *mut u8 as u64)
    }

    #[test]
    fn pack_option_boxed_and_niche_construct_once() {
        let mut heap = Heap::default();
        let s = intern(&mut heap, "hi");
        let boxed = pack_option(&mut heap, HostEnumLayout::Boxed, Some(s)).unwrap();
        assert!(matches!(
            heap.find_object_by_addr(boxed.raw() as u64),
            Some(Object::Enum(_))
        ));
        assert_eq!(
            unpack_option(&heap, HostEnumLayout::Boxed, boxed).unwrap(),
            Some(s)
        );

        let live = heap.live_object_count();
        let niche = pack_option(&mut heap, HostEnumLayout::OptionNiche, Some(s)).unwrap();
        assert_eq!(niche.raw() as u64, s.raw() as u64);
        assert_eq!(heap.live_object_count(), live, "niche Some must not box");
        assert_eq!(
            unpack_option(&heap, HostEnumLayout::OptionNiche, niche).unwrap(),
            Some(s)
        );

        let none = pack_option(&mut heap, HostEnumLayout::OptionNiche, None).unwrap();
        assert_eq!(none.as_int(), 0);
        assert!(unpack_option(&heap, HostEnumLayout::OptionNiche, none)
            .unwrap()
            .is_none());
    }

    #[test]
    fn unpack_option_niche_refuses_immediate_and_err_bit() {
        let heap = Heap::default();
        assert!(unpack_option(&heap, HostEnumLayout::OptionNiche, Value::from(42i64)).is_err());
        assert!(unpack_option(&heap, HostEnumLayout::OptionNiche, Value::from(1i64)).is_err());
    }

    #[test]
    fn unpack_boxed_option_refuses_tag_payload_disagreement() {
        let mut heap = Heap::default();
        let (none_with_payload, _) = heap.alloc(
            ObjEnum {
                tag: 0,
                payload: EnumPayload::one(Member::Value(Value::from(1i64))),
            },
            Object::Enum,
        );
        assert!(unpack_option(
            &heap,
            HostEnumLayout::Boxed,
            Value::from(none_with_payload.addr())
        )
        .is_err());
        let (bad_tag, _) = heap.alloc(
            ObjEnum {
                tag: 2,
                payload: EnumPayload::empty(),
            },
            Object::Enum,
        );
        assert!(unpack_option(&heap, HostEnumLayout::Boxed, Value::from(bad_tag.addr())).is_err());
        assert!(unpack_option(&heap, HostEnumLayout::Boxed, Value::from(0i64)).is_err());
    }

    #[test]
    fn pack_option_niche_refuses_immediate() {
        let mut heap = Heap::default();
        assert!(pack_option(
            &mut heap,
            HostEnumLayout::OptionNiche,
            Some(Value::from(7i64))
        )
        .is_err());
    }

    #[test]
    fn pack_result_niche_ok_err_and_refuse_box_as_wrong_layout() {
        let mut heap = Heap::default();
        let ok_s = intern(&mut heap, "ok");
        let err_s = intern(&mut heap, "err");
        let live = heap.live_object_count();
        let ok = pack_result(&mut heap, HostEnumLayout::ResultNiche, Ok(ok_s)).unwrap();
        let err = pack_result(&mut heap, HostEnumLayout::ResultNiche, Err(err_s)).unwrap();
        assert_eq!(ok.raw() as u64, ok_s.raw() as u64);
        assert_eq!(err.raw() as u64, err_s.raw() as u64 | 1);
        assert_eq!(heap.live_object_count(), live);
        assert_eq!(
            unpack_result(&heap, HostEnumLayout::ResultNiche, ok).unwrap(),
            Ok(ok_s)
        );
        assert_eq!(
            unpack_result(&heap, HostEnumLayout::ResultNiche, err).unwrap(),
            Err(err_s)
        );

        // Niche word at a boxed boundary is not an ObjEnum — fail closed.
        assert!(unpack_result(&heap, HostEnumLayout::Boxed, ok).is_err());
        assert!(pack_option(&mut heap, HostEnumLayout::ResultNiche, None).is_err());
    }

    #[test]
    fn reserved_layout_3_decodes_as_boxed() {
        assert_eq!(
            HostEnumLayout::from_u32(HOST_ENUM_LAYOUT_RESERVED),
            HostEnumLayout::Boxed
        );
        assert_eq!(HostEnumLayout::from_u32(99), HostEnumLayout::Boxed);
    }

    #[test]
    fn current_layout_follows_with_host_enum_layout() {
        assert_eq!(current_host_enum_layout(), HostEnumLayout::Boxed);
        with_host_enum_layout(HostEnumLayout::OptionNiche, || {
            assert_eq!(current_host_enum_layout(), HostEnumLayout::OptionNiche);
        });
        assert_eq!(current_host_enum_layout(), HostEnumLayout::Boxed);
    }
}
