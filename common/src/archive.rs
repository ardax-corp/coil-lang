//! Versioned code archive format.
//!
//! `ArchivedProgram::version` is a packed `major.minor` (`u16` each in a `u32`):
//! - **same major**, archive minor ≤ runtime minor → loadable (older archives on newer minor runtimes)
//! - **different major** → never loadable either direction
//! - archive minor **greater** than runtime minor → rejected (needs newer opcodes/layout)
//!
//! Early development uses major `0`. Additive append-only code changes bump the
//! minor; incompatible ABI/layout changes bump the major (and reset minor).

use rkyv::{Archive, Deserialize, Serialize};

use crate::debug::{DebugLine, ProgramDebug, SpanLoc};

/// Archive ABI major. Bump (and reset minor to 0) on incompatible layout/opcode changes.
pub const ARCHIVE_MAJOR: u16 = 8;

/// Archive minor. Bump on an additive change to the format.
pub const ARCHIVE_MINOR: u16 = 1;

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
/// Array element kind only (`MakeArrayK`): every element is
/// a `byte` or `bool`, so the runtime may store the array one byte per
/// element. Runtimes that predate it ignore the stamp (elements stay words).
pub const WORD_BYTE: u8 = 3;

/// Class slot kind only ([`ClassWordKinds`]): the first word of a flat
/// two-word field whose payload is a heap pointer under some tags and a
/// scalar under the rest (`Result<int, string>`). The row's trailer names
/// the slot holding its tag and the tags that make it a pointer
/// ([`class_slot_kinds`]). Runtimes that predate it treat the word as
/// [`WORD_UNKNOWN`] and the trailer as kinds of slots that do not exist.
pub const WORD_TAGGED: u8 = 4;

/// Bytes of one [`WORD_TAGGED`] trailer entry: the tag slot (`u16` LE),
/// then the pointer-tag mask (`u64` LE, bit `t` for tag `t`).
pub const TAGGED_ENTRY_BYTES: usize = 10;

/// Append the trailer entry of a [`WORD_TAGGED`] slot to a class row.
pub fn push_tagged_entry(kinds: &mut Vec<u8>, tag_slot: u16, pointer_tags: u64) {
    kinds.extend_from_slice(&tag_slot.to_le_bytes());
    kinds.extend_from_slice(&pointer_tags.to_le_bytes());
}

/// The kind of each of an instance's `slots` under its class row `kinds`,
/// with every [`WORD_TAGGED`] slot resolved by its current tag to
/// [`WORD_POINTER`] or [`WORD_SCALAR`]. A slot past the row, or a tagged
/// one whose entry is missing or names a slot out of range, is
/// [`WORD_UNKNOWN`].
pub fn class_slot_kinds<'a>(kinds: &'a [u8], slots: &'a [crate::Value]) -> impl Iterator<Item = u8> + 'a {
    let mut walk = ClassSlotKinds::new(kinds, slots.len());
    (0..slots.len()).map(move |_| walk.next_kind(slots))
}

/// [`class_slot_kinds`] one slot at a time. Each step reads the slots only
/// for the tag it needs, so a walk may rewrite the slots it has passed
/// between steps (a tag slot is a scalar, which such a walk leaves alone).
pub struct ClassSlotKinds<'a> {
    kinds: &'a [u8],
    entries: std::slice::Iter<'a, [u8; TAGGED_ENTRY_BYTES]>,
    next: usize,
}

impl<'a> ClassSlotKinds<'a> {
    /// The walk over an instance of `n` slots under class row `kinds`.
    #[inline]
    pub fn new(kinds: &'a [u8], n: usize) -> Self {
        let entries = kinds.get(n..).unwrap_or_default().as_chunks::<TAGGED_ENTRY_BYTES>().0.iter();
        Self { kinds, entries, next: 0 }
    }

    /// The kind of the next slot, its tag (if it has one) read from `slots`.
    #[inline]
    pub fn next_kind(&mut self, slots: &[crate::Value]) -> u8 {
        let i = self.next;
        self.next += 1;
        match self.kinds.get(i).copied() {
            Some(WORD_TAGGED) => self
                .entries
                .next()
                .and_then(|e| {
                    let slot = usize::from(u16::from_le_bytes([e[0], e[1]]));
                    let mask = u64::from_le_bytes(e[2..10].try_into().ok()?);
                    let tag = slots.get(slot)?.bits();
                    Some(if tag < 64 && mask >> tag & 1 == 1 { WORD_POINTER } else { WORD_SCALAR })
                })
                .unwrap_or(WORD_UNKNOWN),
            Some(kind) => kind,
            None => WORD_UNKNOWN,
        }
    }
}

