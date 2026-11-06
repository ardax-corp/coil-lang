//! The key index natives (`key_index::*`, archive 6.19): an
//! insertion-ordered hash index that maps keys to dense slot numbers.
//!
//! The index owns no keys. A collection keeps them, and any payload, in
//! its own `Vec`s indexed by slot; the natives keep three `Vec<int>`s
//! beside them:
//!
//! - `table`: `cap` cells (a power of two) of linear-probing positions,
//!   `0` empty, `-1` a removed entry's place, `slot + 1` otherwise, then
//!   one more cell counting the occupied ones (live and removed), so an
//!   insert knows when to grow without a scan.
//! - `hashes`: each slot's key hash, as the caller computed it.
//! - `live`: `1` while the slot holds an entry, `0` once it is removed.
//!
//! Equality stays with the caller: [`host_probe`] returns the slots whose
//! hash matches, and the caller's `==` decides. For keys whose `==` the runtime
//! runs itself (`int`, `string`: what `eq_mode` answers), [`host_find`]
//! compares them too and finds a key in one call. Slots never move on their
//! own: only [`host_compact`], which the owner calls after it compacts its
//! own `Vec`s with [`host_retain`], renumbers them.
//!
//! See `docs/internals/key-index.md`.

use common::Value;

use crate::memory::{Gc, Heap, ObjArray, Object};

const EMPTY: i64 = 0;
const REMOVED: i64 = -1;
const MIN_CAP: usize = 8;

/// The probe start for hash `h`: its high bits folded into the low ones
/// the mask keeps (float hashes are bit patterns with zero low bits).
#[inline(always)]
fn home(h: i64, mask: usize) -> usize {
    let h = h ^ (h >> 32);
    let h = h ^ (h >> 16);
    (h as u64 as usize) & mask
}

/// The smallest table size (a power of two, at least [`MIN_CAP`]) that
/// keeps `n` entries at most half full.
fn cap_for(n: usize) -> usize {
    let mut cap = MIN_CAP;
    while (n + 1) * 2 > cap {
        cap *= 2;
    }
    cap
}

fn array(heap: &Heap, v: Value) -> Option<Gc<ObjArray>> {
    match heap.find_object_by_addr(v.bits()) {
        Some(Object::Array(gc)) => Some(gc),
        _ => None,
    }
}

/// The word elements of an unpacked array (an `int` `Vec` never packs).
fn words(gc: &Gc<ObjArray>) -> Option<&mut Vec<Value>> {
    gc.as_ref().words()?;
    Some(gc.payload_mut().words_mut())
}

/// A table's cells and its size, when it is well formed.
fn cells(table: &[Value]) -> Option<usize> {
    let cap = table.len().checked_sub(1)?;
    cap.is_power_of_two().then_some(cap)
}

/// `key_index::table(n) -> Vec<int>`: an empty table with room for `n`
/// entries before it grows.
pub fn host_table(heap: &mut Heap, args: &[Value]) -> Value {
    // A presize past 2^28 entries is not honoured (it would overflow
    // `cap_for` on a 32-bit `usize`, wasm32); the table grows as it fills.
    let n = args.first().map_or(0, |v| v.as_int()).clamp(0, 1 << 28) as usize;
    let cap = cap_for(n);
    let mut cells = vec![Value::from(EMPTY); cap + 1];
    cells[cap] = Value::from(0i64);
    let (obj, _) = heap.alloc(ObjArray::new(cells), Object::Array);
    Value::from(obj.addr())
}

/// `key_index::probe(table, hashes, h, after) -> int`: the next live slot
/// whose hash is `h`, in probe order after slot `after` (`-1` starts at
/// the first), or `-1` when there is none.
pub fn host_probe(heap: &mut Heap, args: &[Value]) -> Value {
    let arg = |i: usize| args.get(i).copied().unwrap_or(Value::from(0i64));
    let (Some(tg), Some(hg)) = (array(heap, arg(0)), array(heap, arg(1))) else {
        return Value::from(-1i64);
    };
    let (Some(table), Some(hashes)) = (tg.as_ref().words(), hg.as_ref().words()) else {
        return Value::from(-1i64);
    };
    let Some(cap) = cells(table) else {
        return Value::from(-1i64);
    };
    let (h, after) = (arg(2).as_int(), arg(3).as_int());
    Value::from(probe(table, hashes, cap, h, after))
}

