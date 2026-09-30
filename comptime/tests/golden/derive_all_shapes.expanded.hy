use string::format;

class Pt {
    pub x: int,
    pub y: int,
}

impl Show for Pt {
    fn show(Pt __show_Pt) -> string {
        return string::format("Pt { x: %v, y: %v }", __show_Pt.x, __show_Pt.y);
    }
}

impl Eq for Pt {
    fn eq(Pt __eq_a_Pt, Pt __eq_b_Pt) -> bool {
        return (__eq_a_Pt.x == __eq_b_Pt.x) && (__eq_a_Pt.y == __eq_b_Pt.y);
    }
    fn ne(Pt __eq_a_Pt, Pt __eq_b_Pt) -> bool {
        return !(__eq_a_Pt == __eq_b_Pt);
    }
}

impl Lt for Pt {
    fn lt(Pt __ord_lt_a_Pt, Pt __ord_lt_b_Pt) -> bool {
        return (__ord_lt_a_Pt.x < __ord_lt_b_Pt.x) ||
               ((__ord_lt_a_Pt.x == __ord_lt_b_Pt.x) &&
                ((__ord_lt_a_Pt.y < __ord_lt_b_Pt.y) ||
                 ((__ord_lt_a_Pt.y == __ord_lt_b_Pt.y) && false)));
    }
}

impl Le for Pt {
    fn le(Pt __ord_le_a_Pt, Pt __ord_le_b_Pt) -> bool {
        return (__ord_le_a_Pt.x < __ord_le_b_Pt.x) ||
               ((__ord_le_a_Pt.x == __ord_le_b_Pt.x) &&
                ((__ord_le_a_Pt.y < __ord_le_b_Pt.y) ||
                 ((__ord_le_a_Pt.y == __ord_le_b_Pt.y) && true)));
    }
}

impl Gt for Pt {
    fn gt(Pt __ord_gt_a_Pt, Pt __ord_gt_b_Pt) -> bool {
        return (__ord_gt_a_Pt.x > __ord_gt_b_Pt.x) ||
               ((__ord_gt_a_Pt.x == __ord_gt_b_Pt.x) &&
                ((__ord_gt_a_Pt.y > __ord_gt_b_Pt.y) ||
                 ((__ord_gt_a_Pt.y == __ord_gt_b_Pt.y) && false)));
    }
}

impl Ge for Pt {
    fn ge(Pt __ord_ge_a_Pt, Pt __ord_ge_b_Pt) -> bool {
        return (__ord_ge_a_Pt.x > __ord_ge_b_Pt.x) ||
               ((__ord_ge_a_Pt.x == __ord_ge_b_Pt.x) &&
                ((__ord_ge_a_Pt.y > __ord_ge_b_Pt.y) ||
                 ((__ord_ge_a_Pt.y == __ord_ge_b_Pt.y) && true)));
    }
}

impl Ord for Pt {
}

impl Default for Pt {
    static fn default() -> Pt {
        return new Pt(0, 0);
    }
}

impl Hash for Pt {
    fn hash(Pt __hash_Pt) -> int {
        return (__hash_Pt.x.hash() * 31) + __hash_Pt.y.hash();
    }
}

impl String for Pt {
    fn to_string(Pt __str_Pt) -> string {
        return string::format("Pt { x: %v, y: %v }", __str_Pt.x, __str_Pt.y);
    }
}

impl Send for Pt {
}

impl Sensitive for Pt {
}

class Unit {}

impl Show for Unit {
    fn show(Unit __show_Unit) -> string {
        return string::format("Unit {  }");
    }
}

impl Eq for Unit {
    fn eq(Unit __eq_a_Unit, Unit __eq_b_Unit) -> bool {
        return true;
    }
    fn ne(Unit __eq_a_Unit, Unit __eq_b_Unit) -> bool {
        return !(__eq_a_Unit == __eq_b_Unit);
    }
}

impl Lt for Unit {
    fn lt(Unit __ord_lt_a_Unit, Unit __ord_lt_b_Unit) -> bool {
        return false;
    }
}

impl Le for Unit {
    fn le(Unit __ord_le_a_Unit, Unit __ord_le_b_Unit) -> bool {
        return true;
    }
}

