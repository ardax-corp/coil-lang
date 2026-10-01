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

use crate::debug::{DebugLine, DebugLoc, ProgramDebug};

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
/// Major 4 / minor 0: current opcode set after the major-4 reset.
/// 1 — `InitTyped` packs field count with type_id; typed instances use
///     dense slots; `LoadField`/`SetField` index those slots.
/// 2 — `HostInvoke` operand bits `[17:16]` name the Option/Result host-edge
///     layout (`0` boxed, `1` Option pointer-niche, `2` heap-heap Result).
///     Natives construct that shape once; no new opcode.
/// 3 — append HostInvoke `clock_wall_nanos` / `clock_mono_nanos` /
///     `clock_sleep_ms` after `stream_park`. Leftover virtual-time stubs stay
///     panic slots so the time block does not slide.
/// 4 — drop the unused HostInvoke after `vec_from_array`; `stream_attach` /
///     `stream_park` / `clock_*` / `result_unit_probe` ids collapse by one.
/// 5 — append HostInvoke `math_atan` … `math_tanh` after `result_unit_probe`
///     (inverse trig, log10/log2/cbrt, rem, hyperbolic). Frozen math
///     **102–110** is unchanged.
/// 6 — MIR dense numeric opcodes (`DenseBin` … `DenseCast`). Value ABI at
///     CALL/RETURN edges; specialized float/i32 loops only.
/// 7 — HostInvoke `simd_axpy_reduce` (**136**) for MIR saxpy-reduce packs
///     (COI-286). Workspace `coil-simd` kernels; no new opcodes.
/// 8 — compiler-only SIMD opcodes (`VLoad` / `VStore` / `VBin` / `VMove`).
///     Eight numeric lanes; execution via `coil-simd`. HostInvoke packs stay.
/// 9 — compiler-only `VReduce` / `VFma` (S5b V1). Horizontal left-fold
///     add and mul-then-add FMA; `coil-simd` lanes; no fast-math.
/// 10 — dense-native heap ops (COI-335): `DenseIndex` / `DenseStoreIndex`
///     / `DenseArrayLen` / `DenseMake` / `DensePush`.
/// 11 — dense-native `Vec` grow (COI-344 B6): `DenseArrayPush`.
/// 12 — dense-native field / Object make (COI-356 D2): `DenseFieldLoad`
///      / `DenseFieldStore` / `DenseMakeObject`.
/// 13 — persist analyzed `operand_stack_slots` (COI-358 E0). Older
///      envelopes omit the field; loaders fall back to the Seek+CALL
///      heuristic. Not an opcode / IPA change.
/// 14 — persist S2b stack maps (COI-359 E1). Older envelopes load with
///      empty maps (conservative stack GC). Not an opcode / IPA change.
/// 15 — HostInvoke `thread_spawn_shared` (**137**) for C1 shared-heap
///      loop-chunk steal (COI-365 E6). Isolate `thread_spawn` is unchanged.
/// 16 — `DenseBin2` (COI-381 S2): two-word pack of consecutive `DenseBin`
///      (first packing in the opcode word; payload word is the second
///      `DenseBin`). Sequential IEEE; not FMA / `FloatChainStore`.
/// 17 — `DenseBinJmpf` (COI-377 S1): two-word pack of `DenseBin` then a
///      fused `*Jmpf`/`*Jmpt` payload (mandelbrot inner mag + GTF).
/// 18 — `DenseIndexJmpf` (COI-379 S4): two-word pack of `DenseIndex` then a
///      fused `*Jmpf`/`*Jmpt` payload (nsieve p-loop `flags[p] == 1`).
/// 19 — `MakeEnumReturn` (COI-388 X3): fuse-select `MakeEnum; RETURN` for a
///      one-word heap enum (`binary_trees` `bottom_up`). Same packing as
///      `MakeEnum`. Not two-word `RETURN`.
/// 20 — HostInvoke `stream_fd` (**138**) for `Stream.fd()` so packages can
///      pass a real fd to FFI after COI-234 removed silent Stream→Int.
/// 21 — persist [`crate::PreciseFrameMap`]s: frames whose heap words are
///      fully described skip the conservative stack scan. Older envelopes
///      load with none (every frame conservative). Not an opcode change.
/// 22 — `PreciseFrameMap::frame_words`: conservative frames scan at least the
///      body's frame extent. Minor-21 rows load with `0` (unknown). Also
///      `DenseCast` kind `CAST_F2I` (`f64` → `i64`).
/// 23 — `TagEnumType`: stamp an enum's type id so its `fn drop()` runs
///      (COI-26). Older archives never emit it.
/// 24 — [`ClassWordKinds`]: per-class field word kinds (scalar / pointer /
///      unknown) from static types. Older archives load with none, so every
///      typed field stays ambiguous.
/// 25 — `MakeEnumK` / `MakeEnumReturnK` / `MakeTupleK` / `DenseMakeK`: enum
///      payloads and tuples carry construction-site word kinds. Older
///      archives never emit them (every payload word stays ambiguous).
/// 26 — `PRECISE_SLOT_MUST` (bit 15) on precise frame-map slots: the word
///      definitely holds a pointer or `0`. Older archives never set it, so
///      every listed slot stays "may hold a heap word".
/// 27 — `TagArrayKind`: arrays built by typed `Vec` constructors carry an
///      element word kind. Older archives never emit it (elements stay
///      ambiguous).
/// 28 — `MakeArrayK`: array literals of ground pointer elements carry the
///      pointer element kind. Older archives never emit it.
/// 29 — [`ArchivedProgram::static_word_kinds`]: per static slot word kind.
///      Older archives load with none (every static stays ambiguous).
/// 30 — [`ArchivedProgram::debug_lines`]: line/column per debug location,
///      resolved at compile time so a packaged binary does not read its
///      source files (#580). Older archives load with none (lines come from
///      the sources, as before).
///
/// Major 3: persist [`CStructLayout`] (C align/pad) so packaged / `.hyc`
/// execute can restore `extern struct` layouts. rkyv schema change.
pub const ARCHIVE_MINOR: u16 = 30;

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