fn probe(table: &[Value], hashes: &[Value], cap: usize, h: i64, after: i64) -> i64 {
    let mask = cap - 1;
    let mut i = home(h, mask);
    let mut past = after < 0;
    for _ in 0..cap {
        let e = table[i].as_int();
        if e == EMPTY {
            return -1;
        }
        if e > 0 {
            let s = e - 1;
            if past {
                if hashes.get(s as usize).is_some_and(|x| x.as_int() == h) {
                    return s;
                }
            } else if s == after {
                past = true;
            }
        }
        i = (i + 1) & mask;
    }
    -1
}

/// `key_index::find(table, hashes, keys, h, k, mode) -> int`: the live
/// slot whose hash is `h` and whose key in `keys` equals `k`, or `-1`.
/// `mode` says how keys compare ([`common::KEY_EQ_WORD`] or
/// [`common::KEY_EQ_STRING`], what `eq_mode` gave the caller); any other
/// mode finds nothing, so the caller compares with `probe` itself.
pub fn host_find(heap: &mut Heap, args: &[Value]) -> Value {
    let arg = |i: usize| args.get(i).copied().unwrap_or(Value::from(0i64));
    let (Some(tg), Some(hg), Some(kg)) = (array(heap, arg(0)), array(heap, arg(1)), array(heap, arg(2))) else {
        return Value::from(-1i64);
    };
    let (Some(table), Some(hashes), Some(keys)) = (tg.as_ref().words(), hg.as_ref().words(), kg.as_ref().words()) else {
        return Value::from(-1i64);
    };
    let Some(cap) = cells(table) else {
        return Value::from(-1i64);
    };
    let (h, k, mode) = (arg(3).as_int(), arg(4), arg(5).as_int());
    let same = |x: Value| match mode {
        common::KEY_EQ_WORD => x.bits() == k.bits(),
        common::KEY_EQ_STRING => x.bits() == k.bits() || string_eq(heap, x, k),
        _ => false,
    };
    let mask = cap - 1;
    let mut i = home(h, mask);
    for _ in 0..cap {
        let e = table[i].as_int();
        if e == EMPTY {
            return Value::from(-1i64);
        }
        if e > 0 {
            let s = (e - 1) as usize;
            if hashes.get(s).is_some_and(|x| x.as_int() == h) && keys.get(s).is_some_and(|&x| same(x)) {
                return Value::from(s as i64);
            }
        }
        i = (i + 1) & mask;
    }
    Value::from(-1i64)
}

/// Whether `a` and `b` are strings with the same contents.
fn string_eq(heap: &Heap, a: Value, b: Value) -> bool {
    match (heap.find_object_by_addr(a.bits()), heap.find_object_by_addr(b.bits())) {
        (Some(Object::String(ga)), Some(Object::String(gb))) => ga.as_ref().data == gb.as_ref().data,
        _ => false,
    }
}

/// `key_index::eq_mode(k) -> int`: the compiler folds every call it plans,
/// as `KEY_EQ_COIL` in a body generic in `k`'s type, which is what this
/// answers for a call it did not.
pub fn host_eq_mode(_heap: &mut Heap, _args: &[Value]) -> Value {
    Value::from(common::KEY_EQ_COIL)
}

/// Put slot `s` (hash `h`) in the first empty or removed cell of its probe
/// sequence. Returns whether it took an empty one.
fn place(table: &mut [Value], cap: usize, h: i64, s: usize) -> bool {
    let mask = cap - 1;
    let mut i = home(h, mask);
    loop {
        let e = table[i].as_int();
        if e == EMPTY || e == REMOVED {
            table[i] = Value::from(s as i64 + 1);
            return e == EMPTY;
        }
        i = (i + 1) & mask;
    }
}

