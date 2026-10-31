// `?` on `Option` through fields, `?.` on an `Option` field, `??` defaults, generic first / last and flattening options.
// @output 30 none 7 [1, 3] 2 first=Some(1) last=Some(3) none\n
use io::sync::println;
use string::format;

class User {
    pub name: string,
    pub age: Option<int>,
    pub boss: Option<User>,
}

fn find_user(Vec<User> us, string name) -> Option<User> {
    for u in us {
        if u.name == name {
            return Option::Some(u);
        }
    }
    return Option::None;
}

fn boss_age(Vec<User> us, string name) -> Option<int> {
    let u = find_user(us, name)?;
    let b = u.boss?;
    return b.age;
}

fn first<T>(Vec<T> v) -> Option<T> {
    if v.len() == 0 {
        return Option::None;
    }
    return Option::Some(v[0]);
}

fn last<T>(Vec<T> v) -> Option<T> {
    if v.len() == 0 {
        return Option::None;
    }
    return Option::Some(v[v.len() - 1]);
}

fn flatten(Vec<Option<int>> v) -> Vec<int> {
    let out: Vec<int> = Vec::new();
    for o in v {
        if let Option::Some(x) = o {
            out.push(x);
        }
    }
    return out;
}

fn main() {
    let boss = new User("ann", Option::Some(30), Option::None);
    let bob = new User("bob", Option::None, Option::Some(boss));
    let users = Vec::from([boss, bob]);
    let ba = boss_age(users, "bob") ?? -1;
    let none = match boss_age(users, "ann") {
        Option::Some(a) => format("%i", a),
        Option::None => "none",
    };
    let age_or = bob.age ?? 7;
    let flat = flatten(Vec::from([Option::Some(1), Option::None, Option::Some(3)]));
    let via = find_user(users, "bob")?.boss;
    let nested = match via {
        Option::Some(Option::Some(b)) => b.age ?? 0,
        default => 0,
    };
    let empty: Vec<int> = Vec::new();
    let e = match first(empty) {
        Option::Some(_) => "some",
        Option::None => "none",
    };
    let _ = println(
        format(
            "%i %s %i %v %i first=%v last=%v %s",
            ba,
            none,
            age_or,
            flat,
            flat.len(),
            first(flat),
            last(flat),
            e,
        ),
    );
    let _ = nested;
}
