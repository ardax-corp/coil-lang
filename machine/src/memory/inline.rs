//! What native code's inline allocation writes (`docs/internals/native.md`,
//! "Inline allocation"): a fixed-size nursery object (an enum, a tuple
//! among them, with an inline payload, an instance with inline fields) is bumped from
//! its class's cursor ([`super::super::slab::Bump`]) and filled from an
//! image of its slot, without a call into the runtime.
//!
//! The images are probed from real objects, so they follow the
//! `repr(C)` / `repr(u8)` layouts of [`ObjEnum`], [`ObjInstance`] and
//! their payloads. Those reprs keep the layout the same
//! in the compiler that emits the code and the runtime that runs it, and
//! [`layout_fingerprint`] lets an AOT runtime refuse an object compiled
//! against another layout.

use std::alloc::Layout;
use std::hash::{Hash, Hasher};

use super::*;

/// One fixed-size object native code allocates inline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InlineShape {
    /// `ALLOC_ENUM` of a variant (a tuple: [`common::PRODUCT_TAG`]) with
    /// `n` payload words (1 to [`ENUM_INLINE_ARITY`]), `kinds` their packed
    /// word kinds.
    Enum { tag: u32, kinds: u8, n: u8 },
    /// `ALLOC_OBJECT` of a class with `n` fields (1 to
    /// [`INSTANCE_INLINE_FIELDS`]); its fields stay zero until stored.
    Instance { type_id: u32, n: u8 },
}

/// How to allocate one [`InlineShape`] inline.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InlineObject {
    /// Index of its nursery class's cursor ([`Heap::inline_alloc_ptrs`]).
    pub class: u32,
    /// Bytes the cursor moves by (the slot).
    pub slot: u32,
    /// Bytes the heap accounts for it ([`HeapCounts::alloc_bytes`]).
    pub size: u32,
    /// The slot's words as a fresh young object, header first, with its
    /// payload words zero.
    pub image: Vec<u64>,
    /// Index in `image` of the first payload word: enum payload (tuple)
    /// words go there in order.
    pub words_at: u32,
}

/// The nursery classes of the inline shapes, made first in every heap so
/// their cursor indexes are the same in every build.
fn inline_layouts() -> [Layout; 2] {
    [Layout::new::<GcData<ObjEnum>>(), Layout::new::<GcData<ObjInstance>>()]
}

/// Make the inline shapes' nursery classes (a new heap, first).
pub(super) fn make_inline_classes(slab: &mut Slab) -> [usize; 2] {
    inline_layouts().map(|l| slab.young_class(l))
}

/// Offset of `field` within `base`, both inside one object.
fn off<A, B>(base: &A, field: &B) -> usize {
    std::ptr::from_ref(field) as usize - std::ptr::from_ref(base) as usize
}

/// A payload's discriminant byte, `len`, `kinds` and slot offsets, and its
/// slot words, probed from `p`.
fn payload_probe(p: &EnumPayload) -> (u8, usize, usize, usize, [u64; ENUM_INLINE_ARITY]) {
    let EnumPayloadInner::Inline { len, kinds, slots } = &p.inner else {
        unreachable!("an inline-arity payload is inline");
    };
    // SAFETY: `repr(u8)`: the discriminant is the first byte.
    let tag = unsafe { *std::ptr::from_ref(&p.inner).cast::<u8>() };
    (tag, off(p, len), off(p, kinds), off(p, slots), slots.map(|v| v.bits()))
}

/// Write `bytes` at `at` in `img`.
fn put(img: &mut [u8], at: usize, bytes: &[u8]) {
    img[at..at + bytes.len()].copy_from_slice(bytes);
}

/// The header of a fresh young object of `kind`, written into `img`.
fn put_header(img: &mut [u8], kind: u8) {
    let probe = GcHeader::new();
    put(img, off(&probe, &probe.kind), &[kind]);
    put(img, off(&probe, &probe.generation), &[GEN_YOUNG]);
}

fn words(img: &[u8]) -> Vec<u64> {
    img.as_chunks::<8>().0.iter().map(|w| u64::from_le_bytes(*w)).collect()
}

