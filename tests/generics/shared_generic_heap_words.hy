// A shared generic body (another module's generic function) moves `T`
// words that are heap references: its frame maps root them across the
// allocations it makes (#1955). Under the gc-stress harness every
// allocation collects, so an unrooted word would be swept.
use tests::assert;
use string::format;
use collections::vec::filter;
use collections::vec::map;
use collections::map::HashMap;

class Box2 {
    pub label: string,
}

test("map and filter keep fresh strings and instances alive") {
    let xs: Vec<int> = [];
    let i = 0;
    while i < 64 {
        xs.push(i);
        i = i + 1;
    }
    let words = map(xs, fn (int x) => format("w%i", x));
    let boxes = map(words, fn (string w) => new Box2(w + "!"));
    let kept = filter(boxes, fn (Box2 b) => b.label.len() > 3);
    assert!(words.len() == 64);
    assert!(words[63] == "w63");
    assert!(boxes[10].label == "w10!");
    assert!(kept.len() == 54);
    assert!(kept[0].label == "w10!");
}

test("HashMap insert keeps fresh keys and values alive") {
    let m: HashMap<string, string> = HashMap::new();
    let i = 0;
    while i < 48 {
        m.insert(format("k%i", i), format("v%i", i * 2));
        i = i + 1;
    }
    assert!(m.size() == 48);
    match m.get("k47") {
        Option::Some(v) => {
            assert!(v == "v94");
        },
        Option::None => {
            assert!(false, "lost k47");
        },
    }
}
