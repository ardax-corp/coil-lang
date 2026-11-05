//! GC map gates for bodies emitted from MIR.
//!
//! Every body's GC frame map is its precise frame map
//! (`codegen/precise_frames.rs`), built from its final bytecode. The
//! S2b slot maps once drafted here from MIR live roots are no longer
//! produced; the VM still reads them from older archives that carry them.

use common::Instruction;

use super::func::MirFunc;
use super::inst::MirInst;