impl Gt for Unit {
    fn gt(Unit __ord_gt_a_Unit, Unit __ord_gt_b_Unit) -> bool {
        return false;
    }
}

impl Ge for Unit {
    fn ge(Unit __ord_ge_a_Unit, Unit __ord_ge_b_Unit) -> bool {
        return true;
    }
}

impl Ord for Unit {
}

impl Default for Unit {
    static fn default() -> Unit {
        return new Unit();
    }
}

impl Hash for Unit {
    fn hash(Unit __hash_Unit) -> int {
        return 0;
    }
}

impl String for Unit {
    fn to_string(Unit __str_Unit) -> string {
        return string::format("Unit {  }");
    }
}

enum Sh {
    Dot,
    Circle(int),
    Pair(int, string),
    Rect { w: int, h: int },
}

impl Show for Sh {
    fn show(Sh __show_Sh) -> string {
        return match __show_Sh {
            Sh::Dot => string::format("Sh::Dot"),
            Sh::Circle(s_p0) => string::format("Sh::Circle(%v)", s_p0),
            Sh::Pair(s_p0, s_p1) => string::format("Sh::Pair(%v, %v)", s_p0, s_p1),
            Sh::Rect{ w: _, h: _ } => string::format(
                "Sh::Rect { w: %v, h: %v }",
                __show_Sh.w,
                __show_Sh.h,
            ),
        };
    }
}

impl Eq for Sh {
    fn eq(Sh __eq_a_Sh, Sh __eq_b_Sh) -> bool {
        return match __eq_a_Sh {
            Sh::Dot => match __eq_b_Sh {
                Sh::Dot => true,
                default => false,
            },
            Sh::Circle(a_p0) => match __eq_b_Sh {
                Sh::Circle(b_p0) => (a_p0 == b_p0),
                default => false,
            },
            Sh::Pair(a_p0, a_p1) => match __eq_b_Sh {
                Sh::Pair(b_p0, b_p1) => ((a_p0 == b_p0) && (a_p1 == b_p1)),
                default => false,
            },
            Sh::Rect{ w: _, h: _ } => match __eq_b_Sh {
                Sh::Rect{ w: _, h: _ } => ((__eq_a_Sh.w == __eq_b_Sh.w) &&
                                           (__eq_a_Sh.h == __eq_b_Sh.h)),
                default => false,
            },
            default => false,
        };
    }
    fn ne(Sh __eq_a_Sh, Sh __eq_b_Sh) -> bool {
        return !(__eq_a_Sh == __eq_b_Sh);
    }
}

impl Lt for Sh {
    fn lt(Sh __ord_lt_a_Sh, Sh __ord_lt_b_Sh) -> bool {
        return match __ord_lt_a_Sh {
            Sh::Dot => match __ord_lt_b_Sh {
                Sh::Dot => false,
                Sh::Circle(_) => true,
                Sh::Pair(_, _) => true,
                Sh::Rect{ w: _, h: _ } => true,
                default => false,
            },
            Sh::Circle(a_p0) => match __ord_lt_b_Sh {
                Sh::Dot => false,
                Sh::Circle(b_p0) => ((a_p0 < b_p0) || ((a_p0 == b_p0) && false)),
                Sh::Pair(_, _) => true,
                Sh::Rect{ w: _, h: _ } => true,
                default => false,
            },
            Sh::Pair(a_p0, a_p1) => match __ord_lt_b_Sh {
                Sh::Dot => false,
                Sh::Circle(_) => false,
                Sh::Pair(b_p0, b_p1) => ((a_p0 < b_p0) ||
                                         ((a_p0 == b_p0) &&
                                          ((a_p1 < b_p1) || ((a_p1 == b_p1) && false)))),
                Sh::Rect{ w: _, h: _ } => true,
                default => false,
            },
            Sh::Rect{ w: _, h: _ } => match __ord_lt_b_Sh {
                Sh::Dot => false,
                Sh::Circle(_) => false,
                Sh::Pair(_, _) => false,
                Sh::Rect{ w: _, h: _ } => ((__ord_lt_a_Sh.w < __ord_lt_b_Sh.w) ||
                                           ((__ord_lt_a_Sh.w == __ord_lt_b_Sh.w) &&
                                            ((__ord_lt_a_Sh.h < __ord_lt_b_Sh.h) ||
                                             ((__ord_lt_a_Sh.h == __ord_lt_b_Sh.h) && false)))),
                default => false,
            },
            default => false,
        };
    }
}

