use tests::assert;
use string::{find_all, split, replace_all};

test("find_all returns non-overlapping offsets") {
    let at = find_all("a,b,,c", ",");
    assert!(at.len() == 3);
    assert!(at[0] == 1 && at[1] == 3 && at[2] == 4);
    assert!(find_all("aaaa", "aa").len() == 2);
    assert!(find_all("aaa", "aa").len() == 1);
    assert!(find_all("abc", "x").len() == 0);
    let empty = find_all("aé", "");
    assert!(empty.len() == 3 && empty[1] == 1 && empty[2] == 3);
}

test("split keeps empty pieces at the ends") {
    let parts = split("a,b,,c", ",");
    assert!(parts.len() == 4);
    assert!(parts[0] == "a" && parts[1] == "b" && parts[2] == "" && parts[3] == "c");
    let ends = split(",a,", ",");
    assert!(ends.len() == 3 && ends[0] == "" && ends[1] == "a" && ends[2] == "");
    assert!(split("abc", "x").len() == 1);
    let chars = split("ab", "");
    assert!(chars.len() == 4 && chars[1] == "a" && chars[2] == "b" && chars[3] == "");
}

test("split pieces outlive a long source string") {
    let line = "";
    let i = 0;
    while i < 200 {
        line = line + "field" + string::format("%i", i) + ";";
        i = i + 1;
    }
    let parts = split(line, ";");
    assert!(parts.len() == 201);
    assert!(parts[0] == "field0" && parts[199] == "field199" && parts[200] == "");
}

test("replace_all replaces every match") {
    assert!(replace_all("a-b-c", "-", "+") == "a+b+c");
    assert!(replace_all("aaaa", "aa", "b") == "bb");
    assert!(replace_all("ab", "", "-") == "-a-b-");
    assert!(replace_all("abc", "x", "-") == "abc");
    assert!(replace_all("héllo", "é", "e") == "hello");
}
