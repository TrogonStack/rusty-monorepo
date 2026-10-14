fn private_value() -> u8 {
    4
}

#[cfg(test)]
#[path = "layout_split_owner/tests.rs"]
#[allow(redundant_module_path, reason = "a `#[path]`-loaded file resolves `mod tests;` beside itself")]
mod tests;
