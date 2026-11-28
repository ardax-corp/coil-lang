//! The map table natives (`key_index::map_*`, archive 8.16): one word
//! array that indexes a collection's keys by slot, the cells holding each
//! key's hash beside its slot so a probe compares hashes on the cell's own
//! cache line. Native code runs `map_find`, `map_upsert`, `map_live` and
//! `map_len` in place (`coil-native`, "Map table ops"); these functions are
//! what the MIR interpreter calls and what native code falls back to.
//!
//! Layout of a table `ix` (an ordinary `Vec<int>`, every word a number):
//!
//! | Words | Hold |
//! |-------|------|
//! | `ix[0]` | `mask`: `cap - 1`, `cap` a power of two, at least [`MIN_CAP`] |
//! | `ix[1]` | cells in use: live entries and tombstones |
//! | `ix[2]` | live entries |
//! | `ix[3]` | `0`, reserved |
//! | `ix[4..4 + 2cap]` | cells `{hash, ref}`: `ref` is `slot + 1`, `0` empty, `-1` a tombstone |
//! | `ix[4 + 2cap..]` | one word per slot, in insertion order: its cell, or `-1` once removed |
//!
//! The owner keeps keys and values in its own `Vec`s by slot, as with
//! `key_index`. See `docs/internals/key-index.md`, "Map tables".

use common::Value;

use crate::memory::{Gc, Heap, ObjArray, Object};

/// Header words before the cells.
pub const HEADER: usize = 4;
const MIN_CAP: usize = 8;
const EMPTY: i64 = 0;
const TOMB: i64 = -1;

/// The probe start for hash `h`: the `key_index` fold (`home`), so float
/// bit patterns, whose low bits are zero, spread too.
#[inline(always)]
pub fn home(h: i64, mask: usize) -> usize {
    let h = h ^ (h >> 32);
    let h = h ^ (h >> 16);
    (h as u64 as usize) & mask
}

/// The smallest cell count (a power of two, at least [`MIN_CAP`]) that
/// keeps `n` entries at most half full.
fn cap_for(n: usize) -> usize {
    let mut cap = MIN_CAP;
    while (n + 1) * 2 > cap {
        cap *= 2;
    }
    cap
}

/// A well-formed table's cell count: `mask + 1` a power of two and the
/// words long enough for the cells.
pub fn table_cap(ix: &[Value]) -> Option<usize> {
    let mask = usize::try_from(ix.first()?.as_int()).ok()?;
    let cap = mask.checked_add(1)?;
    (cap.is_power_of_two() && ix.len() >= HEADER.checked_add(cap.checked_mul(2)?)?).then_some(cap)
}

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).copied().unwrap_or(Value::from(0i64))
}

fn array(heap: &Heap, v: Value) -> Option<Gc<ObjArray>> {
    match heap.find_object_by_addr(v.bits()) {
        Some(Object::Array(gc)) => Some(gc),
        _ => None,
    }
}

/// The words of a well-formed table, for writing, and its cell count.
fn table_mut(gc: &Gc<ObjArray>) -> Option<(&mut Vec<Value>, usize)> {
    let cap = table_cap(gc.as_ref().words()?)?;
    Some((gc.payload_mut().words_mut(), cap))
}

/// A fresh table with `cap` cells and room for `room` slots before its
/// storage grows. Its words come zeroed from the allocator (large ones
/// as fresh pages), so the empty cells cost no stores.
fn fresh(cap: usize, room: usize) -> Vec<Value> {
    let len = HEADER + 2 * cap;
    let mut words = vec![0u64; len + room];
    words.truncate(len);
    let mut words = std::mem::ManuallyDrop::new(words);
    // SAFETY: `Value` is `repr(transparent)` over a `u64` word and `0` is
    // the number `0`: same size, alignment and allocation.
    let mut ix = unsafe { Vec::from_raw_parts(words.as_mut_ptr().cast::<Value>(), words.len(), words.capacity()) };
    ix[0] = Value::from((cap - 1) as i64);
    ix
}

/// `key_index::map_table(n) -> Vec<int>`: an empty table with room for `n`
/// entries before it grows.
pub fn host_map_table(heap: &mut Heap, args: &[Value]) -> Value {
    // As `key_index::table`: no presize past 2^28 entries (wasm32).
    let n = arg(args, 0).as_int().clamp(0, 1 << 28) as usize;
    let words = fresh(cap_for(n), n);
    let (obj, _) = heap.alloc(ObjArray::new(words).of_kind(common::WORD_SCALAR), Object::Array);
    Value::from(obj.addr())
}

