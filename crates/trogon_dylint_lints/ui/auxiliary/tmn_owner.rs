fn value() -> u8 {
    7
}

#[cfg(test)]
#[path = "tmn_owner_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "layout_directory/../tmn_owner_parse_tests.rs"]
mod parse_tests;