/// A heap word whose static type says nothing (generic, boxed, unresolved):
/// the GC resolves it through the slab.
pub const WORD_UNKNOWN: u8 = 0;
/// A number, bool or scalar enum: never a heap reference.
pub const WORD_SCALAR: u8 = 1;
/// `0` or a heap object address, possibly with bit 0 set (`Result` niche).
pub const WORD_POINTER: u8 = 2;

/// Words whose kinds a packed `u8` records (2 bits each). Later words of a
/// wider payload are unknown.
pub const PACKED_KIND_WORDS: usize = 4;

/// Kind of word `i` in a packed `u8` (see [`PACKED_KIND_WORDS`]).
#[inline]
pub const fn packed_word_kind(kinds: u8, i: usize) -> u8 {
    if i < PACKED_KIND_WORDS {
        (kinds >> (2 * i)) & 3
    } else {
        WORD_UNKNOWN
    }
}

/// Pack per-word kinds (first [`PACKED_KIND_WORDS`]) into a `u8`.
pub fn pack_word_kinds(kinds: impl IntoIterator<Item = u8>) -> u8 {
    kinds
        .into_iter()
        .take(PACKED_KIND_WORDS)
        .enumerate()
        .fold(0, |acc, (i, k)| acc | ((k & 3) << (2 * i)))
}

/// Word kinds of a class's typed fields, in slot order (minor 24+).
#[derive(Clone, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct ClassWordKinds {
    pub type_id: u32,
    /// One of [`WORD_UNKNOWN`] / [`WORD_SCALAR`] / [`WORD_POINTER`] per field.
    pub kinds: Vec<u8>,
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
    /// Compiler-analyzed operand-stack capacity (minor 13+).
    pub operand_stack_slots: u32,
    /// S2b slot / frame maps (minor 14+). Empty means conservative stack GC.
    pub stack_maps: Vec<crate::stack_map::FrameStackMap>,
    /// Complete frame maps (minor 21+); frames without one are scanned.
    pub precise_frames: Vec<crate::stack_map::PreciseFrameMap>,
    /// Per-class field word kinds (minor 24+); empty keeps fields ambiguous.
    pub class_word_kinds: Vec<ClassWordKinds>,
    /// Per static slot word kind (`WORD_*`, minor 29+); empty keeps statics
    /// ambiguous.
    pub static_word_kinds: Vec<u8>,
    /// Line/column per [`Self::debug_locs`] entry (minor 30+); empty means
    /// resolve against the source files.
    pub debug_lines: Vec<DebugLine>,
}

/// Minor 29 envelope (no debug lines).
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct ArchivedProgramV29 {
    pub version: u32,
    pub static_slot_count: u32,
    pub constants: Vec<u64>,
    pub strings: Vec<String>,
    pub bytecode: Vec<Byte>,
    pub source_files: Vec<String>,
    pub debug_locs: Vec<DebugLoc>,
    pub fn_symbols: Vec<crate::debug::FnDebugSym>,
    pub struct_layouts: Vec<CStructLayout>,
    pub operand_stack_slots: u32,
    pub stack_maps: Vec<crate::stack_map::FrameStackMap>,
    pub precise_frames: Vec<crate::stack_map::PreciseFrameMap>,
    pub class_word_kinds: Vec<ClassWordKinds>,
    pub static_word_kinds: Vec<u8>,
}

