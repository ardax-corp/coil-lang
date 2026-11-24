//! Young chunks: instances, payload enums, tuples, arrays and closures are
//! bump-allocated into them ([`super::Slab::alloc_young`]), and a freed slot
//! goes on its class's free list. [`Heap::promote_young`] makes them old
//! chunks where nothing may allocate young (a steal epoch).

use std::ptr::NonNull;

use super::{GcHeader, Heap};

/// Slot addresses of one span.
fn slots(first: u64, end: u64, size: u64) -> impl Iterator<Item = u64> {
    (first..end).step_by(size as usize)
}

impl Heap {

    /// Whether any nursery chunk holds young objects (in use or held).
    pub fn has_young(&self) -> bool {
        self.slab.has_young_chunks()
    }

    /// Make every young object old where it is, without tracing: dead ones
    /// wait for a major cycle. For points where nothing may move (a host
    /// frame holds raw handles, a shared-heap epoch opens).
    pub fn promote_young(&mut self) {
        if !self.has_young() {
            return;
        }
        let mut promoted = Vec::new();
        let mut free = Vec::new();
        for (chunk, first, end, size) in self.slab.young_spans() {
            promoted.push(chunk);
            for addr in slots(first, end, size) {
                let header = unsafe { &*(addr as *const GcHeader) };
                if header.kind.get() == 0 {
                    free.push(unsafe { NonNull::new_unchecked(addr as *mut u8) });
                    continue;
                }
                header.generation.set(0);
            }
            let tail = self.slab.chunk_slots_end(chunk);
            free.extend(slots(end, tail, size).map(|a| unsafe { NonNull::new_unchecked(a as *mut u8) }));
        }
        self.slab.finish_young(&promoted, free);
    }

}
