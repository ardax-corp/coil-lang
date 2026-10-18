//! GC map gates for bodies emitted from MIR.
//!
//! Every body's GC frame map is its precise frame map
//! (`codegen/precise_frames.rs`), built from its final bytecode. The
//! S2b slot maps once drafted here from MIR live roots are no longer
//! produced; the VM still reads them from older archives that carry them.

use common::Instruction;

use super::func::MirFunc;
use super::inst::MirInst;

/// Whether direct MIR can give its allocating body maps: every allocation
/// is followed by its `GcBarrier`.
pub fn mir_maps_ok(func: &MirFunc) -> bool {
    let mut any = false;
    for block in &func.blocks {
        let mut pending = false;
        for inst in &block.insts {
            match inst {
                MirInst::Alloc { .. } | MirInst::ArrayPush { .. } | MirInst::Format { .. } | MirInst::Stringify { .. } => {
                    if pending {
                        return false;
                    }
                    pending = true;
                }
                MirInst::GcBarrier { .. } => {
                    pending = false;
                    any = true;
                }
                _ if pending => return false,
                _ => {}
            }
        }
        if pending {
            return false;
        }
    }
    any
}

/// Bytecode opcodes that are interpreter GC safepoints (alloc).
pub fn is_alloc_opcode(inst: Instruction) -> bool {
    matches!(
        inst,
        Instruction::MakeArray
            | Instruction::MakeArrayK
            | Instruction::MakeTuple
            | Instruction::MakeTupleK
            | Instruction::MakeEnum
            | Instruction::MakeEnumK
            | Instruction::MakeEnumReturn
            | Instruction::MakeEnumReturnK
            | Instruction::InitTyped
            | Instruction::INIT
            | Instruction::ArrayPush
            | Instruction::DenseMake
            | Instruction::DenseMakeK
            | Instruction::DenseMakeObject
            | Instruction::DenseArrayPush
            | Instruction::FORMAT
            | Instruction::STRINGIFY
            |             Instruction::DictEntries
            | Instruction::MakeDict
            | Instruction::MakeCoro
            | Instruction::MakeFn
            | Instruction::MakePolyFn
            | Instruction::MakePolyFnCapture
    )
}
