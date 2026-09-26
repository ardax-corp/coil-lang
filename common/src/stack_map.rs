//! Interpreter slot / frame maps (S2b / COI-306).
//!
//! Encoded from S2a live-root sidecars. Archive minor 14+ stores maps on
//! `.hyc` / embed. Older envelopes load with empty maps (conservative stack
//! GC), matching pre-E1 `execute_archived_program`.

use rkyv::{Archive, Deserialize, Serialize};

/// Live heap IL slots at one alloc / `GcBarrier` bytecode PC.
#[derive(Clone, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct SlotMap {
    /// Absolute bytecode PC of the allocating opcode (safepoint).
    pub pc: u32,
    /// Frame-relative IL slots that hold live heap words.
    pub slots: Vec<u16>,
}

/// Per-function slot / frame map for bodies that allocate.
#[derive(Clone, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct FrameStackMap {
    /// Function entry PC (inclusive).
    pub entry_pc: u32,
    /// First PC past this body (exclusive). `u32::MAX` if unknown.
    pub end_pc: u32,
    /// Union of every safepoint's slots (frame map).
    pub frame_slots: Vec<u16>,
    /// Per-safepoint maps, sorted by `pc`.
    pub safepoints: Vec<SlotMap>,
}

impl FrameStackMap {
    /// Slots live at `ip`, or the frame union when `ip` is not a recorded edge.
    pub fn slots_at(&self, ip: u32) -> &[u16] {
        let mut best: Option<&SlotMap> = None;
        for sp in &self.safepoints {
            if sp.pc <= ip {
                best = Some(sp);
            } else {
                break;
            }
        }
        best.map(|s| s.slots.as_slice())
            .unwrap_or(self.frame_slots.as_slice())
    }

    pub fn contains_pc(&self, ip: u32) -> bool {
        ip >= self.entry_pc && ip < self.end_pc
    }
}

/// Complete frame description (archive minor 21+): anywhere in
/// `[entry_pc, end_pc)`, only `heap_slots` (frame-relative) of this frame can
/// hold heap words, so the GC roots those instead of scanning the frame.
#[derive(Clone, Debug, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct PreciseFrameMap {
    pub entry_pc: u32,
    pub end_pc: u32,
    pub heap_slots: Vec<u16>,
}

/// The precise map covering `pc`, if any (`maps` sorted by `entry_pc`, disjoint).
pub fn precise_map_for_pc(maps: &[PreciseFrameMap], pc: u32) -> Option<&PreciseFrameMap> {
    let i = maps.partition_point(|m| m.entry_pc <= pc);
    let m = maps.get(i.checked_sub(1)?)?;
    (pc < m.end_pc).then_some(m)
}

/// Last map whose `entry_pc <= ip` and `contains_pc(ip)`.
pub fn map_for_ip(maps: &[FrameStackMap], ip: u32) -> Option<&FrameStackMap> {
    let mut best = None;
    for (i, m) in maps.iter().enumerate() {
        if m.entry_pc > ip {
            break;
        }
        if m.contains_pc(ip) {
            best = Some(i);
        }
    }
    best.map(|i| &maps[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_at_uses_last_safepoint_at_or_before_ip() {
        let m = FrameStackMap {
            entry_pc: 10,
            end_pc: 40,
            frame_slots: vec![0, 1],
            safepoints: vec![
                SlotMap {
                    pc: 12,
                    slots: vec![0],
                },
                SlotMap {
                    pc: 20,
                    slots: vec![0, 2],
                },
            ],
        };
        assert_eq!(m.slots_at(11), &[0, 1]);
        assert_eq!(m.slots_at(12), &[0]);
        assert_eq!(m.slots_at(25), &[0, 2]);
        assert!(m.contains_pc(10) && !m.contains_pc(40));
    }

    #[test]
    fn precise_map_lookup_respects_bounds() {
        let maps = vec![
            PreciseFrameMap { entry_pc: 10, end_pc: 20, heap_slots: vec![] },
            PreciseFrameMap { entry_pc: 30, end_pc: 40, heap_slots: vec![1] },
        ];
        assert!(precise_map_for_pc(&maps, 9).is_none());
        assert_eq!(precise_map_for_pc(&maps, 10).map(|m| m.entry_pc), Some(10));
        assert!(precise_map_for_pc(&maps, 20).is_none());
        assert!(precise_map_for_pc(&maps, 25).is_none());
        assert_eq!(precise_map_for_pc(&maps, 39).map(|m| m.entry_pc), Some(30));
        assert!(precise_map_for_pc(&maps, 40).is_none());
    }
}