/// Minor 24–28 envelope (no static word kinds).
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct ArchivedProgramV28 {
    pub version: u32,
    pub static_slot_count: u32,
    pub constants: Vec<u64>,
    pub strings: Vec<String>,
    pub bytecode: Vec<Byte>,
    pub source_files: Vec<String>,
    pub debug_locs: Vec<DebugLoc>,
    pub fn_symbols: Vec<crate::debug::FnDebugSym>,
    pub struct_layouts: Vec<CStructLayout>,
    pub operand_stack_slots: u32,
    pub stack_maps: Vec<crate::stack_map::FrameStackMap>,
    pub precise_frames: Vec<crate::stack_map::PreciseFrameMap>,
    pub class_word_kinds: Vec<ClassWordKinds>,
}

/// Minor 22–23 envelope (no class word kinds).
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct ArchivedProgramV23 {
    pub version: u32,
    pub static_slot_count: u32,
    pub constants: Vec<u64>,
    pub strings: Vec<String>,
    pub bytecode: Vec<Byte>,
    pub source_files: Vec<String>,
    pub debug_locs: Vec<DebugLoc>,
    pub fn_symbols: Vec<crate::debug::FnDebugSym>,
    pub struct_layouts: Vec<CStructLayout>,
    pub operand_stack_slots: u32,
    pub stack_maps: Vec<crate::stack_map::FrameStackMap>,
    pub precise_frames: Vec<crate::stack_map::PreciseFrameMap>,
}

/// Minor-21 envelope (precise frame maps without `frame_words`).
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct ArchivedProgramV21 {
    pub version: u32,
    pub static_slot_count: u32,
    pub constants: Vec<u64>,
    pub strings: Vec<String>,
    pub bytecode: Vec<Byte>,
    pub source_files: Vec<String>,
    pub debug_locs: Vec<DebugLoc>,
    pub fn_symbols: Vec<crate::debug::FnDebugSym>,
    pub struct_layouts: Vec<CStructLayout>,
    pub operand_stack_slots: u32,
    pub stack_maps: Vec<crate::stack_map::FrameStackMap>,
    pub precise_frames: Vec<crate::stack_map::PreciseFrameMapV21>,
}

/// Minor 14–20 envelope (S2b maps, no precise frame maps).
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct ArchivedProgramV20 {
    pub version: u32,
    pub static_slot_count: u32,
    pub constants: Vec<u64>,
    pub strings: Vec<String>,
    pub bytecode: Vec<Byte>,
    pub source_files: Vec<String>,
    pub debug_locs: Vec<DebugLoc>,
    pub fn_symbols: Vec<crate::debug::FnDebugSym>,
    pub struct_layouts: Vec<CStructLayout>,
    pub operand_stack_slots: u32,
    pub stack_maps: Vec<crate::stack_map::FrameStackMap>,
}

/// Minor-13 envelope (`operand_stack_slots`, no S2b maps).
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct ArchivedProgramV13 {
    pub version: u32,
    pub static_slot_count: u32,
    pub constants: Vec<u64>,
    pub strings: Vec<String>,
    pub bytecode: Vec<Byte>,
    pub source_files: Vec<String>,
    pub debug_locs: Vec<DebugLoc>,
    pub fn_symbols: Vec<crate::debug::FnDebugSym>,
    pub struct_layouts: Vec<CStructLayout>,
    pub operand_stack_slots: u32,
}

/// Pre-minor-13 envelope. Loader fallback so older `.hyc` stay readable.
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct ArchivedProgramV12 {
    pub version: u32,
    pub static_slot_count: u32,
    pub constants: Vec<u64>,
    pub strings: Vec<String>,
    pub bytecode: Vec<Byte>,
    pub source_files: Vec<String>,
    pub debug_locs: Vec<DebugLoc>,
    pub fn_symbols: Vec<crate::debug::FnDebugSym>,
    pub struct_layouts: Vec<CStructLayout>,
}

/// Failed `.hyc` / embed envelope access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveDecodeError {
    Corrupt,
    Version(u32),
    /// Envelope decoded but the bytecode failed [`crate::verify_bytecode`].
    Invalid(crate::BytecodeError),
}

/// Owned archive plus which additive fields were on the wire.
pub struct DecodedArchive {
    pub program: ArchivedProgram,
    pub operand_stack_slots_persisted: bool,
    pub stack_maps_persisted: bool,
}

/// Default operand-stack guess for envelopes that omit the analyzed bound.
pub const LEGACY_DEFAULT_OPERAND_STACK_SLOTS: u32 = 256;

/// Hard ceiling matching the VM operand-stack limit (1 048 576 slots).
pub const LEGACY_MAX_OPERAND_STACK_SLOTS: u32 = 1_048_576;