impl Le for Sh {
    fn le(Sh __ord_le_a_Sh, Sh __ord_le_b_Sh) -> bool {
        return match __ord_le_a_Sh {
            Sh::Dot => match __ord_le_b_Sh {
                Sh::Dot => true,
                Sh::Circle(_) => true,
                Sh::Pair(_, _) => true,
                Sh::Rect{ w: _, h: _ } => true,
                default => false,
            },
            Sh::Circle(a_p0) => match __ord_le_b_Sh {
                Sh::Dot => false,
                Sh::Circle(b_p0) => ((a_p0 < b_p0) || ((a_p0 == b_p0) && true)),
                Sh::Pair(_, _) => true,
                Sh::Rect{ w: _, h: _ } => true,
                default => false,
            },
            Sh::Pair(a_p0, a_p1) => match __ord_le_b_Sh {
                Sh::Dot => false,
                Sh::Circle(_) => false,
                Sh::Pair(b_p0, b_p1) => ((a_p0 < b_p0) ||
                                         ((a_p0 == b_p0) &&
                                          ((a_p1 < b_p1) || ((a_p1 == b_p1) && true)))),
                Sh::Rect{ w: _, h: _ } => true,
                default => false,
            },
            Sh::Rect{ w: _, h: _ } => match __ord_le_b_Sh {
                Sh::Dot => false,
                Sh::Circle(_) => false,
                Sh::Pair(_, _) => false,
                Sh::Rect{ w: _, h: _ } => ((__ord_le_a_Sh.w < __ord_le_b_Sh.w) ||
                                           ((__ord_le_a_Sh.w == __ord_le_b_Sh.w) &&
                                            ((__ord_le_a_Sh.h < __ord_le_b_Sh.h) ||
                                             ((__ord_le_a_Sh.h == __ord_le_b_Sh.h) && true)))),
                default => false,
            },
            default => false,
        };
    }
}

impl Gt for Sh {
    fn gt(Sh __ord_gt_a_Sh, Sh __ord_gt_b_Sh) -> bool {
        return match __ord_gt_a_Sh {
            Sh::Dot => match __ord_gt_b_Sh {
                Sh::Dot => false,
                Sh::Circle(_) => false,
                Sh::Pair(_, _) => false,
                Sh::Rect{ w: _, h: _ } => false,
                default => false,
            },
            Sh::Circle(a_p0) => match __ord_gt_b_Sh {
                Sh::Dot => true,
                Sh::Circle(b_p0) => ((a_p0 > b_p0) || ((a_p0 == b_p0) && false)),
                Sh::Pair(_, _) => false,
                Sh::Rect{ w: _, h: _ } => false,
                default => false,
            },
            Sh::Pair(a_p0, a_p1) => match __ord_gt_b_Sh {
                Sh::Dot => true,
                Sh::Circle(_) => true,
                Sh::Pair(b_p0, b_p1) => ((a_p0 > b_p0) ||
                                         ((a_p0 == b_p0) &&
                                          ((a_p1 > b_p1) || ((a_p1 == b_p1) && false)))),
                Sh::Rect{ w: _, h: _ } => false,
                default => false,
            },
            Sh::Rect{ w: _, h: _ } => match __ord_gt_b_Sh {
                Sh::Dot => true,
                Sh::Circle(_) => true,
                Sh::Pair(_, _) => true,
                Sh::Rect{ w: _, h: _ } => ((__ord_gt_a_Sh.w > __ord_gt_b_Sh.w) ||
                                           ((__ord_gt_a_Sh.w == __ord_gt_b_Sh.w) &&
                                            ((__ord_gt_a_Sh.h > __ord_gt_b_Sh.h) ||
                                             ((__ord_gt_a_Sh.h == __ord_gt_b_Sh.h) && false)))),
                default => false,
            },
            default => false,
        };
    }
}

