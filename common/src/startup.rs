//! What a program does before `main`, in order.

use rkyv::{Archive, Deserialize, Serialize};

/// One step of a program's startup. `entry` names a body.
#[derive(Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize, Debug)]
#[rkyv(compare(PartialEq))]
pub enum StartupOp {
    /// Call the body for what it does.
    Call { entry: u32 },
    /// Call the body and store its word in static `slot`.
    Init { entry: u32, slot: u32 },
    /// Restore the snapshot at `at` in the constant pool.
    Restore { at: u32 },
}