/// Whether two key words are equal under `mode` (`common::KEY_EQ_*`).
fn same_key(heap: &Heap, mode: i64, a: Value, b: Value) -> bool {
    match mode {
        common::KEY_EQ_WORD => a.bits() == b.bits(),
        common::KEY_EQ_STRING => a.bits() == b.bits() || string_eq(heap, a, b),
        _ => false,
    }
}

/// Whether `a` and `b` are strings with the same contents.
fn string_eq(heap: &Heap, a: Value, b: Value) -> bool {
    match (heap.find_object_by_addr(a.bits()), heap.find_object_by_addr(b.bits())) {
        (Some(Object::String(ga)), Some(Object::String(gb))) => ga.as_ref().data == gb.as_ref().data,
        _ => false,
    }
}

/// The slot of the key equal to `k` (hash `h`) in `keys`, or `-1`.
fn find(heap: &Heap, ix: &[Value], cap: usize, keys: &[Value], h: i64, k: Value, mode: i64) -> i64 {
    let mask = cap - 1;
    let mut i = home(h, mask);
    for _ in 0..cap {
        let r = ix[HEADER + 2 * i + 1].as_int();
        if r == EMPTY {
            return -1;
        }
        if r > 0 && ix[HEADER + 2 * i].as_int() == h {
            let s = (r - 1) as usize;
            if keys.get(s).is_some_and(|&x| same_key(heap, mode, x, k)) {
                return s as i64;
            }
        }
        i = (i + 1) & mask;
    }
    -1
}

/// `key_index::map_find(ix, keys, h, k, mode) -> int`: the slot whose key
/// in `keys` equals `k` (hash `h`), comparing by `mode` (what `eq_mode`
/// gave), or `-1`. Mode `0` finds nothing: the caller probes and compares.
pub fn host_map_find(heap: &mut Heap, args: &[Value]) -> Value {
    let (Some(tg), Some(kg)) = (array(heap, arg(args, 0)), array(heap, arg(args, 1))) else {
        return Value::from(-1i64);
    };
    let (Some(ix), Some(keys)) = (tg.as_ref().words(), kg.as_ref().words()) else {
        return Value::from(-1i64);
    };
    let Some(cap) = table_cap(ix) else {
        return Value::from(-1i64);
    };
    let (h, k, mode) = (arg(args, 2).as_int(), arg(args, 3), arg(args, 4).as_int());
    Value::from(find(heap, ix, cap, keys, h, k, mode))
}

/// Rebuild the cells of `ix` at `cap` cells over the live slots. With
/// `renumber`, the removed slots are dropped and the live ones numbered
/// from `0` in order; otherwise every slot keeps its number.
fn rebuild(ix: &mut Vec<Value>, old_cap: usize, cap: usize, renumber: bool) {
    let tail = HEADER + 2 * old_cap;
    let slots = ix.len() - tail;
    // Room for the slots the new cells take before they grow again.
    let room = (cap / 2).max(slots + 1);
    let mut out = fresh(cap, room);
    let mask = cap - 1;
    let (head, slot_words) = ix.split_at(tail);
    let old_cells = &head[HEADER..];
    // The new slot words go after the cells, written while the cells fill.
    let mut words: Vec<Value> = Vec::with_capacity(slots);
    {
        let cells = &mut out[HEADER..HEADER + 2 * cap];
        for c in slot_words {
            let Ok(c) = usize::try_from(c.as_int()) else {
                if !renumber {
                    words.push(Value::from(TOMB));
                }
                continue;
            };
            let h = old_cells[2 * c];
            let mut i = home(h.as_int(), mask);
            while cells[2 * i + 1].bits() != 0 {
                i = (i + 1) & mask;
            }
            cells[2 * i] = h;
            cells[2 * i + 1] = Value::from(words.len() as i64 + 1);
            words.push(Value::from(i as i64));
        }
    }
    let used = words.iter().filter(|w| w.as_int() >= 0).count() as i64;
    out.extend_from_slice(&words);
    out[1] = Value::from(used);
    out[2] = Value::from(used);
    *ix = out;
}

