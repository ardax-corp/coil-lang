// Large ints cannot use inline CONST (i32); two-word Result Ok must pool them.
use io::{IoError};

fn big() -> Result<int, IoError> {
    return 94805378185680;
}

test("two-word Result Ok keeps ints above i32::MAX") {
    match big() {
        Result::Ok(n) => {
            assert(n == 94805378185680)?;
        },
        Result::Err(_) => {
            panic "big";
        },
    };
}