/// How to allocate `shape` inline; `None` when it does not fit inline (a
/// spilled payload, a fieldless or untyped instance).
#[must_use]
pub fn inline_object(shape: InlineShape) -> Option<InlineObject> {
    let mut slab = Slab::new();
    let classes = make_inline_classes(&mut slab);
    let placeholder = [Value::default(); ENUM_INLINE_ARITY];
    let (class, layout, data, size, img, words_at) = match shape {
        InlineShape::Enum { tag, kinds, n } => {
            let n = usize::from(n);
            if n == 0 || n > ENUM_INLINE_ARITY {
                return None;
            }
            let obj = ObjEnum::new(tag, EnumPayload::from_slice(&placeholder[..n]).with_kinds(kinds));
            let layout = Layout::new::<GcData<ObjEnum>>();
            let data = std::mem::offset_of!(GcData<ObjEnum>, data);
            let mut img = vec![0u8; layout.size()];
            put_header(&mut img, KIND_ENUM);
            put(&mut img, data + off(&obj, &obj.tag), &obj.tag.to_le_bytes());
            put(&mut img, data + off(&obj, &obj.type_id), &obj.type_id.to_le_bytes());
            let base = data + off(&obj, &obj.payload);
            let (d, len, k, slots, words) = payload_probe(&obj.payload);
            put(&mut img, base, &[d]);
            put(&mut img, base + len, &[n as u8]);
            put(&mut img, base + k, &[kinds]);
            for (i, w) in words.iter().enumerate() {
                put(&mut img, base + slots + i * 8, &w.to_le_bytes());
            }
            (classes[0], layout, data, PACED_HEADER_BYTES + obj.size(), img, base + slots)
        }
        InlineShape::Instance { type_id, n } => {
            if type_id == 0 || n == 0 || usize::from(n) > INSTANCE_INLINE_FIELDS {
                return None;
            }
            let obj = ObjInstance::with_type_id_and_fields(type_id, usize::from(n));
            let layout = Layout::new::<GcData<ObjInstance>>();
            let data = std::mem::offset_of!(GcData<ObjInstance>, data);
            let mut img = vec![0u8; layout.size()];
            put_header(&mut img, KIND_INSTANCE);
            put(&mut img, data + off(&obj, &obj.type_id), &obj.type_id.to_le_bytes());
            let base = data + off(&obj, &obj.storage);
            let (d, len, _, slots, words) = payload_probe(&obj.storage);
            put(&mut img, base, &[d]);
            put(&mut img, base + len, &[n]);
            for (i, w) in words.iter().enumerate() {
                put(&mut img, base + slots + i * 8, &w.to_le_bytes());
            }
            (classes[1], layout, data, PACED_HEADER_BYTES + obj.size(), img, base + slots)
        }
    };
    debug_assert!(data % 8 == 0 && words_at % 8 == 0 && layout.size() % 8 == 0 && layout.align() <= 8);
    Some(InlineObject {
        class: class as u32,
        slot: layout.size() as u32,
        size: size as u32,
        image: words(&img),
        words_at: (words_at / 8) as u32,
    })
}

/// Where native code reads a product object's slots in place, and writes an
/// instance's (`docs/internals/native.md`, "Inline field access"): byte
/// offsets from the object's address, probed from real objects as the
/// images are. An instance's fields and an enum's payload (a tuple's items)
/// are one store at one offset, so the store offsets serve both kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProductSlots {
    /// The header's kind byte, an instance's kind and an enum's.
    pub kind_at: u32,
    pub kind: u8,
    pub enum_kind: u8,
    /// The header's generation byte, and the bit the write barrier takes
    /// the slow path on ([`GEN_UNLOGGED`]).
    pub generation_at: u32,
    pub unlogged: u8,
    /// The storage's discriminant byte.
    pub storage_at: u32,
    /// Inline slots: the discriminant, the `u8` slot count and the words.
    pub inline_tag: u8,
    pub inline_len_at: u32,
    pub inline_slots_at: u32,
    /// Spilled slots: the discriminant and the `Vec`'s pointer and length words.
    pub spill_tag: u8,
    pub spill_ptr_at: u32,
    pub spill_len_at: u32,
    /// Stores may be written in place. A debug or GC-stress runtime checks
    /// every write ([`GcData::as_mut`]), so its stores take the helper.
    pub inline_stores: bool,
}

