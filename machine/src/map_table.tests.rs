use super::*;

fn int_vec(heap: &mut Heap, xs: &[i64]) -> Value {
    let (obj, _) = heap.alloc(ObjArray::new(xs.iter().map(|&x| Value::from(x)).collect()), Object::Array);
    Value::from(obj.addr())
}

fn ints(heap: &Heap, v: Value) -> Vec<i64> {
    array(heap, v).unwrap().as_ref().iter().map(|x| x.as_int()).collect()
}

/// An int-keyed map's table and keys, as `KeyIndex` keeps them.
struct Ix {
    ix: Value,
    keys: Vec<i64>,
}

impl Ix {
    fn new(heap: &mut Heap, n: i64) -> Self {
        Self { ix: host_map_table(heap, &[Value::from(n)]), keys: Vec::new() }
    }

    fn keys(&self, heap: &mut Heap) -> Value {
        int_vec(heap, &self.keys)
    }

    fn find(&self, heap: &mut Heap, k: i64) -> i64 {
        let keys = self.keys(heap);
        host_map_find(heap, &[self.ix, keys, Value::from(k), Value::from(k), Value::from(common::KEY_EQ_WORD)]).as_int()
    }

    /// `HashMap::insert`'s shape: the slot, and whether the key is new.
    fn upsert(&mut self, heap: &mut Heap, k: i64) -> (i64, bool) {
        let keys = self.keys(heap);
        let r = host_map_upsert(heap, &[self.ix, keys, Value::from(k), Value::from(k), Value::from(common::KEY_EQ_WORD)]).as_int();
        if r >= 0 {
            return (r, false);
        }
        self.keys.push(k);
        (-1 - r, true)
    }

    fn remove(&self, heap: &mut Heap, s: i64) -> bool {
        host_map_remove(heap, &[self.ix, Value::from(s)]).bits() != 0
    }

    fn live(&self, heap: &mut Heap, s: i64) -> bool {
        host_map_live(heap, &[self.ix, Value::from(s)]).bits() != 0
    }

    fn len(&self, heap: &mut Heap) -> i64 {
        host_map_len(heap, &[self.ix]).as_int()
    }

    fn cap(&self, heap: &Heap) -> usize {
        ints(heap, self.ix)[0] as usize + 1
    }

    /// `KeyIndex::compact`: the keys of live slots, then the table.
    fn compact(&mut self, heap: &mut Heap) {
        let live: Vec<bool> = (0..self.keys.len() as i64).map(|s| self.live(heap, s)).collect();
        let mut i = 0;
        self.keys.retain(|_| {
            i += 1;
            live[i - 1]
        });
        host_map_compact(heap, &[self.ix]);
    }
}

#[test]
fn a_new_table_has_room_for_its_count() {
    let mut heap = Heap::default();
    for (n, cap) in [(0, 8), (3, 8), (4, 16), (100, 256)] {
        let t = Ix::new(&mut heap, n);
        let words = ints(&heap, t.ix);
        assert_eq!(words.len(), HEADER + 2 * cap, "n={n}");
        assert_eq!(&words[..HEADER], &[cap as i64 - 1, 0, 0, 0]);
    }
}

#[test]
fn upsert_finds_or_adds_in_insertion_order_and_grows() {
    let mut heap = Heap::default();
    let mut t = Ix::new(&mut heap, 0);
    for k in 0..1000 {
        assert_eq!(t.upsert(&mut heap, k * 7919), (k, true));
    }
    assert!(t.cap(&heap) >= 2000);
    for k in 0..1000 {
        assert_eq!(t.upsert(&mut heap, k * 7919), (k, false));
        assert_eq!(t.find(&mut heap, k * 7919), k);
    }
    assert_eq!(t.find(&mut heap, 1), -1);
    assert_eq!(t.len(&mut heap), 1000);
}

#[test]
fn colliding_hashes_compare_keys() {
    let mut heap = Heap::default();
    let mut t = Ix::new(&mut heap, 0);
    // Same probe start: 8 cells, keys differing only above the mask's bits
    // after the fold still land apart; equal hashes need the key compare.
    let keys = int_vec(&mut heap, &[10, 20]);
    let ix = t.ix;
    for (s, _) in [10, 20].iter().enumerate() {
        let r = host_map_add(&mut heap, &[ix, Value::from(5i64)]).as_int();
        assert_eq!(r, s as i64);
    }
    t.keys = vec![10, 20];
    let find = |heap: &mut Heap, k: i64| host_map_find(heap, &[ix, keys, Value::from(5i64), Value::from(k), Value::from(common::KEY_EQ_WORD)]).as_int();
    assert_eq!((find(&mut heap, 10), find(&mut heap, 20), find(&mut heap, 30)), (0, 1, -1));
    // `probe` gives both candidates in probe order, then nothing.
    let probe = |heap: &mut Heap, after: i64| host_map_probe(heap, &[ix, Value::from(5i64), Value::from(after)]).as_int();
    assert_eq!((probe(&mut heap, -1), probe(&mut heap, 0), probe(&mut heap, 1)), (0, 1, -1));
}

