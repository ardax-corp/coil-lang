// Expected: compile failure — a module cannot use a derive it declares.
use macro::{TypeDecl, Code, raw};

derive Local(TypeDecl t) -> Code {
    return raw("");
}

#[derive(Local)]
class C {
    pub x: int,
}
