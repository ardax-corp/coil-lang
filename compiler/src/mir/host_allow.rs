//! Precise HostInvoke types (math / packed LA / axpy) plus I6 edges.
//!
//! LICM hoist uses purity bits ([`super::effects::host_may_hoist`]), not
//! these id ranges. MIR lowering types these HostInvoke edges by them
//! (box → call → unbox), including Q9 R2 `from_bytes` / `to_bytes`.

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

/// Type a HostInvoke for SSA (I6). W4 keeps precise float/heap specs;
/// other natives are `i64` Value words. `None` if the id is unknown or
/// arity is too wide for a word edge.
pub fn host_edge_spec(id: u16) -> Option<HostSpec> {
    if let Some(spec) = host_spec(id) {
        return Some(spec);
    }
    if embedder_native(id) {
        // Its arity is the checker's, as the call site already agreed;
        // every argument is a Value word ([`host_variadic`]).
        return Some(HostSpec {
            id,
            name: "embedder native",
            args: I64_0,
            ret: I64,
        });
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

/// A native taking more arguments than its declared arity: a thread spawn
/// passes the function's own arguments after it.
pub fn host_variadic(id: u16) -> bool {
    embedder_native(id)
        || matches!(
            HOST_NATIVES.get(id as usize).map(|n| n.name),
            Some("thread_spawn" | "thread_spawn_shared")
        )
}

/// A native the embedder registered (`Compiler::register`): its id follows
/// the frozen [`HOST_NATIVES`] table.
pub fn embedder_native(id: u16) -> bool {
    id as usize >= HOST_NATIVES.len()
}

/// Whether a HostInvoke of native `id` may pass `n` arguments to a spec
/// of `declared`: exactly, more to a variadic native, or two to
/// `packed_vec_arith` (a negation: the operand and the meta word).
pub fn host_arity_ok(id: u16, declared: usize, n: usize) -> bool {
    n == declared || (host_variadic(id) && n > declared) || (id == PACKED_VEC_ARITH_ID && n == 2)
}

/// Whether `actual` may feed a HostInvoke argument typed `expected`.
/// Generic word edges (no precise [`host_spec`]) take any Value word, and a
/// heap pointer is one; precise math / packed specs stay exact.
pub fn host_arg_ok(id: u16, expected: MirTy, actual: MirTy) -> bool {
    actual == expected
        || ((host_spec(id).is_none() || packed_la(id)) && expected == MirTy::I64 && actual.is_heap_word())
        // A generic word edge takes a `bool` (`set_nodelay(s, true)`) as
        // the Value word it is, as its result may be one.
        || (host_spec(id).is_none() && expected == MirTy::I64 && actual == MirTy::Bool)
        // A packed kernel's scalar operand (`10.0 - a`) is its word, as an
        // embedder native's `float` argument or result is.
        || ((packed_la(id) || embedder_native(id)) && expected == MirTy::I64 && actual == MirTy::F64)
        // `black_box` hands back whatever word it is given.
        || (id == common::BLACK_BOX_ID && expected == MirTy::I64 && actual == MirTy::F64)
}

/// A packed linear-algebra kernel: its operands are heap arrays or tuples
/// (Value words), its result a new array or `dot`'s `int` / `float`,
/// which direct MIR types from the expression.
fn packed_la(id: u16) -> bool {
    (PACKED_DOT_ID..=PACKED_VEC_ARITH_ID).contains(&id)
}

/// Whether a HostInvoke typed `expected` may produce `actual`: as
/// [`host_arg_ok`] (a generic word edge's result may be a `bool`,
/// `match_at`, the `Value` word the native returns).
pub fn host_result_ok(id: u16, expected: MirTy, actual: MirTy) -> bool {
    host_arg_ok(id, expected, actual)
        || (packed_la(id) && matches!(actual, MirTy::I64 | MirTy::F64 | MirTy::HeapRef))
}

/// Host Option/Result layouts the dense edge can reconstruct: the one-word
/// ones and the two-word pair.
pub fn dense_host_layout_ok(layout: u8) -> bool {
    matches!(
        layout as u32,
        common::HOST_ENUM_LAYOUT_BOXED
            | common::HOST_ENUM_LAYOUT_OPTION_NICHE
            | common::HOST_ENUM_LAYOUT_RESULT_NICHE
            | common::HOST_ENUM_LAYOUT_PAIR
    )
}

pub fn host_edge_spec_by_name(name: &str) -> Option<HostSpec> {
    HOST_NATIVES
        .iter()
        .find(|n| n.name == name)
        .and_then(|n| host_edge_spec(n.id))
}
