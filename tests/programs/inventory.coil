// An inventory class: find returning `Option`, restock raising on a missing item, filtering names into a `Vec` and a float total.
// @output 3 items, 2 low: [bolt, nut] value=41.5 restocked bolt=10 err:unknown item 'gear'\n
use io::sync::println;
use string::format;

#[derive(Show, Eq)]
class Item {
    pub name: string,
    pub qty: int,
    pub price: float,
}

class Inventory {
    items: Vec<Item>,
}

impl Inventory {
    pub static fn new() -> Inventory {
        return new Inventory(Vec::new());
    }
    pub fn add(string name, int qty, float price) {
        self.items.push(new Item(name, qty, price));
    }
    pub fn find(string name) -> Option<Item> {
        for it in self.items {
            if it.name == name {
                return Option::Some(it);
            }
        }
        return Option::None;
    }
    pub fn restock(string name, int by) -> Result<int, string> {
        // `find` hands back a copy-on-write share: write through a borrow.
        for mut it in self.items {
            if it.name == name {
                it.qty += by;
                return it.qty;
            }
        }
        raise format("unknown item '%s'", name);
    }
    pub fn low(int threshold) -> Vec<string> {
        let out: Vec<string> = Vec::new();
        for it in self.items {
            if it.qty < threshold {
                out.push(it.name);
            }
        }
        return out;
    }
    pub fn value() -> float {
        let t = 0.0;
        for it in self.items {
            t += (it.qty as float) * it.price;
        }
        return t;
    }
    pub fn count() -> int {
        return self.items.len();
    }
}

fn main() {
    let inv = Inventory::new();
    inv.add("bolt", 2, 0.5);
    inv.add("nut", 3, 0.5);
    inv.add("plate", 7, 5.0);
    let low = inv.low(5);
    let r = inv.restock("bolt", 8).unwrap_or(-1);
    let e = match inv.restock("gear", 1) {
        Result::Ok(n) => format("%i", n),
        Result::Err(m) => "err:" + m,
    };
    let _ = println(
        format(
            "%i items, %i low: %v value=%f restocked bolt=%i %s",
            inv.count(),
            low.len(),
            low,
            inv.value(),
            r,
            e,
        ),
    );
}
