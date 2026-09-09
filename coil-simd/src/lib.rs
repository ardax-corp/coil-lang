//! Stable-Rust SIMD helpers built on [`std::arch`].
//!
//! Coil stays on stable (no `portable_simd`). This crate provides a small,
//! runtime-dispatched surface for dense `f64` / `i64` kernels used by the VM's
//! packed linear-algebra HostInvoke paths.
//!
//! # Feature selection
//!
//! [`detect`] probes the host once and caches the best available
//! [`SimdLevel`]. Kernels then pick an SSE2 / AVX2 / AVX-512 / NEON path or a
//! scalar fallback. Callers never need `RUSTFLAGS` target-cpu flags for
//! correctness; wider ISAs are used only when present at runtime.

pub mod bytes;
mod kernels;
pub mod lanes;
mod level;
pub mod scalar;

#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
mod aarch64;

pub use kernels::{
    axpy_reduce_f64, dot_f64, dot_i64, matmul_f64, matmul_i64, scale_f64, scale_i64, zip_add_f64,
    zip_add_i64, zip_div_f64, zip_mul_f64, zip_mul_i64, zip_neg_f64, zip_neg_i64, zip_sub_f64,
    zip_sub_i64,
};
pub use lanes::LANES;
pub use level::{detect, SimdLevel};