impl Ge for Sh {
    fn ge(Sh __ord_ge_a_Sh, Sh __ord_ge_b_Sh) -> bool {
        return match __ord_ge_a_Sh {
            Sh::Dot => match __ord_ge_b_Sh {
                Sh::Dot => true,
                Sh::Circle(_) => false,
                Sh::Pair(_, _) => false,
                Sh::Rect{ w: _, h: _ } => false,
                default => false,
            },
            Sh::Circle(a_p0) => match __ord_ge_b_Sh {
                Sh::Dot => true,
                Sh::Circle(b_p0) => ((a_p0 > b_p0) || ((a_p0 == b_p0) && true)),
                Sh::Pair(_, _) => false,
                Sh::Rect{ w: _, h: _ } => false,
                default => false,
            },
            Sh::Pair(a_p0, a_p1) => match __ord_ge_b_Sh {
                Sh::Dot => true,
                Sh::Circle(_) => true,
                Sh::Pair(b_p0, b_p1) => ((a_p0 > b_p0) ||
                                         ((a_p0 == b_p0) &&
                                          ((a_p1 > b_p1) || ((a_p1 == b_p1) && true)))),
                Sh::Rect{ w: _, h: _ } => false,
                default => false,
            },
            Sh::Rect{ w: _, h: _ } => match __ord_ge_b_Sh {
                Sh::Dot => true,
                Sh::Circle(_) => true,
                Sh::Pair(_, _) => true,
                Sh::Rect{ w: _, h: _ } => ((__ord_ge_a_Sh.w > __ord_ge_b_Sh.w) ||
                                           ((__ord_ge_a_Sh.w == __ord_ge_b_Sh.w) &&
                                            ((__ord_ge_a_Sh.h > __ord_ge_b_Sh.h) ||
                                             ((__ord_ge_a_Sh.h == __ord_ge_b_Sh.h) && true)))),
                default => false,
            },
            default => false,
        };
    }
}

impl Ord for Sh {
}

impl Default for Sh {
    static fn default() -> Sh {
        return Sh::Dot;
    }
}

impl Hash for Sh {
    fn hash(Sh __hash_Sh) -> int {
        return match __hash_Sh {
            Sh::Dot => 0,
            Sh::Circle(h_p0) => ((1 * 31) + h_p0.hash()),
            Sh::Pair(h_p0, h_p1) => ((((2 * 31) + h_p0.hash()) * 31) + h_p1.hash()),
            Sh::Rect{ w: _, h: _ } => ((((3 * 31) + __hash_Sh.w.hash()) * 31) + __hash_Sh.h.hash()),
            default => 0,
        };
    }
}

impl String for Sh {
    fn to_string(Sh __str_Sh) -> string {
        return match __str_Sh {
            Sh::Dot => string::format("Sh::Dot"),
            Sh::Circle(s_p0) => string::format("Sh::Circle(%v)", s_p0),
            Sh::Pair(s_p0, s_p1) => string::format("Sh::Pair(%v, %v)", s_p0, s_p1),
            Sh::Rect{ w: _, h: _ } => string::format(
                "Sh::Rect { w: %v, h: %v }",
                __str_Sh.w,
                __str_Sh.h,
            ),
        };
    }
}

impl Send for Sh {
}

enum Holder {
    S(string),
    B(bool),
}

impl Show for Holder {
    fn show(Holder __show_Holder) -> string {
        return match __show_Holder {
            Holder::S(s_p0) => string::format("Holder::S(%v)", s_p0),
            Holder::B(s_p0) => string::format("Holder::B(%v)", s_p0),
        };
    }
}

impl Eq for Holder {
    fn eq(Holder __eq_a_Holder, Holder __eq_b_Holder) -> bool {
        return match __eq_a_Holder {
            Holder::S(a_p0) => match __eq_b_Holder {
                Holder::S(b_p0) => (a_p0 == b_p0),
                default => false,
            },
            Holder::B(a_p0) => match __eq_b_Holder {
                Holder::B(b_p0) => (a_p0 == b_p0),
                default => false,
            },
            default => false,
        };
    }
    fn ne(Holder __eq_a_Holder, Holder __eq_b_Holder) -> bool {
        return !(__eq_a_Holder == __eq_b_Holder);
    }
}

impl String for Holder {
    fn to_string(Holder __str_Holder) -> string {
        return "Holder";
    }
}

enum Status {
    Ok = 200,
    NotFound = 404,
}

