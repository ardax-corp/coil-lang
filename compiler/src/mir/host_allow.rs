//! W4 allowlist plus I6 typed HostInvoke edges.
//!
//! Dense specialize still emits only pure math, packed LA, and
//! `coil-simd` `simd_axpy_reduce`. [`host_edge_spec`] types any catalog
//! native as Value words so impure IO / clocks / GC / FFI can sit on SSA
//! as barriers. User `CALL` is a separate path (COI-291) when the callee
//! is already dense. At a W4 edge, dense emit boxes typed slots onto the
//! Value stack, `HostInvoke`s, then unboxes.

use common::{
    HOST_NATIVES, MATH_ATAN_ID, MATH_POW_ID, MATH_SIN_ID, MATH_TANH_ID, PACKED_DOT_ID,
    PACKED_VEC_ARITH_ID, SIMD_AXPY_REDUCE_ID,
};

use super::ty::MirTy;

/// Type and hoist rules for one allowlisted HostInvoke.
#[derive(Clone, Copy, Debug)]
pub struct HostSpec {
    pub id: u16,
    pub name: &'static str,
    pub args: &'static [MirTy],
    pub ret: MirTy,
    /// Scalar-pure: LICM may hoist (math + saxpy-reduce). Packed LA stays.
    pub hoistable: bool,
}

const F64: MirTy = MirTy::F64;
const I64: MirTy = MirTy::I64;

const MATH1: &[MirTy] = &[F64];
const MATH2: &[MirTy] = &[F64, F64];
const AXPY: &[MirTy] = &[I64, F64, F64, F64, F64];
const HEAP3: &[MirTy] = &[I64, I64, I64];
const HEAP2: &[MirTy] = &[I64, I64];
const I64_0: &[MirTy] = &[];
const I64_1: &[MirTy] = &[I64];
const I64_2: &[MirTy] = &[I64, I64];
const I64_3: &[MirTy] = &[I64, I64, I64];
const I64_4: &[MirTy] = &[I64, I64, I64, I64];
const I64_5: &[MirTy] = &[I64, I64, I64, I64, I64];
const I64_6: &[MirTy] = &[I64, I64, I64, I64, I64, I64];

fn i64_args(arity: u8) -> Option<&'static [MirTy]> {
    Some(match arity {
        0 => I64_0,
        1 => I64_1,
        2 => I64_2,
        3 => I64_3,
        4 => I64_4,
        5 => I64_5,
        6 => I64_6,
        _ => return None,
    })
}

/// Look up a dense-allowlisted HostInvoke. `None` → infer/lower refuse.
pub fn host_spec(id: u16) -> Option<HostSpec> {
    let (args, ret, hoistable) = match id {
        MATH_SIN_ID..=MATH_POW_ID | MATH_ATAN_ID..=MATH_TANH_ID => {
            let arity = HOST_NATIVES.get(id as usize)?.arity;
            let args = match arity {
                1 => MATH1,
                2 => MATH2,
                _ => return None,
            };
            (args, F64, true)
        }
        SIMD_AXPY_REDUCE_ID => (AXPY, F64, true),
        PACKED_DOT_ID => (HEAP3, F64, false),
        common::PACKED_MATMUL_ID | common::PACKED_MATRIX_ZIP_ID | PACKED_VEC_ARITH_ID => {
            (HEAP3, I64, false)
        }
        common::PACKED_MATRIX_NEG_ID => (HEAP2, I64, false),
        _ => return None,
    };
    let name = HOST_NATIVES.get(id as usize)?.name;
    Some(HostSpec {
        id,
        name,
        args,
        ret,
        hoistable,
    })
}

pub fn host_spec_by_name(name: &str) -> Option<HostSpec> {
    HOST_NATIVES
        .iter()
        .find(|n| n.name == name)
        .and_then(|n| host_spec(n.id))
}

/// Type a HostInvoke for SSA (I6). W4 keeps precise float/heap specs;
/// other natives are `i64` Value words. `None` if the id is unknown or
/// arity is too wide for a word edge.
pub fn host_edge_spec(id: u16) -> Option<HostSpec> {
    if let Some(spec) = host_spec(id) {
        return Some(spec);
    }
    let native = HOST_NATIVES.get(id as usize)?;
    if native.id != id {
        return None;
    }
    let args = i64_args(native.arity)?;
    Some(HostSpec {
        id,
        name: native.name,
        args,
        ret: I64,
        hoistable: false,
    })
}

pub fn host_edge_spec_by_name(name: &str) -> Option<HostSpec> {
    HOST_NATIVES
        .iter()
        .find(|n| n.name == name)
        .and_then(|n| host_edge_spec(n.id))
}

/// Documented W4 set: packed LA **87–91**, frozen math **102–110**,
/// M1 math **125–135**, `simd_axpy_reduce` **136**.
pub fn allowlisted_host_ids() -> impl Iterator<Item = u16> {
    (PACKED_DOT_ID..=PACKED_VEC_ARITH_ID)
        .chain(MATH_SIN_ID..=MATH_POW_ID)
        .chain(MATH_ATAN_ID..=MATH_TANH_ID)
        .chain(std::iter::once(SIMD_AXPY_REDUCE_ID))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_matches_host_natives() {
        let ids: Vec<u16> = allowlisted_host_ids().collect();
        assert_eq!(
            ids,
            vec![
                87, 88, 89, 90, 91, 102, 103, 104, 105, 106, 107, 108, 109, 110, 125, 126, 127,
                128, 129, 130, 131, 132, 133, 134, 135, 136
            ]
        );
        for id in ids {
            let spec = host_spec(id).expect("listed id");
            let native = &HOST_NATIVES[id as usize];
            assert_eq!(spec.name, native.name);
            assert_eq!(spec.args.len(), native.arity as usize);
            assert_eq!(spec.id, native.id);
        }
        assert!(host_spec(common::CLOCK_SLEEP_MS_ID).is_none());
        assert!(host_spec(common::RESULT_UNIT_PROBE_ID).is_none());
        // I4/I6: string bytes stay off W4 dense; they are typed I6 edges.
        assert!(host_spec_by_name("from_bytes").is_none());
        assert!(host_spec_by_name("to_bytes").is_none());
        let clock = host_edge_spec(common::CLOCK_MONO_NANOS_ID).expect("clock edge");
        assert!(clock.args.is_empty());
        assert_eq!(clock.ret, MirTy::I64);
        assert!(!clock.hoistable);
        assert!(host_edge_spec_by_name("write").is_some());
        assert!(host_edge_spec_by_name("from_bytes").is_some());
    }
}
