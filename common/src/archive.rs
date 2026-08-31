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
pub const ARCHIVE_MAJOR: u16 = 4;

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
/// 13 — `ArrayPin` / `IndexPin*` / `StoreIndexPin*` for pinned array indexing.
/// 14 — drop leftover TLS (`tls_client_enable` … `tls_alpn_protocol`) and
///      virtual crypto HostInvoke slots; holes collapse. Package IO is
///      `stream_attach` / `stream_park` only. coil-crypto is a `dload` package.
///
/// Major 3: persist [`CStructLayout`] (C align/pad) so packaged / `.hyc`
/// execute can restore `extern struct` layouts. rkyv schema change.
pub const ARCHIVE_MINOR: u16 = 0;

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

/// Persisted C struct layout (SysV-style align and trailing pad).
///
/// Computed once at compile and restored on execute. Field `enc` values are
/// [`crate::encode_tag_operand`] integers.
#[derive(Clone, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct CStructLayout {
    pub name: String,
    /// `(field name, encoded FFI tag operand)`.
    pub fields: Vec<(String, u32)>,
    /// Byte offset of each field (same length as `fields`).
    pub offsets: Vec<u32>,
    pub size: u32,
    pub align: u32,
}

fn align_up(n: u32, align: u32) -> u32 {
    if align <= 1 {
        n
    } else {
        n.div_ceil(align) * align
    }
}

fn c_scalar_size_align(tag: u32) -> Option<(u32, u32)> {
    use crate::ffi::tag as t;
    let ptr = std::mem::size_of::<*const ()>() as u32;
    match tag {
        x if x == t::BOOL || x == t::INT8 || x == t::UINT8 => Some((1, 1)),
        x if x == t::INT16 || x == t::UINT16 => Some((2, 2)),
        x if x == t::INT32 || x == t::UINT32 => Some((4, 4)),
        x if x == t::INT
            || x == t::UINT64
            || x == t::FLOAT
            || x == t::PTR
            || x == t::STRING
            || x == t::CALLBACK =>
        {
            Some((ptr, ptr))
        }
        _ => None,
    }
}

fn field_size_align(enc: u32, prior: &[CStructLayout]) -> Result<(u32, u32), String> {
    let (tag, aux) = crate::ffi::decode_tag_operand(enc);
    if tag == crate::ffi::tag::STRUCT {
        let nested = prior
            .get(aux as usize)
            .ok_or_else(|| format!("unknown nested struct layout id {aux}"))?;
        return Ok((nested.size, nested.align));
    }
    c_scalar_size_align(tag).ok_or_else(|| format!("FFI tag {tag} cannot be a C struct field"))
}

/// Compute C align/pad for `fields` against already-computed `prior` layouts.
pub fn compute_c_struct_layout(
    name: String,
    fields: Vec<(String, u32)>,
    prior: &[CStructLayout],
) -> Result<CStructLayout, String> {
    let mut offsets = Vec::with_capacity(fields.len());
    let mut off = 0u32;
    let mut max_align = 1u32;
    for (_, enc) in &fields {
        let (sz, al) = field_size_align(*enc, prior)?;
        max_align = max_align.max(al);
        off = align_up(off, al);
        offsets.push(off);
        off += sz;
    }
    let size = align_up(off, max_align);
    Ok(CStructLayout {
        name,
        fields,
        offsets,
        size,
        align: max_align,
    })
}

