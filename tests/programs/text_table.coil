// A padded text table: column widths folded over `Vec<Vec<string>>`, rows joined with `+` and escaped newlines.
// @output |name |qty|\\n|apple|3  |\\n|kiwi |12 |\\ntotal=15\n
use io::sync::println;
use string::format;

fn pad(string s, int w) -> string {
    let out = s;
    while out.len() < w {
        out += " ";
    }
    return out;
}

fn row(Vec<string> cells, Vec<int> widths) -> string {
    let s = "|";
    let i = 0;
    while i < cells.len() {
        s += pad(cells[i], widths[i]) + "|";
        i += 1;
    }
    return s;
}

fn max_widths(Vec<Vec<string>> rows) -> Vec<int> {
    let w: Vec<int> = Vec::new();
    for c in rows[0] {
        w.push(0);
    }
    for r in rows {
        let i = 0;
        while i < r.len() {
            if r[i].len() > w[i] {
                w[i] = r[i].len();
            }
            i += 1;
        }
    }
    return w;
}

fn main() {
    let rows: Vec<Vec<string>> = Vec::new();
    rows.push(Vec::from(["name", "qty"]));
    rows.push(Vec::from(["apple", "3"]));
    rows.push(Vec::from(["kiwi", "12"]));
    let w = max_widths(rows);
    let out = "";
    for r in rows {
        out += row(r, w) + "\\n";
    }
    let _ = println(format("%stotal=%i", out, 3 + 12));
}