/// A call through a function value of an open type (a `forall` parameter,
/// a local bound from a generic function) passes, as its last dictionary
/// word, one bit per type variable of that type in order of first
/// appearance, and their count at this bit. Bit 63 is clear: an entry's
/// own `kinds` word has its top bit set (its sentinel).
pub const KINDS_CALL_COUNT_SHIFT: u32 = 56;

/// The tag of a [`kinds_from_call`] word: bit 62 set, bit 63 clear, so it
/// is neither an entry's `kinds` word nor a heap address.
const KINDS_FROM_CALL_TAG: u64 = 1 << 62;

/// A generic function value's captured word (`MakePolyFnCapture`) after
/// its `kinds` word (#2506, #2621): set bit `to` of that word when the
/// call's word has bit `from` and counts `n` type variables. The runtime
/// (`machine::apply`) applies it and passes no word for it.
pub const fn kinds_from_call(n: u32, to: u32, from: u32) -> u64 {
    KINDS_FROM_CALL_TAG | (n as u64) << 12 | (to as u64) << 6 | from as u64
}

/// `(n, to, from)` of a [`kinds_from_call`] word.
pub const fn kinds_from_call_parts(word: u64) -> Option<(u32, u32, u32)> {
    if word & !0x3_FFFF != KINDS_FROM_CALL_TAG {
        return None;
    }
    Some(((word >> 12) as u32 & 0x3F, (word >> 6) as u32 & 0x3F, word as u32 & 0x3F))
}

/// The tag of a [`dict_from_call`] word: bits 61 and 62 set, bit 63 clear.
const DICT_FROM_CALL_TAG: u64 = 3 << 61;

/// A generic function value's captured dictionary slot (`MakePolyFnCapture`)
/// the call fills (#2636): the call's dictionary `j`. A call through a
/// function value of an open type passes its type's constraints'
/// dictionaries in the order its `kinds` bits follow, so the value names
/// the one each of its function's constraints is.
pub const fn dict_from_call(j: u32) -> u64 {
    DICT_FROM_CALL_TAG | j as u64
}

/// `j` of a [`dict_from_call`] word.
pub const fn dict_from_call_parts(word: u64) -> Option<u32> {
    if word & !0xFFFF != DICT_FROM_CALL_TAG {
        return None;
    }
    Some(word as u32 & 0xFFFF)
}

/// The tag of a product object: an enum object with no variant, which is
/// how the runtime keeps a tuple (and `()`, its empty one). No enum has
/// this many variants.
pub const PRODUCT_TAG: u32 = u32::MAX;

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

/// Serialized program with constant pool and code.
#[derive(Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct ArchivedProgram {
    /// Packed `major.minor` (`pack_archive_version`); see module docs.
    pub version: u32,
    /// Number of global static slots.
    pub static_slot_count: u32,
    /// The snapshots startup restores.
    pub constants: Vec<u64>,
    /// Interned program string literals: `MirInst::String` indexes this table.
    pub strings: Vec<String>,
    /// Paths in stable order (project-relative when compiled from disk).
    pub source_files: Vec<String>,
    /// Each location the bodies use, sorted (the spans: the inline scopes
    /// are in the MIR section, `common::section_inline_scopes`).
    pub debug_locs: Vec<SpanLoc>,
    /// Line and column of each of [`Self::debug_locs`].
    pub debug_lines: Vec<DebugLine>,
    /// Function names by entry, for panic backtraces.
    pub fn_symbols: Vec<crate::debug::FnDebugSym>,
    /// `extern struct` C layouts (align/pad), restored on packaged / `.ccoil` execute.
    pub struct_layouts: Vec<CStructLayout>,
    /// Per-class field word kinds; empty keeps fields ambiguous.
    pub class_word_kinds: Vec<ClassWordKinds>,
    /// Per static slot word kind (`WORD_*`); empty keeps statics ambiguous.
    pub static_word_kinds: Vec<u8>,
    /// What runs before `main`, in order.
    pub startup: Vec<crate::StartupOp>,
    /// `main`'s body, when the program has one.
    pub main: Option<u32>,
    /// Every body's typed MIR. The bytes are `compiler::native_mir::archive`'s
    /// format, opaque to this crate.
    pub native_mir: Vec<u8>,
}

/// Failed `.ccoil` / embed envelope access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveDecodeError {
    Corrupt,
    Version(u32),
}

