// COI-16: inlined Vec::push must not leave the receiver under format / match /
// `new Class` args (those STORE/Seek on the shared operand/local buffer).
use string::{format};

enum LockPackage {
    Git(string, string, string, string, string),
}

class LockRecord {
    pub name: string,
    pub git: string,
    pub tag: string,
    pub rev: string,
    pub hash: string,
}

fn quote(string s) -> string {
    return "'" + s + "'";
}

fn serialize_enum(Vec<LockPackage> packages) -> string {
    let lines: Vec<string> = Vec::new();
    lines.push("# hdr");
    let i = 0;
    while i < len(packages) {
        let p = packages[i];
        i = i + 1;
        match p {
            LockPackage::Git(name, git, tag, rev, hash) => {
                lines.push(format("name = %s", quote(name)));
                lines.push(format("git = %s", quote(git)));
                lines.push(format("tag = %s", quote(tag)));
                lines.push(format("rev = %s", quote(rev)));
                lines.push(format("hash = %s", quote(hash)));
            },
        };
    }
    let out = "";
    let j = 0;
    while j < len(lines) {
        out = out + lines[j] + "\n";
        j = j + 1;
    }
    return out;
}

fn serialize_class(Vec<LockRecord> packages) -> string {
    let lines: Vec<string> = Vec::new();
    let i = 0;
    while i < len(packages) {
        let p = packages[i];
        i = i + 1;
        lines.push(format("%s\t%s\t%s\t%s\t%s", p.name, p.git, p.tag, p.rev, p.hash));
    }
    let out = "";
    let j = 0;
    while j < len(lines) {
        out = out + lines[j] + "\n";
        j = j + 1;
    }
    return out;
}

test("push format keeps both strings") {
    let lines = Vec::new();
    lines.push(format("name = %s", "alpha"));
    lines.push(format("git = %s", "a.git"));
    assert(len(lines) == 2)?;
}

test("push match binding keeps both strings") {
    let lines = Vec::new();
    let a = LockPackage::Git("alpha", "a.git", "v1", "r1", "h1");
    let b = LockPackage::Git("zeta", "z.git", "v2", "r2", "h2");
    lines.push(match a {
        LockPackage::Git(name, git, tag, rev, hash) => name,
    });
    lines.push(match b {
        LockPackage::Git(name, git, tag, rev, hash) => name,
    });
    assert(len(lines) == 2)?;
}

test("push new Class keeps both instances") {
    let pkgs = Vec::new();
    pkgs.push(new LockRecord("alpha", "a.git", "v1", "r1", "h1"));
    pkgs.push(new LockRecord("zeta", "z.git", "v2", "r2", "h2"));
    assert(len(pkgs) == 2)?;
    assert(pkgs[0].name != pkgs[1].name)?;
}

test("enum match plus format serialize round-trip") {
    let pkgs = Vec::new();
    pkgs.push(LockPackage::Git("alpha", "a.git", "v1", "r1", "h1"));
    pkgs.push(LockPackage::Git("zeta", "z.git", "v2", "r2", "h2"));
    let text = serialize_enum(pkgs);
    // hdr + 5 fields × 2 packages, each terminated by '\n'
    assert(len(text) == len("# hdr\nname = 'alpha'\ngit = 'a.git'\ntag = 'v1'\nrev = 'r1'\nhash = 'h1'\nname = 'zeta'\ngit = 'z.git'\ntag = 'v2'\nrev = 'r2'\nhash = 'h2'\n"))?;
}

test("class Vec plus format serialize keeps rows") {
    let pkgs = Vec::new();
    pkgs.push(new LockRecord("alpha", "a.git", "v1", "r1", "h1"));
    pkgs.push(new LockRecord("zeta", "z.git", "v2", "r2", "h2"));
    let text = serialize_class(pkgs);
    assert(len(text) == len("alpha\ta.git\tv1\tr1\th1\nzeta\tz.git\tv2\tr2\th2\n"))?;
}