/// The [`ProductSlots`] of this build.
#[must_use]
pub fn product_slots() -> ProductSlots {
    let data = std::mem::offset_of!(GcData<ObjInstance>, data);
    let header = GcHeader::new();
    let inline = ObjInstance::with_type_id_and_fields(1, 2);
    let storage = data + off(&inline, &inline.storage);
    let variant = ObjEnum::new(7, EnumPayload::one(Value::default()));
    let enum_data = std::mem::offset_of!(GcData<ObjEnum>, data);
    assert_eq!(enum_data + off(&variant, &variant.payload), storage, "an enum's payload and an instance's fields share an offset");
    // SAFETY: `repr(u8)`: the discriminant is the first byte.
    let tag = |s: &EnumPayload| unsafe { *std::ptr::from_ref(&s.inner).cast::<u8>() };
    let (inline_tag, len, _, slots, _) = payload_probe(&inline.storage);
    let (inline_len_at, inline_slots_at) = (storage + len, storage + slots);
    let mut spill = ObjInstance::with_type_id_and_fields(1, INSTANCE_INLINE_FIELDS + 3);
    let spill_tag = tag(&spill.storage);
    if let EnumPayloadInner::Spill { words, .. } = &mut spill.storage.inner {
        words.reserve_exact(5);
    }
    let EnumPayloadInner::Spill { words: v, .. } = &spill.storage.inner else {
        unreachable!("a wide instance spills");
    };
    let vec_at = storage + off(&spill.storage, v);
    // A `Vec`'s words are its capacity, pointer and length in some order:
    // find the pointer and the length (the capacity is larger).
    // SAFETY: a `Vec` is three words.
    let words: [usize; 3] = unsafe { std::mem::transmute_copy(v) };
    let ptr = words.iter().position(|&w| w == v.as_ptr() as usize).expect("a Vec holds its pointer");
    let len_word = words.iter().position(|&w| w == v.len()).expect("a Vec holds its length");
    assert!(v.capacity() != v.len() && ptr != len_word);
    ProductSlots {
        kind_at: off(&header, &header.kind) as u32,
        kind: KIND_INSTANCE,
        enum_kind: KIND_ENUM,
        generation_at: off(&header, &header.generation) as u32,
        unlogged: GEN_UNLOGGED,
        storage_at: storage as u32,
        inline_tag,
        inline_len_at: inline_len_at as u32,
        inline_slots_at: inline_slots_at as u32,
        spill_tag,
        spill_ptr_at: (vec_at + ptr * 8) as u32,
        spill_len_at: (vec_at + len_word * 8) as u32,
        inline_stores: !cfg!(any(debug_assertions, feature = "gc-stress")),
    }
}