/// First minor that stores [`ArchivedProgram::operand_stack_slots`].
pub const OPERAND_STACK_SLOTS_MINOR: u16 = 13;

/// First minor that stores [`ArchivedProgram::stack_maps`].
pub const STACK_MAPS_MINOR: u16 = 14;

/// First minor that stores [`ArchivedProgram::precise_frames`].
pub const PRECISE_FRAMES_MINOR: u16 = 21;

/// First minor whose precise frame rows carry `frame_words`.
pub const FRAME_WORDS_MINOR: u16 = 22;

/// First minor that stores [`ArchivedProgram::class_word_kinds`].
pub const CLASS_WORD_KINDS_MINOR: u16 = 24;

/// First minor that stores [`ArchivedProgram::static_word_kinds`].
pub const STATIC_WORD_KINDS_MINOR: u16 = 29;

/// First minor that stores [`ArchivedProgram::debug_lines`].
pub const DEBUG_LINES_MINOR: u16 = 30;

pub use crate::opcode::Byte;

impl ArchivedProgram {
    pub fn debug_bundle(&self) -> ProgramDebug {
        ProgramDebug {
            source_files: self.source_files.clone(),
            debug_locs: self.debug_locs.clone(),
            fn_symbols: self.fn_symbols.clone(),
            debug_lines: self.debug_lines.clone(),
        }
    }
}

impl ArchivedProgramV29 {
    fn into_program(self) -> ArchivedProgram {
        ArchivedProgram {
            version: self.version,
            static_slot_count: self.static_slot_count,
            constants: self.constants,
            strings: self.strings,
            bytecode: self.bytecode,
            source_files: self.source_files,
            debug_locs: self.debug_locs,
            fn_symbols: self.fn_symbols,
            struct_layouts: self.struct_layouts,
            operand_stack_slots: self.operand_stack_slots,
            stack_maps: self.stack_maps,
            precise_frames: self.precise_frames,
            class_word_kinds: self.class_word_kinds,
            static_word_kinds: self.static_word_kinds,
            debug_lines: Vec::new(),
        }
    }
}

impl ArchivedProgramV12 {
    fn into_program(self) -> ArchivedProgram {
        ArchivedProgram {
            version: self.version,
            static_slot_count: self.static_slot_count,
            constants: self.constants,
            strings: self.strings,
            bytecode: self.bytecode,
            source_files: self.source_files,
            debug_locs: self.debug_locs,
            fn_symbols: self.fn_symbols,
            struct_layouts: self.struct_layouts,
            operand_stack_slots: 0,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
        }
    }
}