/// Rebuild `table` over the live slots at the size that keeps `extra` more
/// entries under half full.
fn rebuild(table: &mut Vec<Value>, hashes: &[Value], live: &[Value], extra: usize) {
    let n = live.iter().filter(|v| v.bits() != 0).count();
    let cap = cap_for(n + extra);
    table.clear();
    table.resize(cap + 1, Value::from(EMPTY));
    for (s, (h, l)) in hashes.iter().zip(live).enumerate() {
        if l.bits() != 0 {
            place(table, cap, h.as_int(), s);
        }
    }
    table[cap] = Value::from(n as i64);
}

/// `key_index::insert(table, hashes, live, h) -> int`: a new slot for a
/// key with hash `h` (the caller has checked the key is not there),
/// growing the table when it would pass half full. Returns the slot, or
/// `-1` for malformed arguments.
pub fn host_insert(heap: &mut Heap, args: &[Value]) -> Value {
    let arg = |i: usize| args.get(i).copied().unwrap_or(Value::from(0i64));
    let (t, hs, lv) = (arg(0), arg(1), arg(2));
    if t.bits() == hs.bits() || t.bits() == lv.bits() || hs.bits() == lv.bits() {
        return Value::from(-1i64);
    }
    let (Some(tg), Some(hg), Some(lg)) = (array(heap, t), array(heap, hs), array(heap, lv)) else {
        return Value::from(-1i64);
    };
    let (Some(table), Some(hashes), Some(live)) = (words(&tg), words(&hg), words(&lg)) else {
        return Value::from(-1i64);
    };
    let Some(cap) = cells(table) else {
        return Value::from(-1i64);
    };
    if hashes.len() != live.len() {
        return Value::from(-1i64);
    }
    let h = arg(3).as_int();
    let s = hashes.len();
    let occupied = table[cap].as_int().max(0) as usize;
    let cap = if (occupied + 1) * 2 > cap {
        rebuild(table, hashes, live, 1);
        table.len() - 1
    } else {
        cap
    };
    hashes.push(Value::from(h));
    live.push(Value::from(1i64));
    if place(table, cap, h, s) {
        table[cap] = Value::from(table[cap].as_int() + 1);
    }
    for mut gc in [tg, hg, lg] {
        heap.account_array(gc.as_mut());
    }
    Value::from(s as i64)
}

/// `key_index::remove(table, hashes, live, slot) -> bool`: mark live slot
/// `slot` removed. False when it was not live.
pub fn host_remove(heap: &mut Heap, args: &[Value]) -> Value {
    let arg = |i: usize| args.get(i).copied().unwrap_or(Value::from(0i64));
    let (t, hs, lv) = (arg(0), arg(1), arg(2));
    if t.bits() == lv.bits() {
        return Value::from(false);
    }
    let (Some(tg), Some(hg), Some(lg)) = (array(heap, t), array(heap, hs), array(heap, lv)) else {
        return Value::from(false);
    };
    let (Some(table), Some(hashes), Some(live)) = (words(&tg), hg.as_ref().words(), words(&lg)) else {
        return Value::from(false);
    };
    let Some(cap) = cells(table) else {
        return Value::from(false);
    };
    let Ok(s) = usize::try_from(arg(3).as_int()) else {
        return Value::from(false);
    };
    if live.get(s).is_none_or(|l| l.bits() == 0) || s >= hashes.len() {
        return Value::from(false);
    }
    let mask = cap - 1;
    let mut i = home(hashes[s].as_int(), mask);
    for _ in 0..cap {
        let e = table[i].as_int();
        if e == EMPTY {
            break;
        }
        if e == s as i64 + 1 {
            table[i] = Value::from(REMOVED);
            live[s] = Value::from(0i64);
            return Value::from(true);
        }
        i = (i + 1) & mask;
    }
    Value::from(false)
}

