// Second helper module for tests/positive/same_name_types_across_modules.hy
// (#957): another `Reader`, so two non-entry modules share the name too.
use string::format;

#[derive(Show)]
class Reader {
    pub text: string,
}

fn show_text_reader(string text) -> string {
    return format("%v", new Reader(text));
}
