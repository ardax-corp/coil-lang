//! Precise HostInvoke types (math / packed LA / axpy) plus I6 edges.
//!
//! LICM hoist uses purity bits ([`super::effects::host_may_hoist`]), not
//! these id ranges. S3 dense emit reconstructs I6-typed HostInvoke edges
//! (box → call → unbox), including Q9 R2 `from_bytes` / `to_bytes`. User `CALL`
//! is one-word (dense map or open fuse-IL / LIR callee).

use common::{
    HOST_NATIVES, MATH_ATAN_ID, MATH_POW_ID, MATH_SIN_ID, MATH_TANH_ID, PACKED_DOT_ID,
    PACKED_VEC_ARITH_ID, SIMD_AXPY_REDUCE_ID,
};

use super::ty::MirTy;

/// Typed HostInvoke edge (precise math / packed / axpy, or I6 i64 words).
#[derive(Clone, Copy, Debug)]
pub struct HostSpec {
    pub id: u16,
    pub name: &'static str,
    pub args: &'static [MirTy],
    pub ret: MirTy,
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

/// Precise types for math / packed LA / axpy. Other natives use
/// [`host_edge_spec`] (i64 Value words).
pub fn host_spec(id: u16) -> Option<HostSpec> {
    let (args, ret) = match id {
        MATH_SIN_ID..=MATH_POW_ID | MATH_ATAN_ID..=MATH_TANH_ID => {
            let arity = HOST_NATIVES.get(id as usize)?.arity;
            let args = match arity {
                1 => MATH1,
                2 => MATH2,
                _ => return None,
            };
            (args, F64)
        }
        SIMD_AXPY_REDUCE_ID => (AXPY, F64),
        PACKED_DOT_ID => (HEAP3, F64),
        common::PACKED_MATMUL_ID | common::PACKED_MATRIX_ZIP_ID | PACKED_VEC_ARITH_ID => {
            (HEAP3, I64)
        }
        common::PACKED_MATRIX_NEG_ID => (HEAP2, I64),
        _ => return None,
    };
    let name = HOST_NATIVES.get(id as usize)?.name;
    Some(HostSpec {
        id,
        name,
        args,
        ret,
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
    })
}

/// Q9 R2 string-bytes HostInvoke (`from_bytes` / `to_bytes`).
pub fn is_i4_bytes_host(id: u16) -> bool {
    matches!(
        HOST_NATIVES.get(id as usize).map(|n| n.name),
        Some("from_bytes" | "to_bytes")
    )
}

/// S3 / Q9 R2: any I6-typed host may sit in a dense body (box at the edge).
pub fn dense_host_ok(id: u16) -> bool {
    host_edge_spec(id).is_some()
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
        // Q9 R2: string bytes stay off W4 float specs; they are I6 word edges.
        assert!(host_spec_by_name("from_bytes").is_none());
        assert!(host_spec_by_name("to_bytes").is_none());
        let from = host_edge_spec_by_name("from_bytes").expect("from_bytes I6");
        let to = host_edge_spec_by_name("to_bytes").expect("to_bytes I6");
        assert!(dense_host_ok(from.id));
        assert!(dense_host_ok(to.id));
        assert!(is_i4_bytes_host(from.id));
        assert!(is_i4_bytes_host(to.id));
        assert_eq!(from.args, I64_1);
        assert_eq!(to.args, I64_1);
        assert_eq!(from.ret, MirTy::I64);
        assert_eq!(to.ret, MirTy::I64);
        let clock = host_edge_spec(common::CLOCK_MONO_NANOS_ID).expect("clock edge");
        assert!(clock.args.is_empty());
        assert_eq!(clock.ret, MirTy::I64);
        assert!(!crate::mir::effects::host_may_hoist(common::CLOCK_MONO_NANOS_ID));
        assert!(host_edge_spec_by_name("write").is_some());
    }
}