/// Register a new slot with hash `h` (the caller has checked its key is
/// absent): grow when the cells would pass half full, take the first empty
/// or tombstone cell of its probe sequence. Returns the slot.
fn add(ix: &mut Vec<Value>, cap: usize, h: i64) -> i64 {
    let used = ix[1].as_int().max(0) as usize;
    let cap = if (used + 1) * 2 > cap {
        let live = ix[2].as_int().max(0) as usize;
        let new_cap = cap_for(live + 1).max(cap);
        rebuild(ix, cap, new_cap, false);
        new_cap
    } else {
        cap
    };
    let mask = cap - 1;
    let mut i = home(h, mask);
    loop {
        let r = ix[HEADER + 2 * i + 1].as_int();
        if r == EMPTY || r == TOMB {
            let s = ix.len() - (HEADER + 2 * cap);
            ix[HEADER + 2 * i] = Value::from(h);
            ix[HEADER + 2 * i + 1] = Value::from(s as i64 + 1);
            if r == EMPTY {
                ix[1] = Value::from(ix[1].as_int() + 1);
            }
            ix[2] = Value::from(ix[2].as_int() + 1);
            ix.push(Value::from(i as i64));
            return s as i64;
        }
        i = (i + 1) & mask;
    }
}

/// `key_index::map_add(ix, h) -> int`: a new slot for a key with hash `h`
/// the caller checked is absent; grows the table. `-1` for a malformed
/// table.
pub fn host_map_add(heap: &mut Heap, args: &[Value]) -> Value {
    let Some(mut tg) = array(heap, arg(args, 0)) else {
        return Value::from(-1i64);
    };
    let Some((ix, cap)) = table_mut(&tg) else {
        return Value::from(-1i64);
    };
    let s = add(ix, cap, arg(args, 1).as_int());
    heap.account_array(tg.as_mut());
    Value::from(s)
}

/// `key_index::map_upsert(ix, keys, h, k, mode) -> int`: the slot of `k`
/// as `map_find` gives it, or, when `k` is absent, a new slot `s` (the
/// number of slots so far) registered for it and `-1 - s` returned: the
/// caller then pushes `k` and its value. Mode `0` registers without looking.
pub fn host_map_upsert(heap: &mut Heap, args: &[Value]) -> Value {
    let (t, kv) = (arg(args, 0), arg(args, 1));
    if t.bits() == kv.bits() {
        return Value::from(-1i64);
    }
    let (Some(mut tg), Some(kg)) = (array(heap, t), array(heap, kv)) else {
        return Value::from(-1i64);
    };
    let (h, k, mode) = (arg(args, 2).as_int(), arg(args, 3), arg(args, 4).as_int());
    let found = match (tg.as_ref().words(), kg.as_ref().words()) {
        (Some(ix), Some(keys)) => match table_cap(ix) {
            Some(cap) => find(heap, ix, cap, keys, h, k, mode),
            None => return Value::from(-1i64),
        },
        _ => return Value::from(-1i64),
    };
    if found >= 0 {
        return Value::from(found);
    }
    let Some((ix, cap)) = table_mut(&tg) else {
        return Value::from(-1i64);
    };
    let s = add(ix, cap, h);
    heap.account_array(tg.as_mut());
    Value::from(-1 - s)
}

/// `key_index::map_probe(ix, h, after) -> int`: the next live slot whose
/// hash is `h`, in probe order after slot `after` (`-1` starts at the
/// first), or `-1`. The caller compares the candidate's key with `==`.
pub fn host_map_probe(heap: &mut Heap, args: &[Value]) -> Value {
    let Some(tg) = array(heap, arg(args, 0)) else {
        return Value::from(-1i64);
    };
    let Some(ix) = tg.as_ref().words() else {
        return Value::from(-1i64);
    };
    let Some(cap) = table_cap(ix) else {
        return Value::from(-1i64);
    };
    let (h, after) = (arg(args, 1).as_int(), arg(args, 2).as_int());
    let mask = cap - 1;
    let mut i = home(h, mask);
    let mut past = after < 0;
    for _ in 0..cap {
        let r = ix[HEADER + 2 * i + 1].as_int();
        if r == EMPTY {
            break;
        }
        if r > 0 {
            if past {
                if ix[HEADER + 2 * i].as_int() == h {
                    return Value::from(r - 1);
                }
            } else if r - 1 == after {
                past = true;
            }
        }
        i = (i + 1) & mask;
    }
    Value::from(-1i64)
}

