// #2033: `derive(Show)` on an imported enum whose record variants share a
// field name narrows each arm to its variant instead of failing with E0119.
use tests::assert;
use string::format;
use shared_field_enum::Failure;

test("derived Show of an imported enum with a shared field name") {
    assert!(
        format("%v", Failure::Protocol { message: "bad" }) == "Failure::Protocol { message: bad }",
    );
    assert!(
        format("%v", Failure::Server { message: "down" }) == "Failure::Server { message: down }",
    );
    assert!(format("%v", Failure::Closed) == "Failure::Closed");
}

test("derived Eq of an imported enum with a shared field name") {
    assert!(Failure::Server { message: "a" } == Failure::Server { message: "a" });
    assert!(Failure::Server { message: "a" } != Failure::Protocol { message: "a" });
}
