// COI-114: tuple variant may wrap a class (including mutual recursion).
class JsonObject {
    pub keys: Vec<string>,
    pub vals: Vec<JsonValue>,
}

enum JsonValue {
    Null,
    Obj(JsonObject),
}

test("construct and match Obj(JsonObject)") {
    let o = new JsonObject(Vec::from(["a"]), Vec::from([JsonValue::Null]));
    let v = JsonValue::Obj(o);
    let n = match v {
        JsonValue::Obj(inner) => inner.keys.len(),
        JsonValue::Null => -1,
    };
    assert(n == 1)?;
}