/// Compute layouts for a sequence of `extern struct` defs (declaration order).
pub fn compute_c_struct_layouts(
    defs: impl IntoIterator<Item = (String, Vec<(String, u32)>)>,
) -> Result<Vec<CStructLayout>, String> {
    let mut out = Vec::new();
    for (name, fields) in defs {
        let layout = compute_c_struct_layout(name, fields, &out)?;
        out.push(layout);
    }
    Ok(out)
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
    /// `extern struct` C layouts (align/pad), restored on packaged / `.hyc` execute.
    pub struct_layouts: Vec<CStructLayout>,
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
            struct_layouts: Vec::new(),
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
    fn archive_abi_omits_env_grants() {
        // Exhaustive destructure: grants stay off `.hyc` (no rkyv major).
        let _ = |p: &ArchivedProgram| {
            let ArchivedProgram {
                version,
                static_slot_count,
                constants,
                strings,
                bytecode,
                source_files,
                debug_locs,
                fn_symbols,
                struct_layouts,
            } = p;
            let _ = (
                version,
                static_slot_count,
                constants,
                strings,
                bytecode,
                source_files,
                debug_locs,
                fn_symbols,
                struct_layouts,
            );
        };
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
            struct_layouts: Vec::new(),
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
        assert_eq!(ARCHIVE_MAJOR, 4);
        assert_eq!(ARCHIVE_MINOR, 0);
        assert_eq!(ARCHIVE_VERSION, pack_archive_version(4, 0));
        assert_eq!(format_archive_version(ARCHIVE_VERSION), "4.0");
    }

    #[test]
    fn archive_rejects_older_major() {
        let runtime = ARCHIVE_VERSION;
        assert!(!archive_version_compatible(
            pack_archive_version(1, 99),
            runtime
        ));
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

    #[test]
    fn padded_u8_i32_u8_is_size_12_align_4() {
        use crate::ffi::{encode_tag_operand, tag};
        let fields = vec![
            ("a".into(), encode_tag_operand(tag::UINT8, 0)),
            ("b".into(), encode_tag_operand(tag::INT32, 0)),
            ("c".into(), encode_tag_operand(tag::UINT8, 0)),
        ];
        let layout = compute_c_struct_layout("Padded".into(), fields, &[]).unwrap();
        assert_eq!(layout.offsets, vec![0, 4, 8]);
        assert_eq!(layout.size, 12);
        assert_eq!(layout.align, 4);
    }

    #[test]
    fn archive_round_trip_preserves_struct_layouts() {
        use crate::ffi::{encode_tag_operand, tag};

        let layout = compute_c_struct_layout(
            "Padded".into(),
            vec![
                ("a".into(), encode_tag_operand(tag::UINT8, 0)),
                ("b".into(), encode_tag_operand(tag::INT32, 0)),
                ("c".into(), encode_tag_operand(tag::UINT8, 0)),
            ],
            &[],
        )
        .unwrap();
        let program = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![DebugLoc::unknown()],
            fn_symbols: Vec::new(),
            struct_layouts: vec![layout.clone()],
        };
        let bytes = rkyv::to_bytes::<Error>(&program).expect("serialize");
        let archived =
            rkyv::access::<ArchivedArchivedProgram, Error>(bytes.as_slice()).expect("access");
        let back: ArchivedProgram =
            rkyv::deserialize::<ArchivedProgram, Error>(archived).expect("deserialize");
        assert_eq!(back.struct_layouts, vec![layout]);
        assert_eq!(back.struct_layouts[0].offsets, vec![0, 4, 8]);
        assert_eq!(back.struct_layouts[0].size, 12);
        assert_eq!(back.struct_layouts[0].align, 4);
    }

    #[test]
    fn nested_struct_aligns_to_inner_max() {
        use crate::ffi::{encode_tag_operand, tag};
        let inner = compute_c_struct_layout(
            "Inner".into(),
            vec![("x".into(), encode_tag_operand(tag::INT, 0))],
            &[],
        )
        .unwrap();
        assert_eq!(inner.size, 8);
        assert_eq!(inner.align, 8);
        let outer = compute_c_struct_layout(
            "Outer".into(),
            vec![
                ("a".into(), encode_tag_operand(tag::UINT8, 0)),
                ("inner".into(), encode_tag_operand(tag::STRUCT, 0)),
            ],
            std::slice::from_ref(&inner),
        )
        .unwrap();
        assert_eq!(outer.offsets, vec![0, 8]);
        assert_eq!(outer.size, 16);
        assert_eq!(outer.align, 8);
    }
}
