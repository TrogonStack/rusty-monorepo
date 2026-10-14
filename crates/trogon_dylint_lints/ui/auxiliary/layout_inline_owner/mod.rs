mod inner {
    #[cfg(test)]
    mod tests {
        #[test]
        fn parses() {}
    }
}

#[allow(
    inline_module_block,
    reason = "the test module's home follows the owner's expected file, not the file the owner is written in"
)]
mod placed {
    #[cfg(test)]
    mod tests;
}
