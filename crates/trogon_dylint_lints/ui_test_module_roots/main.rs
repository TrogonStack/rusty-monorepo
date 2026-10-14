// edition:2024
// compile-flags: --test

fn private_value() -> u8 {
    8
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
