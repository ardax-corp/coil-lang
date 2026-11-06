//! Shared types for the coil compiler and runtime.

mod archive;
mod array_vec;
mod builtins;
mod caps;
mod debug;
mod effects;
pub mod ext;
mod ffi;
mod host;
mod interner;
pub mod int_arith;
mod lane;
mod package;
mod seekable_iter;
mod snapshot;
mod source_pos;
mod startup;
mod value;

pub use archive::*;
pub use array_vec::*;
pub use builtins::*;
pub use caps::{Caps, open_mode_caps};
pub use debug::*;
pub use startup::StartupOp;
pub use effects::EffectFlags;
pub use ffi::tag;
pub use ffi::*;
pub use host::*;
pub use interner::*;
pub use lane::*;
pub use package::*;
pub use seekable_iter::*;
pub use snapshot::*;
pub use source_pos::*;
pub use value::*;

#[macro_export]
macro_rules! promise {
    ($cond: expr) => {
        #[cfg(debug_assertions)]
        {
            debug_assert!($cond);
        }
        #[cfg(not(debug_assertions))]
        {
            unsafe { std::hint::assert_unchecked($cond) }
        }
    };
    ($cond: expr, $msg: literal) => {
        #[cfg(debug_assertions)]
        {
            debug_assert!($cond, $msg);
        }
        #[cfg(not(debug_assertions))]
        {
            unsafe { std::hint::assert_unchecked($cond) }
        }
    };
}

#[inline(always)]
#[cold]
fn cold() {}

#[inline(always)]
pub fn likely(b: bool) -> bool {
    {
        if !b {
            cold()
        }

        b
    }
}

#[inline(always)]
pub fn unlikely(b: bool) -> bool {
    if b {
        cold()
    }

    b
}
