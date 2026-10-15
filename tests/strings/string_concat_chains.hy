// Chained `+` on string variables builds the whole string: in a test case,
// across literals with escapes, and grown with `acc = acc + …` in a loop.
use tests::assert;

test("variable plus variable in a test case") {
    let a = "GET";
    let b = "/hi";
    assert!(a + b == "GET/hi");
}

test("a nested concat chain keeps every piece") {
    let method = "GET";
    let path = "/hi";
    let host = "example.com";
    let line = method + " " + path + " HTTP/1.1\r\nHost: " + host + "\r\n";
    assert!(line == "GET /hi HTTP/1.1\r\nHost: example.com\r\n", line);
    let a = "a";
    let b = "b";
    let c = "c";
    assert!(a + b + c == "abc");
}

test("a header block grown in a loop") {
    let names = ["X-Trace", "Accept"];
    let values = ["abc", "text/plain"];
    let acc = names[0] + ": " + values[0] + "\r\n";
    let i = 1;
    while i < 2 {
        acc = acc + names[i] + ": " + values[i] + "\r\n";
        i = i + 1;
    }
    assert!(acc == "X-Trace: abc\r\nAccept: text/plain\r\n", acc);
}
