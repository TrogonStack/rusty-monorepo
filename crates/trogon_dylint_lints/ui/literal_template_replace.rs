// edition:2024
#![allow(unused, constant_outside_constants_module)]

const TEMPLATE: &str = "Hello, $USER!";
static BANNER: &str = "== {title} ==";

struct Holder {
    text: String,
}

struct Custom;

impl Custom {
    fn replace(&self, _from: &str, _to: &str) -> String {
        String::new()
    }
}

fn main() {
    let name = "world";

    // A literal receiver must fire.
    let _ = "Hello, {name}!".replace("{name}", name);

    // `replacen` must fire.
    let _ = "{a} and {a}".replacen("{a}", name, 1);

    // A `const` receiver must fire.
    let _ = TEMPLATE.replace("$USER", name);

    // A `static` receiver must fire.
    let _ = BANNER.replace("{title}", name);

    // A runtime value is data, not a template, and must NOT fire.
    let variable = String::from("a-b");
    let _ = variable.replace('-', "_");
    let owned = String::from("a-b").replace('-', "_");
    let slice: &str = "a-b";
    let _ = slice.replace('-', "_");
    let holder = Holder { text: String::new() };
    let _ = holder.text.replace('-', "_");

    // A user type with its own `replace` must NOT fire.
    let _ = Custom.replace("a", "b");

    // `#[allow]` at the site must suppress the lint.
    #[allow(literal_template_replace)]
    let _ = "Hello, {name}!".replace("{name}", name);
}