impl Show for Status {
    fn show(Status __show_Status) -> string {
        let __show_n_Status: int = __show_Status;
        return __show_n_Status.show();
    }
}

impl Eq for Status {
    fn eq(Status __eq_a_Status, Status __eq_b_Status) -> bool {
        let __eq_al_Status: int = __eq_a_Status;
        let __eq_bl_Status: int = __eq_b_Status;
        return __eq_al_Status == __eq_bl_Status;
    }
    fn ne(Status __eq_a_Status, Status __eq_b_Status) -> bool {
        return !(__eq_a_Status == __eq_b_Status);
    }
}

impl Lt for Status {
    fn lt(Status __ord_lt_a_Status, Status __ord_lt_b_Status) -> bool {
        let __ord_lt_al_Status: int = __ord_lt_a_Status;
        let __ord_lt_bl_Status: int = __ord_lt_b_Status;
        return __ord_lt_al_Status < __ord_lt_bl_Status;
    }
}

impl Le for Status {
    fn le(Status __ord_le_a_Status, Status __ord_le_b_Status) -> bool {
        let __ord_le_al_Status: int = __ord_le_a_Status;
        let __ord_le_bl_Status: int = __ord_le_b_Status;
        return __ord_le_al_Status <= __ord_le_bl_Status;
    }
}

impl Gt for Status {
    fn gt(Status __ord_gt_a_Status, Status __ord_gt_b_Status) -> bool {
        let __ord_gt_al_Status: int = __ord_gt_a_Status;
        let __ord_gt_bl_Status: int = __ord_gt_b_Status;
        return __ord_gt_al_Status > __ord_gt_bl_Status;
    }
}

impl Ge for Status {
    fn ge(Status __ord_ge_a_Status, Status __ord_ge_b_Status) -> bool {
        let __ord_ge_al_Status: int = __ord_ge_a_Status;
        let __ord_ge_bl_Status: int = __ord_ge_b_Status;
        return __ord_ge_al_Status >= __ord_ge_bl_Status;
    }
}

impl Ord for Status {
}

impl Hash for Status {
    fn hash(Status __hash_Status) -> int {
        let __hash_n_Status: int = __hash_Status;
        return __hash_n_Status.hash();
    }
}

impl String for Status {
    fn to_string(Status __str_Status) -> string {
        let __str_n_Status: int = __str_Status;
        return __str_n_Status.show();
    }
}

impl Default for Status {
    static fn default() -> Status {
        return Status::Ok;
    }
}

#[repr(string)]
enum Mode {
    Read = "r",
    Write = "w",
}

impl Show for Mode {
    fn show(Mode __show_Mode) -> string {
        let __show_n_Mode: string = __show_Mode;
        return __show_n_Mode.show();
    }
}

impl Eq for Mode {
    fn eq(Mode __eq_a_Mode, Mode __eq_b_Mode) -> bool {
        let __eq_al_Mode: string = __eq_a_Mode;
        let __eq_bl_Mode: string = __eq_b_Mode;
        return __eq_al_Mode == __eq_bl_Mode;
    }
    fn ne(Mode __eq_a_Mode, Mode __eq_b_Mode) -> bool {
        return !(__eq_a_Mode == __eq_b_Mode);
    }
}

impl Lt for Mode {
    fn lt(Mode __ord_lt_a_Mode, Mode __ord_lt_b_Mode) -> bool {
        let __ord_lt_al_Mode: string = __ord_lt_a_Mode;
        let __ord_lt_bl_Mode: string = __ord_lt_b_Mode;
        return __ord_lt_al_Mode < __ord_lt_bl_Mode;
    }
}

impl Le for Mode {
    fn le(Mode __ord_le_a_Mode, Mode __ord_le_b_Mode) -> bool {
        let __ord_le_al_Mode: string = __ord_le_a_Mode;
        let __ord_le_bl_Mode: string = __ord_le_b_Mode;
        return __ord_le_al_Mode <= __ord_le_bl_Mode;
    }
}

impl Gt for Mode {
    fn gt(Mode __ord_gt_a_Mode, Mode __ord_gt_b_Mode) -> bool {
        let __ord_gt_al_Mode: string = __ord_gt_a_Mode;
        let __ord_gt_bl_Mode: string = __ord_gt_b_Mode;
        return __ord_gt_al_Mode > __ord_gt_bl_Mode;
    }
}

