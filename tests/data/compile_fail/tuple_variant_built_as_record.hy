// Expected: E0205 — `JsonValue::Obj` wraps a class as a tuple payload; build it
// with `JsonValue::Obj(new JsonObject(1))`, not with record syntax.
class JsonObject {
    pub x: int,
}

enum JsonValue {
    Obj(JsonObject),
}

fn main() {
    let _ = JsonValue::Obj { x: 1 };
}
