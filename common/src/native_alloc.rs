//! The kind codes of the runtime's `coil_native_alloc`, which both MIR
//! engines pass (the compiler maps each `MirAllocKind` to one).

pub const ALLOC_ARRAY: u64 = 0;
// 1 was a tuple; a tuple is an `ALLOC_ENUM` of `PRODUCT_TAG` since archive major 8.
pub const ALLOC_ENUM: u64 = 2;
pub const ALLOC_OBJECT: u64 = 3;
pub const ALLOC_FN: u64 = 4;
pub const ALLOC_POLY_FN: u64 = 5;
pub const ALLOC_DROP_ENUM: u64 = 6;
pub const ALLOC_CORO: u64 = 7;
pub const ALLOC_LANES: u64 = 8;
/// A lane array of wide elements (3 to [`crate::PACKED_KIND_WORDS`] words
/// each): `extra`'s low byte is the words per element, the bits above it
/// the lanes that are heap words (`ObjArray::tag_mask` of a wide element).
pub const ALLOC_WIDE_LANES: u64 = 9;
