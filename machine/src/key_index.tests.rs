use super::*;

fn int_vec(heap: &mut Heap, xs: &[i64]) -> Value {
    let (obj, _) = heap.alloc(ObjArray::new(xs.iter().map(|&x| Value::from(x)).collect()), Object::Array);
    Value::from(obj.addr())
}

fn ints(heap: &Heap, v: Value) -> Vec<i64> {
    array(heap, v).unwrap().as_ref().iter().map(|x| x.as_int()).collect()
}

struct Index {
    table: Value,
    hashes: Value,
    live: Value,
}

impl Index {
    fn new(heap: &mut Heap, n: i64) -> Self {
        let table = host_table(heap, &[Value::from(n)]);
        let hashes = int_vec(heap, &[]);
        let live = int_vec(heap, &[]);
        Self { table, hashes, live }
    }

    fn insert(&self, heap: &mut Heap, h: i64) -> i64 {
        host_insert(heap, &[self.table, self.hashes, self.live, Value::from(h)]).as_int()
    }

    fn probe(&self, heap: &mut Heap, h: i64, after: i64) -> i64 {
        host_probe(heap, &[self.table, self.hashes, Value::from(h), Value::from(after)]).as_int()
    }

    /// Every slot with hash `h`, in probe order.
    fn all(&self, heap: &mut Heap, h: i64) -> Vec<i64> {
        let mut out = Vec::new();
        let mut s = self.probe(heap, h, -1);
        while s >= 0 {
            out.push(s);
            s = self.probe(heap, h, s);
        }
        out
    }

    fn remove(&self, heap: &mut Heap, s: i64) -> bool {
        host_remove(heap, &[self.table, self.hashes, self.live, Value::from(s)]).bits() != 0
    }

    fn cap(&self, heap: &Heap) -> usize {
        ints(heap, self.table).len() - 1
    }
}

#[test]
fn a_new_table_has_room_for_its_count() {
    let mut heap = Heap::default();
    for (n, cap) in [(0, 8), (3, 8), (4, 16), (100, 256)] {
        let t = host_table(&mut heap, &[Value::from(n)]);
        let cells = ints(&heap, t);
        assert_eq!(cells.len(), cap + 1, "n={n}");
        assert!(cells.iter().all(|&c| c == 0));
    }
}

#[test]
fn inserted_slots_are_found_by_hash_in_order() {
    let mut heap = Heap::default();
    let ix = Index::new(&mut heap, 0);
    for h in 0..1000 {
        assert_eq!(ix.insert(&mut heap, h * 7919), h);
    }
    for h in 0..1000 {
        assert_eq!(ix.all(&mut heap, h * 7919), vec![h]);
    }
    assert_eq!(ix.probe(&mut heap, 3, -1), -1);
    assert!(ix.cap(&heap) >= 2002);
}

#[test]
fn equal_hashes_are_all_candidates() {
    let mut heap = Heap::default();
    let ix = Index::new(&mut heap, 0);
    let a = ix.insert(&mut heap, 42);
    let b = ix.insert(&mut heap, 7);
    let c = ix.insert(&mut heap, 42);
    let d = ix.insert(&mut heap, 42);
    assert_eq!(ix.all(&mut heap, 42), vec![a, c, d]);
    assert_eq!(ix.all(&mut heap, 7), vec![b]);
    assert!(ix.remove(&mut heap, c));
    assert_eq!(ix.all(&mut heap, 42), vec![a, d]);
}

#[test]
fn a_removed_slot_is_gone_and_its_place_reused() {
    let mut heap = Heap::default();
    let ix = Index::new(&mut heap, 0);
    let a = ix.insert(&mut heap, 1);
    let b = ix.insert(&mut heap, 9);
    assert!(ix.remove(&mut heap, a));
    assert!(!ix.remove(&mut heap, a));
    assert!(!ix.remove(&mut heap, 99));
    assert_eq!(ix.probe(&mut heap, 1, -1), -1);
    assert_eq!(ix.all(&mut heap, 9), vec![b]);
    assert_eq!(ints(&heap, ix.live), vec![0, 1]);
    // The removed place takes the next insert on its probe sequence.
    let c = ix.insert(&mut heap, 1);
    assert_eq!(c, 2);
    let cells = ints(&heap, ix.table);
    assert!(!cells[..8].contains(&-1));
    assert_eq!(cells[8], 2);
}

