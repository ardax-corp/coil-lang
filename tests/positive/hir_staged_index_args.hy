// A call-indexed read (`v[f(i)]`) inside call arguments or a string `+`
// lowers from HIR: the arguments and both concat operands stage through
// temps at depth zero, as the AST does.
use string::format;

fn at(int i) -> int {
    return i;
}

fn wrap(string s) -> string {
    return "[" + s + "]";
}

fn show(Vec<int> st) -> string {
    return "> " + wrap(format("%i/%i", st[at(1)], st[at(2)]));
}

fn pair(Vec<int> st) -> string {
    return format("%i,%i", st[0], st[at(2)]);
}

fn joined(Vec<string> names, int i) -> string {
    return names[at(i)] + "-" + names[at(i + 1)];
}

fn sum(int a, int b) -> int {
    return a + b;
}

test("call-indexed reads in staged args") {
    let st = Vec::new();
    st.push(5);
    st.push(7);
    st.push(9);
    assert(show(st) == "> [7/9]")?;
    assert(pair(st) == "5,9")?;
    assert(sum(st[at(0)], st[at(1)]) == 12)?;
}

test("call-indexed reads in a string concat") {
    let names = Vec::new();
    names.push("a");
    names.push("b");
    names.push("c");
    assert(joined(names, 1) == "b-c")?;
}