impl ArchivedProgramV21 {
    fn into_program(self) -> ArchivedProgram {
        ArchivedProgram {
            version: self.version,
            static_slot_count: self.static_slot_count,
            constants: self.constants,
            strings: self.strings,
            bytecode: self.bytecode,
            source_files: self.source_files,
            debug_locs: self.debug_locs,
            fn_symbols: self.fn_symbols,
            struct_layouts: self.struct_layouts,
            operand_stack_slots: self.operand_stack_slots,
            stack_maps: self.stack_maps,
            precise_frames: self.precise_frames.into_iter().map(Into::into).collect(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
        }
    }
}

impl ArchivedProgramV28 {
    fn into_program(self) -> ArchivedProgram {
        ArchivedProgram {
            version: self.version,
            static_slot_count: self.static_slot_count,
            constants: self.constants,
            strings: self.strings,
            bytecode: self.bytecode,
            source_files: self.source_files,
            debug_locs: self.debug_locs,
            fn_symbols: self.fn_symbols,
            struct_layouts: self.struct_layouts,
            operand_stack_slots: self.operand_stack_slots,
            stack_maps: self.stack_maps,
            precise_frames: self.precise_frames,
            class_word_kinds: self.class_word_kinds,
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
        }
    }
}

impl ArchivedProgramV23 {
    fn into_program(self) -> ArchivedProgram {
        ArchivedProgram {
            version: self.version,
            static_slot_count: self.static_slot_count,
            constants: self.constants,
            strings: self.strings,
            bytecode: self.bytecode,
            source_files: self.source_files,
            debug_locs: self.debug_locs,
            fn_symbols: self.fn_symbols,
            struct_layouts: self.struct_layouts,
            operand_stack_slots: self.operand_stack_slots,
            stack_maps: self.stack_maps,
            precise_frames: self.precise_frames,
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
        }
    }
}

impl ArchivedProgramV20 {
    fn into_program(self) -> ArchivedProgram {
        ArchivedProgram {
            version: self.version,
            static_slot_count: self.static_slot_count,
            constants: self.constants,
            strings: self.strings,
            bytecode: self.bytecode,
            source_files: self.source_files,
            debug_locs: self.debug_locs,
            fn_symbols: self.fn_symbols,
            struct_layouts: self.struct_layouts,
            operand_stack_slots: self.operand_stack_slots,
            stack_maps: self.stack_maps,
            precise_frames: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
        }
    }
}

impl ArchivedProgramV13 {
    fn into_program(self) -> ArchivedProgram {
        ArchivedProgram {
            version: self.version,
            static_slot_count: self.static_slot_count,
            constants: self.constants,
            strings: self.strings,
            bytecode: self.bytecode,
            source_files: self.source_files,
            debug_locs: self.debug_locs,
            fn_symbols: self.fn_symbols,
            struct_layouts: self.struct_layouts,
            operand_stack_slots: self.operand_stack_slots,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
        }
    }
}

/// Seek+CALL heuristic used when the envelope has no persisted slot count.
pub fn legacy_archive_operand_slots(bytecode: &[Byte]) -> u32 {
    use crate::opcode::Instruction;
    let has_seek = bytecode.iter().any(|b| *b.bytecode() == Instruction::Seek);
    let has_call = bytecode
        .iter()
        .any(|b| matches!(*b.bytecode(), Instruction::CALL | Instruction::TailCall));
    if has_seek && has_call {
        LEGACY_MAX_OPERAND_STACK_SLOTS
    } else {
        LEGACY_DEFAULT_OPERAND_STACK_SLOTS
    }
}

/// Prefer the persisted compiler bound; otherwise the pre-13 heuristic.
pub fn resolve_archive_operand_slots(persisted: Option<u32>, bytecode: &[Byte]) -> u32 {
    match persisted {
        Some(slots) => slots.clamp(1, LEGACY_MAX_OPERAND_STACK_SLOTS),
        None => legacy_archive_operand_slots(bytecode),
    }
}

/// Access a `.hyc` blob. Same major + archive minor ≤ runtime; older
/// envelopes without `operand_stack_slots` / S2b maps still load.
/// Bytecode is verified before it is returned.
pub fn decode_archived_program(buffer: &[u8]) -> Result<DecodedArchive, ArchiveDecodeError> {
    let decoded = decode_envelope(buffer)?;
    let p = &decoded.program;
    crate::verify_bytecode(
        &p.bytecode,
        &p.constants,
        crate::VerifyLimits {
            strings: p.strings.len(),
            static_slots: p.static_slot_count as usize,
        },
    )
    .map_err(ArchiveDecodeError::Invalid)?;
    Ok(decoded)
}

fn decode_envelope(buffer: &[u8]) -> Result<DecodedArchive, ArchiveDecodeError> {
    use rkyv::rancor::Error;

    let current = rkyv::access::<ArchivedArchivedProgram, Error>(buffer)
        .ok()
        .and_then(|archived| rkyv::deserialize::<ArchivedProgram, Error>(archived).ok());
    let v29 = rkyv::access::<ArchivedArchivedProgramV29, Error>(buffer)
        .ok()
        .and_then(|archived| rkyv::deserialize::<ArchivedProgramV29, Error>(archived).ok());
    let v28 = rkyv::access::<ArchivedArchivedProgramV28, Error>(buffer)
        .ok()
        .and_then(|archived| rkyv::deserialize::<ArchivedProgramV28, Error>(archived).ok());
    let v23 = rkyv::access::<ArchivedArchivedProgramV23, Error>(buffer)
        .ok()
        .and_then(|archived| rkyv::deserialize::<ArchivedProgramV23, Error>(archived).ok());
    let v21 = rkyv::access::<ArchivedArchivedProgramV21, Error>(buffer)
        .ok()
        .and_then(|archived| rkyv::deserialize::<ArchivedProgramV21, Error>(archived).ok());
    let v20 = rkyv::access::<ArchivedArchivedProgramV20, Error>(buffer)
        .ok()
        .and_then(|archived| rkyv::deserialize::<ArchivedProgramV20, Error>(archived).ok());
    let v13 = rkyv::access::<ArchivedArchivedProgramV13, Error>(buffer)
        .ok()
        .and_then(|archived| rkyv::deserialize::<ArchivedProgramV13, Error>(archived).ok());
    let v12 = rkyv::access::<ArchivedArchivedProgramV12, Error>(buffer)
        .ok()
        .and_then(|archived| rkyv::deserialize::<ArchivedProgramV12, Error>(archived).ok());

    if let Some(program) = current {
        if archive_version_compatible(program.version, ARCHIVE_VERSION)
            && archive_minor(program.version) >= DEBUG_LINES_MINOR
        {
            return Ok(DecodedArchive {
                program,
                operand_stack_slots_persisted: true,
                stack_maps_persisted: true,
            });
        }
        if !archive_version_compatible(program.version, ARCHIVE_VERSION) {
            return Err(ArchiveDecodeError::Version(program.version));
        }
    }
    if let Some(old) = v29 {
        if archive_version_compatible(old.version, ARCHIVE_VERSION)
            && archive_minor(old.version) >= STATIC_WORD_KINDS_MINOR
        {
            return Ok(DecodedArchive {
                program: old.into_program(),
                operand_stack_slots_persisted: true,
                stack_maps_persisted: true,
            });
        }
        if !archive_version_compatible(old.version, ARCHIVE_VERSION) {
            return Err(ArchiveDecodeError::Version(old.version));
        }
    }
    if let Some(old) = v28 {
        if archive_version_compatible(old.version, ARCHIVE_VERSION)
            && archive_minor(old.version) >= CLASS_WORD_KINDS_MINOR
        {
            return Ok(DecodedArchive {
                program: old.into_program(),
                operand_stack_slots_persisted: true,
                stack_maps_persisted: true,
            });
        }
        if !archive_version_compatible(old.version, ARCHIVE_VERSION) {
            return Err(ArchiveDecodeError::Version(old.version));
        }
    }
    if let Some(old) = v23 {
        if archive_version_compatible(old.version, ARCHIVE_VERSION)
            && archive_minor(old.version) >= FRAME_WORDS_MINOR
        {
            return Ok(DecodedArchive {
                program: old.into_program(),
                operand_stack_slots_persisted: true,
                stack_maps_persisted: true,
            });
        }
        if !archive_version_compatible(old.version, ARCHIVE_VERSION) {
            return Err(ArchiveDecodeError::Version(old.version));
        }
    }
    if let Some(old) = v21 {
        if archive_version_compatible(old.version, ARCHIVE_VERSION)
            && archive_minor(old.version) >= PRECISE_FRAMES_MINOR
        {
            return Ok(DecodedArchive {
                program: old.into_program(),
                operand_stack_slots_persisted: true,
                stack_maps_persisted: true,
            });
        }
        if !archive_version_compatible(old.version, ARCHIVE_VERSION) {
            return Err(ArchiveDecodeError::Version(old.version));
        }
    }
    if let Some(old) = v20 {
        if archive_version_compatible(old.version, ARCHIVE_VERSION)
            && archive_minor(old.version) >= STACK_MAPS_MINOR
        {
            return Ok(DecodedArchive {
                program: old.into_program(),
                operand_stack_slots_persisted: true,
                stack_maps_persisted: true,
            });
        }
        if !archive_version_compatible(old.version, ARCHIVE_VERSION) {
            return Err(ArchiveDecodeError::Version(old.version));
        }
    }
    if let Some(old) = v13 {
        if archive_version_compatible(old.version, ARCHIVE_VERSION)
            && archive_minor(old.version) >= OPERAND_STACK_SLOTS_MINOR
        {
            return Ok(DecodedArchive {
                program: old.into_program(),
                operand_stack_slots_persisted: true,
                stack_maps_persisted: false,
            });
        }
        if !archive_version_compatible(old.version, ARCHIVE_VERSION) {
            return Err(ArchiveDecodeError::Version(old.version));
        }
    }
    if let Some(old) = v12 {
        if archive_version_compatible(old.version, ARCHIVE_VERSION) {
            return Ok(DecodedArchive {
                program: old.into_program(),
                operand_stack_slots_persisted: false,
                stack_maps_persisted: false,
            });
        }
        return Err(ArchiveDecodeError::Version(old.version));
    }
    Err(ArchiveDecodeError::Corrupt)
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
            operand_stack_slots: LEGACY_DEFAULT_OPERAND_STACK_SLOTS,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
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
                operand_stack_slots,
                stack_maps,
                precise_frames,
                class_word_kinds,
                static_word_kinds,
                debug_lines,
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
                operand_stack_slots,
                stack_maps,
                precise_frames,
                class_word_kinds,
                static_word_kinds,
                debug_lines,
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
            operand_stack_slots: LEGACY_DEFAULT_OPERAND_STACK_SLOTS,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
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
        assert_eq!(ARCHIVE_MINOR, 30);
        assert_eq!(ARCHIVE_VERSION, pack_archive_version(4, 30));
        assert_eq!(format_archive_version(ARCHIVE_VERSION), "4.30");
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
            operand_stack_slots: 512,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
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
        assert_eq!(back.operand_stack_slots, 512);
    }

