//! Versioned bytecode archive format.
//!
//! `ArchivedProgram::version` is a packed `major.minor` (`u16` each in a `u32`):
//! - **same major**, archive minor ≤ runtime minor → loadable (older archives on newer minor runtimes)
//! - **different major** → never loadable either direction
//! - archive minor **greater** than runtime minor → rejected (needs newer opcodes/layout)
//!
//! Early development uses major `0`. Additive append-only bytecode changes bump the
//! minor; incompatible ABI/layout changes bump the major (and reset minor).

use rkyv::{Archive, Deserialize, Serialize};

use crate::debug::{DebugLoc, ProgramDebug};

/// Archive ABI major. Bump (and reset minor to 0) on incompatible layout/opcode changes.
pub const ARCHIVE_MAJOR: u16 = 2;

/// Archive ABI minor. Bump on additive, append-only bytecode changes.
///
/// 2 — `BinSlotSlotStore` accepts float ops (ADDF…GEQF, PowF) in its op field.
/// 3 — pointer-niche Option conversion and unary pair representation opcodes.
/// 4 — allocation-free niche Vec host invocation.
/// 5 — source-ordered two-stage float chain storage.
/// 6 — `FloatChainStore` extended descriptor: up to 3 stages, const-pool
///     operands, and `BinSlotSlot` stage0 (bit 63 distinguishes layouts).
/// 7 — `BinSlotSlotConstJmpf`: float BinSlotSlot + pool CONST + CmpJmpf.
/// 8 — `NEGF`: float unary negate (replaces `CONST -1; MULF`).
/// 9 — `InitTyped`: class instances carry a compile-time type id.
/// 10 — `*Jmpt` twins of fused `*Jmpf` (Cmp / BinSlotImm / LogNot /
///     BinSlotSlot / BinSlotSlotConst).
/// 11 — drop removed-regex HostInvoke slots (nine fewer standard natives).
/// 12 — `IndexUnchecked` / `StoreIndexUnchecked` for bounds-proven loops.
pub const ARCHIVE_MINOR: u16 = 12;

/// Packed `ARCHIVE_MAJOR.ARCHIVE_MINOR` stamped into new archives.
pub const ARCHIVE_VERSION: u32 = pack_archive_version(ARCHIVE_MAJOR, ARCHIVE_MINOR);

/// Pack major/minor into the `u32` stored in archives and package trailers.
pub const fn pack_archive_version(major: u16, minor: u16) -> u32 {
    ((major as u32) << 16) | (minor as u32)
}

/// High 16 bits of a packed archive version.
pub const fn archive_major(version: u32) -> u16 {
    (version >> 16) as u16
}

/// Low 16 bits of a packed archive version.
pub const fn archive_minor(version: u32) -> u16 {
    (version & 0xffff) as u16
}

/// Whether `archive` can run on a runtime stamped with `runtime`.
///
/// Requires equal majors and `archive` minor ≤ `runtime` minor.
pub const fn archive_version_compatible(archive: u32, runtime: u32) -> bool {
    archive_major(archive) == archive_major(runtime)
        && archive_minor(archive) <= archive_minor(runtime)
}

/// Human-readable `major.minor` for diagnostics.
pub fn format_archive_version(version: u32) -> String {
    format!("{}.{}", archive_major(version), archive_minor(version))
}

/// Serialized program with constant pool and bytecode.
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct ArchivedProgram {
    /// Packed `major.minor` (`pack_archive_version`); see module docs.
    pub version: u32,
    /// Number of global static slots (`LoadStatic` / `StoreStatic`).
    pub static_slot_count: u32,
    /// Wide immediates (floats, large ints, jump targets, …).
    /// Referenced from `Byte.operands` via pool index or `Byte::POOL_FLAG`.
    pub constants: Vec<u64>,
    /// Interned program string literals. `STRING` operands index this table.
    pub strings: Vec<String>,
    pub bytecode: Vec<Byte>,
    /// Paths in stable order (project-relative when compiled from disk).
    pub source_files: Vec<String>,
    /// One [`DebugLoc`] per bytecode slot after finalize (same length as `bytecode`).
    pub debug_locs: Vec<DebugLoc>,
    /// Function entry symbols for panic backtraces (sorted by `entry_pc`).
    pub fn_symbols: Vec<crate::debug::FnDebugSym>,
}

pub use crate::opcode::Byte;