/// Slot `s`'s cell in a well-formed table, when the slot is live.
fn live_cell(ix: &[Value], cap: usize, s: i64) -> Option<usize> {
    let s = usize::try_from(s).ok()?;
    let c = ix.get(HEADER + 2 * cap + s)?.as_int();
    usize::try_from(c).ok().filter(|&c| c < cap)
}

/// `key_index::map_live(ix, s) -> bool`: whether slot `s` holds a key.
pub fn host_map_live(heap: &mut Heap, args: &[Value]) -> Value {
    let live = array(heap, arg(args, 0)).is_some_and(|tg| {
        tg.as_ref().words().is_some_and(|ix| table_cap(ix).is_some_and(|cap| live_cell(ix, cap, arg(args, 1).as_int()).is_some()))
    });
    Value::from(live)
}

/// `key_index::map_len(ix) -> int`: the number of live entries.
pub fn host_map_len(heap: &mut Heap, args: &[Value]) -> Value {
    let n = array(heap, arg(args, 0))
        .and_then(|tg| {
            let ix = tg.as_ref().words()?;
            table_cap(ix)?;
            Some(ix[2].as_int())
        })
        .unwrap_or(0);
    Value::from(n)
}

/// `key_index::map_remove(ix, s) -> bool`: removes live slot `s`, leaving a
/// tombstone in its cell; other slots keep their numbers. False when the
/// slot was not live.
pub fn host_map_remove(heap: &mut Heap, args: &[Value]) -> Value {
    let Some(tg) = array(heap, arg(args, 0)) else {
        return Value::from(false);
    };
    let Some(cap) = tg.as_ref().words().and_then(table_cap) else {
        return Value::from(false);
    };
    let s = arg(args, 1).as_int();
    let Some(c) = tg.as_ref().words().and_then(|ix| live_cell(ix, cap, s)) else {
        return Value::from(false);
    };
    let Some((ix, _)) = table_mut(&tg) else {
        return Value::from(false);
    };
    ix[HEADER + 2 * c + 1] = Value::from(TOMB);
    ix[HEADER + 2 * cap + s as usize] = Value::from(TOMB);
    ix[2] = Value::from(ix[2].as_int() - 1);
    Value::from(true)
}

/// `key_index::map_compact(ix) -> int`: drops the removed slots and numbers
/// the live ones from `0` in order (the owner has compacted its own `Vec`s
/// the same way), rebuilding the cells. Returns the live count.
pub fn host_map_compact(heap: &mut Heap, args: &[Value]) -> Value {
    let Some(mut tg) = array(heap, arg(args, 0)) else {
        return Value::from(-1i64);
    };
    let Some((ix, cap)) = table_mut(&tg) else {
        return Value::from(-1i64);
    };
    rebuild(ix, cap, cap, true);
    let n = ix[2].as_int();
    heap.account_array(tg.as_mut());
    Value::from(n)
}

/// `key_index::map_clear(ix) -> unit`: no entries and no slots; the table
/// keeps its cell count.
pub fn host_map_clear(heap: &mut Heap, args: &[Value]) -> Value {
    if let Some(tg) = array(heap, arg(args, 0))
        && let Some((ix, cap)) = table_mut(&tg)
    {
        ix.truncate(HEADER + 2 * cap);
        ix[HEADER..].fill(Value::from(EMPTY));
        ix[1] = Value::from(0i64);
        ix[2] = Value::from(0i64);
    }
    Value::from(0i64)
}

/// HostInvoke wiring for `key_index::map_*` (archive 8.16), after
/// `thread_par_room`.
pub const MAP_TABLE_WIRING: &[(&str, usize, crate::HostValueFn)] = &[
    (common::MAP_TABLE_NATIVE, 1, host_map_table),
    (common::MAP_FIND_NATIVE, 5, host_map_find),
    (common::MAP_UPSERT_NATIVE, 5, host_map_upsert),
    (common::MAP_PROBE_NATIVE, 3, host_map_probe),
    (common::MAP_ADD_NATIVE, 2, host_map_add),
    (common::MAP_LIVE_NATIVE, 2, host_map_live),
    (common::MAP_LEN_NATIVE, 1, host_map_len),
    (common::MAP_REMOVE_NATIVE, 2, host_map_remove),
    (common::MAP_COMPACT_NATIVE, 1, host_map_compact),
    (common::MAP_CLEAR_NATIVE, 1, host_map_clear),
];

#[cfg(test)]
#[path = "map_table.tests.rs"]
mod tests;