/// A digest of everything inline allocation assumes: every shape's image,
/// the counter and cursor layouts. An AOT object records the compiler's,
/// and the runtime refuses it when its own differs
/// ([`crate::native::install_aot`]).
#[must_use]
pub fn layout_fingerprint() -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    for n in 1..=ENUM_INLINE_ARITY as u8 {
        inline_object(InlineShape::Enum { tag: 0x0102_0304, kinds: 0x5a, n }).hash(&mut h);
    }
    for n in 1..=INSTANCE_INLINE_FIELDS as u8 {
        inline_object(InlineShape::Instance { type_id: 0x0607_0809, n }).hash(&mut h);
    }
    let counts = [
        std::mem::offset_of!(HeapCounts, inline_gate),
        std::mem::offset_of!(HeapCounts, fin_words),
        std::mem::offset_of!(HeapCounts, fin_len),
        std::mem::offset_of!(HeapCounts, alloc_bytes),
        std::mem::offset_of!(HeapCounts, young_bytes),
        std::mem::offset_of!(HeapCounts, live_count),
        std::mem::offset_of!(super::super::slab::Bump, next),
        std::mem::offset_of!(super::super::slab::Bump, end),
        std::mem::size_of::<super::super::slab::Bump>(),
    ];
    counts.hash(&mut h);
    product_slots().hash(&mut h);
    // The inline share (#2506).
    (super::HEADER_KIND_OFFSET, super::COW_SHARED_OFFSET, super::COW_SHARED_SET, super::KIND_ARRAY).hash(&mut h);
    // The store view protocol (#2687): code built for plain views only
    // would store in place into an array with a card table.
    (super::VIEW_PLAIN, super::VIEW_TRACKED, super::CARD_SHIFT, super::CARD_MIN_WORDS).hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An inline enum, tuple and instance written from their images read
    /// back as the objects the runtime would have made.
    #[test]
    fn images_build_the_objects_the_runtime_makes() {
        let mut heap = Heap::default();
        heap.set_nursery(true);
        let (counts, bumps) = heap.inline_alloc_ptrs();
        assert_eq!(unsafe { (*counts).inline_gate }, u64::from(!cfg!(feature = "gc-stress")));
        let words = [Value::from(7_i64), Value::from(8_i64), Value::from(9_i64)];
        let mut alloc = |shape: InlineShape, payload: &[Value]| -> u64 {
            let o = inline_object(shape).expect("inline shape");
            // The slow path first, so the class's cursor has a chunk.
            let _ = heap.alloc(ObjEnum::new(1, EnumPayload::one(words[0])), Object::Enum);
            let _ = heap.alloc(ObjInstance::with_type_id_and_fields(3, 1), Object::Instance);
            let b = unsafe { &mut *bumps.add(o.class as usize) };
            assert!(b.next + u64::from(o.slot) <= b.end);
            let at = b.next;
            b.next += u64::from(o.slot);
            let c = unsafe { &mut *counts };
            c.alloc_bytes += o.size as usize;
            c.young_bytes += o.size as usize;
            c.live_count += 1;
            let p = at as *mut u64;
            for (i, w) in o.image.iter().enumerate() {
                unsafe { p.add(i).write(*w) };
            }
            for (i, v) in payload.iter().enumerate() {
                unsafe { p.add(o.words_at as usize + i).write(v.bits()) };
            }
            at
        };
        let e = alloc(InlineShape::Enum { tag: 4, kinds: 0, n: 2 }, &words[1..]);
        let tuple = InlineShape::Enum { tag: common::PRODUCT_TAG, kinds: 0, n: 3 };
        let t = alloc(tuple, &words);
        let i = alloc(InlineShape::Instance { type_id: 5, n: 2 }, &[]);
        let Some(Object::Enum(gc)) = heap.find_object_by_addr(e) else { panic!("not an enum") };
        assert_eq!(gc.as_ref().tag, 4);
        assert_eq!(gc.as_ref().payload.as_slice(), &words[1..]);
        assert_eq!(Object::Enum(gc).generation() & GEN_YOUNG, GEN_YOUNG);
        let Some(gc) = heap.find_object_by_addr(t).and_then(Object::as_tuple) else { panic!("not a tuple") };
        assert_eq!(gc.as_ref().elements(), &words);
        let Some(Object::Instance(gc)) = heap.find_object_by_addr(i) else { panic!("not an instance") };
        assert_eq!(gc.as_ref().type_id, 5);
        assert_eq!(gc.as_ref().slot(1), Some(Value::from(0_i64)));
        assert_eq!(gc.as_ref().slot(2), None);
        for (a, s) in [(e, InlineShape::Enum { tag: 4, kinds: 0, n: 2 }), (t, tuple), (i, InlineShape::Instance { type_id: 5, n: 2 })] {
            assert_eq!(heap.find_object_by_addr(a).map(|o| o.size()), inline_object(s).map(|o| o.size as usize));
        }
    }

    #[test]
    fn wide_payloads_and_untyped_instances_stay_on_the_helper() {
        assert!(inline_object(InlineShape::Enum { tag: 0, kinds: 0, n: 5 }).is_none());
        assert!(inline_object(InlineShape::Enum { tag: common::PRODUCT_TAG, kinds: 0, n: 0 }).is_none());
        assert!(inline_object(InlineShape::Instance { type_id: 0, n: 1 }).is_none());
        assert!(inline_object(InlineShape::Instance { type_id: 1, n: 5 }).is_none());
    }

    /// Slots read and written at [`product_slots`]' offsets are the
    /// instance's own, inline and spilled.
    #[test]
    fn product_slots_find_the_fields() {
        let l = product_slots();
        let mut heap = Heap::default();
        for n in [2usize, INSTANCE_INLINE_FIELDS + 2] {
            let mut obj = ObjInstance::with_type_id_and_fields(9, n);
            for i in 0..n {
                obj.set_slot(i, Value::from(100 + i as i64));
            }
            let addr = heap.alloc(obj, Object::Instance).1.as_ptr() as u64;
            let byte = |at: u32| unsafe { *((addr + u64::from(at)) as *const u8) };
            let word = |at: u32| unsafe { *((addr + u64::from(at)) as *const u64) };
            assert_eq!(byte(l.kind_at), l.kind);
            let (ptr, len) = if byte(l.storage_at) == l.inline_tag {
                (addr + u64::from(l.inline_slots_at), u64::from(byte(l.inline_len_at)))
            } else {
                assert_eq!(byte(l.storage_at), l.spill_tag);
                (word(l.spill_ptr_at), word(l.spill_len_at))
            };
            assert_eq!(len, n as u64);
            for i in 0..n {
                assert_eq!(unsafe { *((ptr + 8 * i as u64) as *const i64) }, 100 + i as i64);
            }
            unsafe { *((ptr + 8) as *mut i64) = 7 };
            let Some(Object::Instance(gc)) = heap.find_object_by_addr(addr) else { panic!("not an instance") };
            assert_eq!(gc.as_ref().slot(1), Some(Value::from(7_i64)));
        }
    }

    #[test]
    fn the_fingerprint_is_stable_within_a_build() {
        assert_eq!(layout_fingerprint(), layout_fingerprint());
    }
}
