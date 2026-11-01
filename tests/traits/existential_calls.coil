// Trait methods on a bare-class existential on direct MIR: the pack's
// boxed value, the further arguments, the pack's dictionary, then the
// code pointer in the method's slot. Packs stay live across allocating
// calls; a pack made from a call is evaluated once.
use tests::assert;

trait Greet<G> {
    fn greet(G self, string who, int times) -> string {}
}

class Polite {
    pub word: string,
}

impl Greet for Polite {
    fn greet(Polite self, string who, int times) -> string {
        let out = "";
        let i = 0;
        while i < times {
            out = out + self.word + " " + who + ";";
            i += 1;
        }
        return out;
    }
}

impl Greet for int {
    fn greet(int self, string who, int times) -> string {
        return who + (self * times).show();
    }
}

fn twice(Greet g, string who) -> string {
    let a = g.greet(who, 1);
    let b = g.greet(who + "!", 2);
    return a + "|" + b;
}

class Counter {
    pub n: int,
}

fn make(string word, Counter made) -> Greet {
    made.n += 1;
    return new Polite(word);
}

test("an existential parameter called twice") {
    assert!(twice(new Polite("hi"), "bo") == "hi bo;|hi bo!;hi bo!;");
    assert!(twice(7, "n") == "n7|n!14");
}

test("a pack from a call is made once") {
    let made = new Counter(0);
    let s = make("yo", made).greet("x", 3);
    assert!(s == "yo x;yo x;yo x;");
    assert!(made.n == 1);
}

test("an existential local across a loop") {
    let g: Greet = new Polite("hey");
    let total = 0;
    let i = 0;
    while i < 20 {
        total += g.greet("abc", 2).len();
        i += 1;
    }
    assert!(total == 20 * 16);
}
