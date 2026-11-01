// See macro_cycle_a.
use macro::{TypeDecl, Code, raw};
use macro_cycle_a::CycleA;

derive CycleB(TypeDecl t) -> Code {
    return raw("");
}

#[derive(CycleA)]
class UsesA {
    pub x: int,
}