#[test]
fn removes_and_inserts_never_fill_the_table() {
    let mut heap = Heap::default();
    let ix = Index::new(&mut heap, 0);
    for round in 0..200 {
        let s = ix.insert(&mut heap, round);
        assert!(ix.remove(&mut heap, s));
    }
    let cap = ix.cap(&heap);
    let cells = ints(&heap, ix.table);
    assert!(cells[..cap].contains(&0), "no empty cell left");
    assert!(cap <= 16, "only removed entries: cap {cap}");
    assert_eq!(ix.probe(&mut heap, 5, -1), -1);
}

#[test]
fn retain_then_compact_renumbers_in_order() {
    let mut heap = Heap::default();
    let ix = Index::new(&mut heap, 0);
    for h in 0..10 {
        ix.insert(&mut heap, 100 + h);
    }
    for s in [0, 3, 4, 9] {
        assert!(ix.remove(&mut heap, s));
    }
    let keys = int_vec(&mut heap, &(0..10).collect::<Vec<_>>());
    assert_eq!(host_retain(&mut heap, &[keys, ix.live]).as_int(), 6);
    assert_eq!(ints(&heap, keys), vec![1, 2, 5, 6, 7, 8]);
    assert_eq!(host_compact(&mut heap, &[ix.table, ix.hashes, ix.live]).as_int(), 6);
    assert_eq!(ints(&heap, ix.hashes), vec![101, 102, 105, 106, 107, 108]);
    assert_eq!(ints(&heap, ix.live), vec![1; 6]);
    for (s, h) in [101, 102, 105, 106, 107, 108].into_iter().enumerate() {
        assert_eq!(ix.all(&mut heap, h), vec![s as i64]);
    }
    assert_eq!(ix.probe(&mut heap, 100, -1), -1);
}

#[test]
fn retain_refuses_a_vec_of_another_length() {
    let mut heap = Heap::default();
    let ix = Index::new(&mut heap, 0);
    ix.insert(&mut heap, 1);
    let keys = int_vec(&mut heap, &[1, 2]);
    assert_eq!(host_retain(&mut heap, &[keys, ix.live]).as_int(), -1);
    assert_eq!(ints(&heap, keys), vec![1, 2]);
}

#[test]
fn retain_keeps_packed_bytes_packed() {
    let mut heap = Heap::default();
    let live = int_vec(&mut heap, &[1, 0, 1]);
    let (obj, _) = heap.alloc(ObjArray::from_bytes(vec![7, 8, 9]), Object::Array);
    let bytes = Value::from(obj.addr());
    assert_eq!(host_retain(&mut heap, &[bytes, live]).as_int(), 2);
    let arr = array(&heap, bytes).unwrap();
    assert_eq!(arr.as_ref().bytes(), Some(&[7u8, 9][..]));
}

#[test]
fn clear_empties_and_keeps_the_size() {
    let mut heap = Heap::default();
    let ix = Index::new(&mut heap, 0);
    for h in 0..20 {
        ix.insert(&mut heap, h);
    }
    let cap = ix.cap(&heap);
    host_clear(&mut heap, &[ix.table, ix.hashes, ix.live]);
    assert_eq!(ix.cap(&heap), cap);
    assert!(ints(&heap, ix.hashes).is_empty());
    assert_eq!(ix.probe(&mut heap, 3, -1), -1);
    assert_eq!(ix.insert(&mut heap, 3), 0);
    assert_eq!(ix.all(&mut heap, 3), vec![0]);
}

#[test]
fn malformed_arguments_change_nothing() {
    let mut heap = Heap::default();
    let ix = Index::new(&mut heap, 0);
    let bad = int_vec(&mut heap, &[0, 0, 0, 0]);
    assert_eq!(host_insert(&mut heap, &[bad, ix.hashes, ix.live, Value::from(1i64)]).as_int(), -1);
    assert_eq!(host_insert(&mut heap, &[ix.table, ix.table, ix.live, Value::from(1i64)]).as_int(), -1);
    assert_eq!(host_probe(&mut heap, &[bad, ix.hashes, Value::from(1i64), Value::from(-1i64)]).as_int(), -1);
    assert_eq!(host_probe(&mut heap, &[Value::from(0i64), ix.hashes, Value::from(1i64), Value::from(-1i64)]).as_int(), -1);
    assert!(ints(&heap, ix.hashes).is_empty());
}