#[test]
fn remove_leaves_a_tombstone_that_a_later_insert_reuses() {
    let mut heap = Heap::default();
    let mut t = Ix::new(&mut heap, 0);
    for k in 0..3 {
        t.upsert(&mut heap, k);
    }
    assert!(t.remove(&mut heap, 1));
    assert!(!t.remove(&mut heap, 1));
    assert!(!t.live(&mut heap, 1) && t.live(&mut heap, 0) && t.live(&mut heap, 2));
    assert_eq!((t.find(&mut heap, 1), t.find(&mut heap, 2), t.len(&mut heap)), (-1, 2, 2));
    let used = ints(&heap, t.ix)[1];
    // Key 1 again: a new slot, in the tombstone's cell (no new cell used).
    assert_eq!(t.upsert(&mut heap, 1), (3, true));
    assert_eq!(ints(&heap, t.ix)[1], used);
    assert_eq!(t.find(&mut heap, 1), 3);
}

#[test]
fn compact_renumbers_the_live_slots_in_order() {
    let mut heap = Heap::default();
    let mut t = Ix::new(&mut heap, 0);
    for k in 0..40 {
        t.upsert(&mut heap, k);
    }
    for s in (0..40).step_by(2) {
        assert!(t.remove(&mut heap, s));
    }
    t.compact(&mut heap);
    assert_eq!(t.keys, (0..40).filter(|k| k % 2 == 1).collect::<Vec<_>>());
    for (s, k) in t.keys.clone().into_iter().enumerate() {
        assert_eq!(t.find(&mut heap, k), s as i64);
        assert!(t.live(&mut heap, s as i64));
    }
    assert_eq!((t.find(&mut heap, 0), t.len(&mut heap)), (-1, 20));
    assert_eq!(ints(&heap, t.ix)[1], 20);
}

#[test]
fn growth_keeps_removed_slots_numbered() {
    let mut heap = Heap::default();
    let mut t = Ix::new(&mut heap, 0);
    for k in 0..3 {
        t.upsert(&mut heap, k);
    }
    t.remove(&mut heap, 0);
    for k in 3..100 {
        assert_eq!(t.upsert(&mut heap, k), (k, true));
    }
    assert!(!t.live(&mut heap, 0));
    assert_eq!((t.find(&mut heap, 2), t.find(&mut heap, 99), t.find(&mut heap, 0)), (2, 99, -1));
}

#[test]
fn clear_empties_and_keeps_the_size() {
    let mut heap = Heap::default();
    let mut t = Ix::new(&mut heap, 0);
    for k in 0..50 {
        t.upsert(&mut heap, k);
    }
    let cap = t.cap(&heap);
    host_map_clear(&mut heap, &[t.ix]);
    t.keys.clear();
    assert_eq!((t.len(&mut heap), t.cap(&heap), t.find(&mut heap, 3)), (0, cap, -1));
    assert_eq!(ints(&heap, t.ix).len(), HEADER + 2 * cap);
    assert_eq!(t.upsert(&mut heap, 3), (0, true));
}

#[test]
fn a_malformed_table_finds_nothing_and_adds_nothing() {
    let mut heap = Heap::default();
    let keys = int_vec(&mut heap, &[1]);
    for bad in [&[][..], &[6, 0, 0, 0][..], &[7, 0, 0, 0, 0][..]] {
        let ix = int_vec(&mut heap, bad);
        let find = host_map_find(&mut heap, &[ix, keys, Value::from(1i64), Value::from(1i64), Value::from(common::KEY_EQ_WORD)]);
        assert_eq!(find.as_int(), -1);
        assert_eq!(host_map_add(&mut heap, &[ix, Value::from(1i64)]).as_int(), -1);
        assert!(host_map_live(&mut heap, &[ix, Value::from(0i64)]).bits() == 0);
        assert_eq!(ints(&heap, ix), bad.to_vec());
    }
}
