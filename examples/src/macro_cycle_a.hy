// macro_cycle_a and macro_cycle_b each need the other's derive to compile.
use macro::{TypeDecl, Code, raw};
use macro_cycle_b::CycleB;

derive CycleA(TypeDecl t) -> Code {
    return raw("");
}

#[derive(CycleB)]
class UsesB {
    pub x: int,
}
