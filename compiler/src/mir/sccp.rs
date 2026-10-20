//! Sparse conditional constant propagation.

use super::func::MirFunc;

pub fn sccp(_func: &mut MirFunc) -> usize {
    0
}

#[cfg(test)]
#[path = "sccp.tests.rs"]
mod tests;