    #[test]
    fn decode_persists_operand_stack_slots_on_current_minor() {
        let program = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![DebugLoc::unknown()],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 512,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<Error>(&program).expect("serialize");
        let decoded = decode_archived_program(bytes.as_slice()).expect("decode");
        assert!(decoded.operand_stack_slots_persisted);
        assert!(decoded.stack_maps_persisted);
        assert_eq!(decoded.program.operand_stack_slots, 512);
        assert!(decoded.program.stack_maps.is_empty());
        assert_eq!(
            resolve_archive_operand_slots(Some(512), &decoded.program.bytecode),
            512
        );
    }

    #[test]
    fn decode_pre13_envelope_omits_operand_stack_slots() {
        let seek = Byte::new(Instruction::Seek).with_operand_u32(25);
        let call = Byte::new(Instruction::CALL);
        let old = ArchivedProgramV12 {
            version: pack_archive_version(4, 12),
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![seek, call, Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![DebugLoc::unknown(); 3],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<Error>(&old).expect("serialize v12");
        assert!(
            rkyv::access::<ArchivedArchivedProgram, Error>(bytes.as_slice()).is_err(),
            "current envelope must not silently read a pre-13 blob"
        );
        let decoded = decode_archived_program(bytes.as_slice()).expect("legacy decode");
        assert!(!decoded.operand_stack_slots_persisted);
        assert!(!decoded.stack_maps_persisted);
        assert!(decoded.program.stack_maps.is_empty());
        assert_eq!(
            resolve_archive_operand_slots(None, &decoded.program.bytecode),
            LEGACY_MAX_OPERAND_STACK_SLOTS
        );
        assert_eq!(
            legacy_archive_operand_slots(&[Byte::new(Instruction::Seek)]),
            LEGACY_DEFAULT_OPERAND_STACK_SLOTS
        );
    }

    #[test]
    fn decode_persists_stack_maps_on_current_minor() {
        use crate::stack_map::{FrameStackMap, SlotMap};

        let maps = vec![FrameStackMap {
            entry_pc: 4,
            end_pc: 20,
            frame_slots: vec![0, 2],
            safepoints: vec![SlotMap {
                pc: 8,
                slots: vec![0],
            }],
        }];
        let program = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![DebugLoc::unknown()],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 256,
            stack_maps: maps.clone(),
            precise_frames: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<Error>(&program).expect("serialize");
        let decoded = decode_archived_program(bytes.as_slice()).expect("decode");
        assert!(decoded.stack_maps_persisted);
        assert_eq!(decoded.program.stack_maps, maps);
    }

    #[test]
    fn decode_persists_precise_frames_on_current_minor() {
        use crate::stack_map::PreciseFrameMap;

        let precise = vec![PreciseFrameMap {
            entry_pc: 4,
            end_pc: 20,
            any_pc: None,
            at_pc: vec![crate::stack_map::SlotMap { pc: 8, slots: vec![1] }],
            frame_words: 6,
        }];
        let program = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![DebugLoc::unknown()],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 256,
            stack_maps: Vec::new(),
            precise_frames: precise.clone(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<Error>(&program).expect("serialize");
        let decoded = decode_archived_program(bytes.as_slice()).expect("decode");
        assert_eq!(decoded.program.precise_frames, precise);
    }

    #[test]
    fn decode_minor21_envelope_leaves_frame_words_unknown() {
        use crate::stack_map::{PreciseFrameMapV21, SlotMap};

        let old = ArchivedProgramV21 {
            version: pack_archive_version(4, 21),
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![DebugLoc::unknown()],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 256,
            stack_maps: Vec::new(),
            precise_frames: vec![PreciseFrameMapV21 {
                entry_pc: 0,
                end_pc: 1,
                any_pc: None,
                at_pc: vec![SlotMap { pc: 0, slots: vec![2] }],
            }],
        };
        let bytes = rkyv::to_bytes::<Error>(&old).expect("serialize v21");
        let decoded = decode_archived_program(bytes.as_slice()).expect("decode v21");
        let row = &decoded.program.precise_frames[0];
        assert_eq!(row.at_pc[0].slots, vec![2]);
        assert_eq!(row.frame_words, 0);
    }

    #[test]
    fn decode_minor23_envelope_has_no_class_word_kinds() {
        let old = ArchivedProgramV23 {
            version: pack_archive_version(4, 23),
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![DebugLoc::unknown()],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 256,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<Error>(&old).expect("serialize v23");
        let decoded = decode_archived_program(bytes.as_slice()).expect("decode v23");
        assert!(decoded.program.class_word_kinds.is_empty());
        assert_eq!(decoded.program.operand_stack_slots, 256);
    }

    #[test]
    fn decode_persists_class_word_kinds_on_current_minor() {
        let program = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![DebugLoc::unknown()],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 256,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
            class_word_kinds: vec![ClassWordKinds {
                type_id: 3,
                kinds: vec![WORD_SCALAR, WORD_POINTER, WORD_UNKNOWN],
            }],
            static_word_kinds: vec![WORD_POINTER, WORD_SCALAR],
            debug_lines: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<Error>(&program).expect("serialize");
        let decoded = decode_archived_program(bytes.as_slice()).expect("decode");
        assert_eq!(decoded.program.class_word_kinds, program.class_word_kinds);
        assert_eq!(decoded.program.static_word_kinds, program.static_word_kinds);
    }

    #[test]
    fn current_archive_round_trips_debug_lines() {
        let program = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT), Byte::new(Instruction::HALT)],
            source_files: vec!["main.hy".into()],
            debug_locs: vec![
                DebugLoc {
                    file: 0,
                    start_byte: 4,
                    end_byte: 9,
                },
                DebugLoc::unknown(),
            ],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 256,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            debug_lines: vec![DebugLine { line: 3, column: 4 }, DebugLine::unknown()],
        };
        let bytes = rkyv::to_bytes::<Error>(&program).expect("serialize");
        let decoded = decode_archived_program(bytes.as_slice()).expect("decode");
        assert_eq!(decoded.program.debug_lines, program.debug_lines);
        let debug = decoded.program.debug_bundle();
        assert_eq!(debug.line_at(0), Some(DebugLine { line: 3, column: 4 }));
        assert_eq!(debug.line_at(1), None);
    }

    #[test]
    fn decode_minor29_envelope_has_no_debug_lines() {
        let old = ArchivedProgramV29 {
            version: pack_archive_version(ARCHIVE_MAJOR, 29),
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec!["main.hy".into()],
            debug_locs: vec![DebugLoc {
                file: 0,
                start_byte: 0,
                end_byte: 1,
            }],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 256,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: vec![WORD_POINTER],
        };
        let bytes = rkyv::to_bytes::<Error>(&old).expect("serialize v29");
        let decoded = decode_archived_program(bytes.as_slice()).expect("decode v29");
        assert!(decoded.program.debug_lines.is_empty());
        assert_eq!(decoded.program.static_word_kinds, vec![WORD_POINTER]);
        // No recorded lines: callers fall back to the source files.
        assert_eq!(decoded.program.debug_bundle().line_at(0), None);
    }

    #[test]
    fn decode_minor28_envelope_has_no_static_word_kinds() {
        let old = ArchivedProgramV28 {
            version: pack_archive_version(ARCHIVE_MAJOR, 28),
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![DebugLoc::unknown()],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 256,
            stack_maps: Vec::new(),
            precise_frames: Vec::new(),
            class_word_kinds: vec![ClassWordKinds {
                type_id: 1,
                kinds: vec![WORD_POINTER],
            }],
        };
        let bytes = rkyv::to_bytes::<Error>(&old).expect("serialize v28");
        let decoded = decode_archived_program(bytes.as_slice()).expect("decode v28");
        assert!(decoded.program.static_word_kinds.is_empty());
        assert_eq!(decoded.program.class_word_kinds.len(), 1);
    }

    #[test]
    fn decode_minor20_envelope_has_no_precise_frames() {
        use crate::stack_map::{FrameStackMap, SlotMap};

        let maps = vec![FrameStackMap {
            entry_pc: 4,
            end_pc: 20,
            frame_slots: vec![0],
            safepoints: vec![SlotMap { pc: 8, slots: vec![0] }],
        }];
        let old = ArchivedProgramV20 {
            version: pack_archive_version(4, 20),
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![DebugLoc::unknown()],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 256,
            stack_maps: maps.clone(),
        };
        let bytes = rkyv::to_bytes::<Error>(&old).expect("serialize v20");
        let decoded = decode_archived_program(bytes.as_slice()).expect("decode v20");
        assert!(decoded.stack_maps_persisted);
        assert_eq!(decoded.program.stack_maps, maps);
        assert!(decoded.program.precise_frames.is_empty());
    }

    #[test]
    fn decode_pre14_envelope_omits_stack_maps() {
        let old = ArchivedProgramV13 {
            version: pack_archive_version(4, 13),
            static_slot_count: 0,
            constants: vec![],
            strings: vec![],
            bytecode: vec![Byte::new(Instruction::HALT)],
            source_files: vec![],
            debug_locs: vec![DebugLoc::unknown()],
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            operand_stack_slots: 512,
        };
        let bytes = rkyv::to_bytes::<Error>(&old).expect("serialize v13");
        assert!(
            rkyv::access::<ArchivedArchivedProgram, Error>(bytes.as_slice()).is_err(),
            "current envelope must not silently read a pre-14 blob"
        );
        let decoded = decode_archived_program(bytes.as_slice()).expect("v13 decode");
        assert!(decoded.operand_stack_slots_persisted);
        assert!(!decoded.stack_maps_persisted);
        assert_eq!(decoded.program.operand_stack_slots, 512);
        assert!(decoded.program.stack_maps.is_empty());
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
