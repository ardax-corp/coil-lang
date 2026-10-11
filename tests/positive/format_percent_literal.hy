// `%%` in a format string is a literal `%` and consumes no argument
// (coil-lang#889).
use string::format;

test("%% alone prints one percent") {
    assert(format("100%%") == "100%")?;
}

test("%% before a specifier") {
    assert(format("[%%] %i", 3) == "[%] 3")?;
    assert(format("%i%%", 50) == "50%")?;
    assert(format("%%%%") == "%%")?;
}