/// `key_index::retain(v, live) -> int`: drop the elements of `v` whose
/// slot is not live, keeping the order of the rest; the owner calls it on
/// each of its `Vec`s before [`host_compact`]. Returns the new length, or
/// `-1` (and changes nothing) when `v` is not as long as `live`.
pub fn host_retain(heap: &mut Heap, args: &[Value]) -> Value {
    let arg = |i: usize| args.get(i).copied().unwrap_or(Value::from(0i64));
    let (v, lv) = (arg(0), arg(1));
    if v.bits() == lv.bits() {
        return Value::from(-1i64);
    }
    let (Some(lg), Some(mut gc)) = (array(heap, lv), array(heap, v)) else {
        return Value::from(-1i64);
    };
    let Some(live) = lg.as_ref().words() else {
        return Value::from(-1i64);
    };
    let arr = gc.as_mut();
    if arr.len() != live.len() {
        return Value::from(-1i64);
    }
    let keep = |i: usize| live[i].bits() != 0;
    let n = if let Some(bytes) = arr.bytes_mut() {
        let mut i = 0;
        bytes.retain(|_| {
            i += 1;
            keep(i - 1)
        });
        bytes.len()
    } else {
        let w = arr.words_mut();
        let mut i = 0;
        w.retain(|_| {
            i += 1;
            keep(i - 1)
        });
        w.len()
    };
    Value::from(n as i64)
}

/// `key_index::compact(table, hashes, live) -> int`: drop the removed
/// slots, renumbering the rest in order (as [`host_retain`] did to the
/// owner's `Vec`s), and rebuild the table. Returns the live count.
pub fn host_compact(heap: &mut Heap, args: &[Value]) -> Value {
    let arg = |i: usize| args.get(i).copied().unwrap_or(Value::from(0i64));
    let (t, hs, lv) = (arg(0), arg(1), arg(2));
    if t.bits() == hs.bits() || t.bits() == lv.bits() || hs.bits() == lv.bits() {
        return Value::from(-1i64);
    }
    let (Some(tg), Some(hg), Some(lg)) = (array(heap, t), array(heap, hs), array(heap, lv)) else {
        return Value::from(-1i64);
    };
    let (Some(table), Some(hashes), Some(live)) = (words(&tg), words(&hg), words(&lg)) else {
        return Value::from(-1i64);
    };
    if cells(table).is_none() || hashes.len() != live.len() {
        return Value::from(-1i64);
    }
    let mut i = 0;
    hashes.retain(|_| {
        i += 1;
        live[i - 1].bits() != 0
    });
    live.clear();
    live.resize(hashes.len(), Value::from(1i64));
    rebuild(table, hashes, live, 0);
    for mut gc in [tg, hg, lg] {
        heap.account_array(gc.as_mut());
    }
    Value::from(hashes.len() as i64)
}

/// `key_index::clear(table, hashes, live) -> unit`: no entries, the table
/// keeps its size.
pub fn host_clear(heap: &mut Heap, args: &[Value]) -> Value {
    let arg = |i: usize| args.get(i).copied().unwrap_or(Value::from(0i64));
    let (t, hs, lv) = (arg(0), arg(1), arg(2));
    if let Some(tg) = array(heap, t)
        && let Some(table) = words(&tg)
        && cells(table).is_some()
    {
        table.fill(Value::from(EMPTY));
    }
    for v in [hs, lv] {
        if v.bits() != t.bits()
            && let Some(mut gc) = array(heap, v)
        {
            gc.as_mut().clear();
        }
    }
    Value::from(0i64)
}

/// HostInvoke wiring for `key_index::*` (archive 6.19), after
/// `entropy_bytes`.
pub const KEY_INDEX_WIRING: &[(&str, usize, crate::HostValueFn)] = &[
    (common::KEY_INDEX_TABLE_NATIVE, 1, host_table),
    (common::KEY_INDEX_PROBE_NATIVE, 4, host_probe),
    (common::KEY_INDEX_INSERT_NATIVE, 4, host_insert),
    (common::KEY_INDEX_REMOVE_NATIVE, 4, host_remove),
    (common::KEY_INDEX_COMPACT_NATIVE, 3, host_compact),
    (common::KEY_INDEX_RETAIN_NATIVE, 2, host_retain),
    (common::KEY_INDEX_CLEAR_NATIVE, 3, host_clear),
    (common::KEY_INDEX_FIND_NATIVE, 6, host_find),
    (common::KEY_INDEX_EQ_MODE_NATIVE, 1, host_eq_mode),
];

#[cfg(test)]
#[path = "key_index.tests.rs"]
mod tests;
