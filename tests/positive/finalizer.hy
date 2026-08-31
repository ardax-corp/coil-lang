use gc::{collect, get, root, weak, upgrade, Root, Weak};

class Handle {
    pub fd: int,
}

class Resurrect {
    pub fd: int,
}

class Rooted {
    pub fd: int,
}

class Fielded {
    pub fd: int,
}

class Bag {
    pub slot: Option<Fielded>,
}

class WeakResurrect {
    pub fd: int,
}

static let drops: int = 0;
static let during: int = 0;
static let held: Option<Weak<Handle>> = Option::None;
static let resurrect_drops: int = 0;
static let kept: Option<Resurrect> = Option::None;
static let root_drops: int = 0;
static let kept_root: Option<Root<Rooted>> = Option::None;
static let field_drops: int = 0;
static let bag: Option<Bag> = Option::None;
static let weak_drops: int = 0;
static let kept_weak: Option<WeakResurrect> = Option::None;
static let held_weak: Option<Weak<WeakResurrect>> = Option::None;

impl Handle {
    fn drop() {
        let live = match held {
            Option::Some(w) => match upgrade(w) {
                Option::Some(_) => 1,
                Option::None => 2,
            },
            Option::None => 0,
        };
        during = live;
        collect();
        drops = drops + 1;
    }
}

impl Resurrect {
    fn drop() {
        resurrect_drops = resurrect_drops + 1;
        kept = Option::Some(self);
    }
}

impl Rooted {
    fn drop() {
        root_drops = root_drops + 1;
        kept_root = Option::Some(root(self));
    }
}

impl Bag {
    pub fn put(Fielded h) {
        self.slot = Option::Some(h);
    }
    pub fn fd() -> int {
        return match self.slot {
            Option::Some(h) => h.fd,
            Option::None => -1,
        };
    }
}

impl Fielded {
    fn drop() {
        field_drops = field_drops + 1;
        match bag {
            Option::Some(b) => b.put(self),
            Option::None => (),
        };
    }
}

impl WeakResurrect {
    fn drop() {
        weak_drops = weak_drops + 1;
        kept_weak = Option::Some(self);
    }
}

fn ephemeral() {
    let h = new Handle(1);
}

fn with_weak() {
    let h = new Handle(5);
    held = Option::Some(weak(h));
}

test("drop runs on collect") {
    drops = 0;
    ephemeral();
    collect();
    assert(drops == 1)?;
}

test("explicit drop counts once") {
    drops = 0;
    let h = new Handle(2);
    h.drop();
    h.drop();
    collect();
    assert(drops == 1)?;
}

test("live Root is not finalized") {
    drops = 0;
    let h = new Handle(3);
    let r = root(h);
    collect();
    assert(drops == 0)?;
}

test("weak stays live during drop and is None after sweep") {
    drops = 0;
    during = 0;
    with_weak();
    collect();
    assert(during == 1)?;
    let after = match held {
        Option::Some(w) => match upgrade(w) {
            Option::Some(_) => 1,
            Option::None => 0,
        },
        Option::None => -1,
    };
    assert(after == 0)?;
}

fn stash_self() {
    let h = new Resurrect(42);
}

fn resurrected_fd() -> int {
    return match kept {
        Option::Some(h) => h.fd,
        Option::None => -1,
    };
}

fn clear_kept() {
    kept = Option::None;
}

test("storing self from drop resurrects once") {
    resurrect_drops = 0;
    kept = Option::None;
    stash_self();
    collect();
    assert(resurrect_drops == 1)?;
    assert(resurrected_fd() == 42)?;
    clear_kept();
    collect();
    assert(resurrect_drops == 1)?;
}

fn explicit_stash() {
    let h = new Resurrect(7);
    h.drop();
}

test("explicit drop storing self stays once") {
    resurrect_drops = 0;
    kept = Option::None;
    explicit_stash();
    collect();
    assert(resurrect_drops == 1)?;
    clear_kept();
    collect();
    assert(resurrect_drops == 1)?;
}

fn stash_into_root() {
    let h = new Rooted(42);
}

test("storing self into Root from drop resurrects") {
    root_drops = 0;
    kept_root = Option::None;
    stash_into_root();
    collect();
    assert(root_drops == 1)?;
    let fd = match kept_root {
        Option::Some(r) => match get(r) {
            Option::Some(h) => h.fd,
            Option::None => -1,
        },
        Option::None => -1,
    };
    assert(fd == 42)?;
    collect();
    assert(root_drops == 1)?;
    let fd2 = match kept_root {
        Option::Some(r) => match get(r) {
            Option::Some(h) => h.fd,
            Option::None => -1,
        },
        Option::None => -1,
    };
    assert(fd2 == 42)?;
}

fn setup_bag() {
    bag = Option::Some(new Bag(Option::None));
}

fn stash_into_field() {
    let h = new Fielded(9);
}

fn field_slot_fd() -> int {
    return match bag {
        Option::Some(b) => b.fd(),
        Option::None => -2,
    };
}

test("storing self into reachable field resurrects") {
    field_drops = 0;
    bag = Option::None;
    setup_bag();
    stash_into_field();
    collect();
    assert(field_drops == 1)?;
    assert(field_slot_fd() == 9)?;
    collect();
    assert(field_drops == 1)?;
}

fn stash_with_weak() {
    let h = new WeakResurrect(3);
    held_weak = Option::Some(weak(h));
}

test("resurrection keeps weak upgradable") {
    weak_drops = 0;
    kept_weak = Option::None;
    held_weak = Option::None;
    stash_with_weak();
    collect();
    assert(weak_drops == 1)?;
    let after = match held_weak {
        Option::Some(w) => match upgrade(w) {
            Option::Some(h) => h.fd,
            Option::None => -1,
        },
        Option::None => -2,
    };
    assert(after == 3)?;
}