impl Ge for Mode {
    fn ge(Mode __ord_ge_a_Mode, Mode __ord_ge_b_Mode) -> bool {
        let __ord_ge_al_Mode: string = __ord_ge_a_Mode;
        let __ord_ge_bl_Mode: string = __ord_ge_b_Mode;
        return __ord_ge_al_Mode >= __ord_ge_bl_Mode;
    }
}

impl Ord for Mode {
}

impl Hash for Mode {
    fn hash(Mode __hash_Mode) -> int {
        let __hash_n_Mode: string = __hash_Mode;
        return __hash_n_Mode.hash();
    }
}

impl String for Mode {
    fn to_string(Mode __str_Mode) -> string {
        let __str_n_Mode: string = __str_Mode;
        return __str_n_Mode.show();
    }
}

test("class Show / String / Eq") {
    let p = new Pt(1, 2);
    assert(p.show() == "Pt { x: 1, y: 2 }", p.show())?;
    assert(p.to_string() == "Pt { x: 1, y: 2 }", p.to_string())?;
    assert(format("%v", p) == "Pt { x: 1, y: 2 }", format("%v", p))?;
    assert(p == new Pt(1, 2), "eq")?;
    assert(p != new Pt(1, 3), "ne")?;
}

test("class Ord / Hash") {
    assert(new Pt(1, 2) < new Pt(1, 3))?;
    assert(new Pt(1, 2) <= new Pt(1, 2))?;
    assert(new Pt(2, 0) > new Pt(1, 9))?;
    assert(new Pt(2, 0) >= new Pt(2, 0))?;
    assert(new Pt(1, 2).hash() == new Pt(1, 2).hash())?;
    assert(new Pt(1, 2).hash() != new Pt(2, 1).hash())?;
}

test("enum Show / String") {
    assert(Sh::Dot.show() == "Sh::Dot")?;
    assert(Sh::Circle(3).show() == "Sh::Circle(3)", Sh::Circle(3).show())?;
    assert(Sh::Pair(1, "a").show() == "Sh::Pair(1, a)", Sh::Pair(1, "a").show())?;
    assert(Sh::Rect{ w: 1, h: 2 }.to_string() == "Sh::Rect { w: 1, h: 2 }")?;
}

test("enum Eq / Ord") {
    assert(Sh::Circle(3) == Sh::Circle(3))?;
    assert(Sh::Circle(3) != Sh::Circle(4))?;
    assert(Sh::Pair(1, "a") != Sh::Pair(1, "b"))?;
    assert(Sh::Dot != Sh::Circle(0))?;
    assert(Sh::Dot < Sh::Circle(0))?;
    assert(Sh::Circle(1) < Sh::Circle(2))?;
    assert(Sh::Rect{ w: 1, h: 2 } < Sh::Rect{ w: 1, h: 3 })?;
    assert(Sh::Rect{ w: 1, h: 2 } >= Sh::Rect{ w: 1, h: 2 })?;
    assert(Sh::Rect{ w: 0, h: 0 } > Sh::Pair(9, "z"))?;
}

test("enum Hash") {
    assert(Sh::Circle(3).hash() == Sh::Circle(3).hash())?;
    assert(Sh::Circle(3).hash() != Sh::Circle(4).hash())?;
    assert(Sh::Dot.hash() != Sh::Circle(0).hash())?;
}

test("scalar enums use their backing") {
    assert(Status::Ok.show() == "200")?;
    assert(Status::NotFound.to_string() == "404")?;
    assert(Status::Ok == Status::Ok)?;
    assert(Status::Ok < Status::NotFound)?;
    assert(Status::Ok.hash() == 200.hash())?;
    assert(Mode::Write.show() == "w")?;
    assert(Mode::Read < Mode::Write)?;
}

test("tuple variants with different payload types") {
    assert(format("%v", Holder::S("x")) == "Holder::S(x)", format("%v", Holder::S("x")))?;
    assert(Holder::B(true).show() == "Holder::B(true)")?;
    assert(Holder::S("x") == Holder::S("x"))?;
    assert(Holder::S("x") != Holder::B(false))?;
}
