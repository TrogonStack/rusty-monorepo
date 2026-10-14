// edition:2024
// compile-flags: --test

fn private_value() -> u8 {
    5
}

#[cfg(test)]
#[path = "./lib_tests.rs"]
mod tests;