impl ArchivedProgram {
    pub fn debug_bundle(&self) -> ProgramDebug {
        ProgramDebug {
            source_files: self.source_files.clone(),
            debug_locs: self.debug_locs.clone(),
            fn_symbols: self.fn_symbols.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::DebugLoc;
    use crate::opcode::{Byte, Instruction};
    use rkyv::rancor::Error;

    #[test]
    fn byte_layout_is_eight_bytes() {
        use std::mem::{align_of, size_of};
        assert_eq!(
            size_of::<Byte>(),
            8,
            "Byte must be 8 bytes for archive layout"
        );
        assert_eq!(align_of::<Byte>(), 4);
        assert_eq!(size_of::<Instruction>(), 1);
    }

    #[test]
    fn archive_round_trip_preserves_bytecode_and_constants() {
        let program = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: 0,
            constants: vec![1.5f64.to_bits(), 42],
            strings: vec!["hi".into()],
            bytecode: vec![
                Byte::new(Instruction::CONST).with_const_inline(7),
                Byte::new(Instruction::STRING).with_operand_u32(0),
                Byte::new(Instruction::HALT),
            ],
            source_files: vec!["main.hy".into()],
            debug_locs: vec![
                DebugLoc {
                    file: 0,
                    start_byte: 0,
                    end_byte: 4,
                },
                DebugLoc::unknown(),
                DebugLoc::unknown(),
            ],
            fn_symbols: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<Error>(&program).expect("serialize");
        let archived =
            rkyv::access::<ArchivedArchivedProgram, Error>(bytes.as_slice()).expect("access");
        assert_eq!(u32::from(archived.version), ARCHIVE_VERSION);
        let back: ArchivedProgram =
            rkyv::deserialize::<ArchivedProgram, Error>(archived).expect("deserialize");
        assert!(back == program);
        assert_eq!(back.source_files, program.source_files);
        assert_eq!(back.debug_locs, program.debug_locs);
    }

    #[test]
    fn archive_round_trip_preserves_fn_symbols() {
        use crate::debug::FnDebugSym;

        let program = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec!["main.hy".into()],
            debug_locs: vec![DebugLoc::unknown()],
            fn_symbols: vec![
                FnDebugSym {
                    name: "main".into(),
                    entry_pc: 0,
                },
                FnDebugSym {
                    name: "helper".into(),
                    entry_pc: 4,
                },
            ],
        };
        let bytes = rkyv::to_bytes::<Error>(&program).expect("serialize");
        let archived =
            rkyv::access::<ArchivedArchivedProgram, Error>(bytes.as_slice()).expect("access");
        let back: ArchivedProgram =
            rkyv::deserialize::<ArchivedProgram, Error>(archived).expect("deserialize");
        assert_eq!(back.fn_symbols, program.fn_symbols);
        let bundle = back.debug_bundle();
        assert_eq!(bundle.fn_symbols.len(), 2);
        assert_eq!(bundle.fn_symbols[0].name, "main");
        assert_eq!(bundle.fn_symbols[1].entry_pc, 4);
    }

    #[test]
    fn archive_version_matches_current_abi() {
        assert_eq!(ARCHIVE_MAJOR, 2);
        assert_eq!(ARCHIVE_MINOR, 12);
        assert_eq!(ARCHIVE_VERSION, pack_archive_version(2, 12));
        assert_eq!(format_archive_version(ARCHIVE_VERSION), "2.12");
    }

    #[test]
    fn archive_rejects_older_major() {
        let runtime = ARCHIVE_VERSION;
        assert!(!archive_version_compatible(pack_archive_version(1, 99), runtime));
    }

    #[test]
    fn archive_version_compatible_within_major() {
        let runtime = pack_archive_version(2, 0);
        assert!(archive_version_compatible(
            pack_archive_version(2, 0),
            runtime
        ));
        assert!(!archive_version_compatible(
            pack_archive_version(2, 1),
            runtime
        ));
        assert!(!archive_version_compatible(
            pack_archive_version(1, 3),
            runtime
        ));
        assert!(!archive_version_compatible(
            pack_archive_version(0, 99),
            runtime
        ));
    }

    #[test]
    fn pack_archive_version_splits_major_minor_bits() {
        let v = pack_archive_version(0xABCD, 0x1234);
        assert_eq!(archive_major(v), 0xABCD);
        assert_eq!(archive_minor(v), 0x1234);
        assert_eq!(format_archive_version(v), "43981.4660");
        // Equal major with older minor is accepted; reverse is not.
        assert!(archive_version_compatible(
            pack_archive_version(7, 1),
            pack_archive_version(7, 9)
        ));
        assert!(!archive_version_compatible(
            pack_archive_version(7, 9),
            pack_archive_version(7, 1)
        ));
    }
}