impl ArchivedProgram {
    pub fn debug_bundle(&self) -> ProgramDebug {
        ProgramDebug {
            source_files: self.source_files.clone(),
            debug_locs: self.debug_locs.iter().map(|&l| l.into()).collect(),
            fn_symbols: self.fn_symbols.clone(),
            debug_lines: self.debug_lines.clone(),
            inline_scopes: crate::section_inline_scopes(&self.native_mir),
        }
    }

    /// Fill the debug sections from `debug` at `level`.
    pub fn set_debug(&mut self, debug: ProgramDebug, level: crate::DebugLevel) {
        let debug = debug.with_level(level);
        self.source_files = debug.source_files;
        self.fn_symbols = debug.fn_symbols;
        self.debug_locs = debug.debug_locs.into_iter().map(SpanLoc::from).collect();
        self.debug_lines = debug.debug_lines;
    }
}

/// Read a `.ccoil` blob of this runtime's major and a minor it knows.
pub fn decode_archived_program(buffer: &[u8]) -> Result<ArchivedProgram, ArchiveDecodeError> {
    use rkyv::rancor::Error;

    let program = rkyv::access::<ArchivedArchivedProgram, Error>(buffer)
        .ok()
        .and_then(|archived| rkyv::deserialize::<ArchivedProgram, Error>(archived).ok())
        .ok_or(ArchiveDecodeError::Corrupt)?;
    if !archive_version_compatible(program.version, ARCHIVE_VERSION) {
        return Err(ArchiveDecodeError::Version(program.version));
    }
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_slot_resolves_by_its_tag() {
        // Slots: [name, payload, n, tag]; the payload is a pointer under tag 1.
        let mut kinds = vec![WORD_POINTER, WORD_TAGGED, WORD_SCALAR, WORD_SCALAR];
        push_tagged_entry(&mut kinds, 3, 0b10);
        let at = |tag: i64| {
            let slots = [crate::Value::from(0i64), crate::Value::from(8i64), crate::Value::from(1i64), crate::Value::from(tag)];
            class_slot_kinds(&kinds, &slots).collect::<Vec<_>>()
        };
        assert_eq!(at(1), [WORD_POINTER, WORD_POINTER, WORD_SCALAR, WORD_SCALAR]);
        assert_eq!(at(0), [WORD_POINTER, WORD_SCALAR, WORD_SCALAR, WORD_SCALAR]);
        assert_eq!(at(70), [WORD_POINTER, WORD_SCALAR, WORD_SCALAR, WORD_SCALAR]);
        // No trailer, or a slot past the row: unknown.
        let bare = [WORD_TAGGED];
        let one = [crate::Value::from(1i64), crate::Value::from(1i64)];
        assert_eq!(class_slot_kinds(&bare, &one).collect::<Vec<_>>(), [WORD_UNKNOWN, WORD_UNKNOWN]);
    }

    fn program(version: u32) -> Vec<u8> {
        let p = ArchivedProgram {
            version,
            static_slot_count: 2,
            constants: vec![7],
            strings: vec!["s".to_string()],
            source_files: Vec::new(),
            debug_locs: Vec::new(),
            debug_lines: Vec::new(),
            fn_symbols: Vec::new(),
            struct_layouts: Vec::new(),
            class_word_kinds: Vec::new(),
            static_word_kinds: Vec::new(),
            startup: vec![crate::StartupOp::Init { entry: 3, slot: 1 }],
            main: Some(4),
            native_mir: vec![1, 2],
        };
        rkyv::to_bytes::<rkyv::rancor::Error>(&p).unwrap().to_vec()
    }

    #[test]
    fn an_archive_round_trips() {
        let Ok(p) = decode_archived_program(&program(ARCHIVE_VERSION)) else { panic!("decodes") };
        assert_eq!(p.startup, vec![crate::StartupOp::Init { entry: 3, slot: 1 }]);
        assert_eq!(p.main, Some(4));
        assert_eq!(p.native_mir, vec![1, 2]);
    }

    #[test]
    fn another_major_and_a_newer_minor_are_refused() {
        for version in [pack_archive_version(ARCHIVE_MAJOR - 1, 0), pack_archive_version(ARCHIVE_MAJOR, ARCHIVE_MINOR + 1)] {
            assert!(matches!(decode_archived_program(&program(version)), Err(ArchiveDecodeError::Version(v)) if v == version));
        }
    }

    #[test]
    fn bytes_that_are_no_archive_are_corrupt() {
        assert!(matches!(decode_archived_program(&[0; 8]), Err(ArchiveDecodeError::Corrupt)));
    }
}
