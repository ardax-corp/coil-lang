// `?` on a `Result` whose error type differs from the function's converts
// the error through `impl Into<FnErr> for Err` when that impl exists.
use tests::assert;

enum ParseError {
    Empty,
    BadDigit(int),
}

enum ConfigError {
    Parse(ParseError),
    Missing(string),
}

impl Into<ConfigError> for ParseError {
    pub fn into(ParseError e) -> ConfigError {
        return ConfigError::Parse(e);
    }
}

fn parse_digit(string s) -> Result<int, ParseError> {
    if s == "" {
        raise ParseError::Empty;
    }
    if s == "x" {
        raise ParseError::BadDigit(0);
    }
    return Result::Ok(7);
}

fn load(string s) -> Result<int, ConfigError> {
    let d = parse_digit(s)?;
    return Result::Ok(d + 1);
}

fn load_inline(string s) -> Result<int, ConfigError> {
    return Result::Ok(parse_digit(s)? * 2);
}

fn require(string key) -> Result<int, ConfigError> {
    if key == "" {
        raise ConfigError::Missing("key");
    }
    return load(key);
}

// The same error type still passes through unchanged.
fn reparse(string s) -> Result<int, ParseError> {
    return Result::Ok(parse_digit(s)? + 0);
}

fn describe(Result<int, ConfigError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(ConfigError::Parse(ParseError::Empty)) => "parse-empty",
        Result::Err(ConfigError::Parse(ParseError::BadDigit(_))) => "parse-digit",
        Result::Err(ConfigError::Missing(k)) => "missing " + k,
    };
}

test("? converts the error through Into") {
    assert!(load("5") == Result::Ok(8));
    assert!(describe(load("")) == "parse-empty");
    assert!(describe(load("x")) == "parse-digit");
}

test("? converts inside a larger expression") {
    assert!(load_inline("5") == Result::Ok(14));
    assert!(describe(load_inline("")) == "parse-empty");
}

test("a converted error forwards again with ?") {
    assert!(describe(require("")) == "missing key");
    assert!(describe(require("x")) == "parse-digit");
}

test("? with the same error type does not convert") {
    let empty = match reparse("") {
        Result::Err(ParseError::Empty) => true,
        default => false,
    };
    assert!(empty);
}
