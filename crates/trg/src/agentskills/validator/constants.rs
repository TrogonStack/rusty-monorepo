pub const MAX_NAME_LEN: usize = 64;

pub const MAX_DESC_LEN: usize = 1024;

pub const MAX_COMPAT_LEN: usize = 500;

pub const ALLOWED_FRONTMATTER_FIELDS: &[&str] = &[
    "name",
    "description",
    "license",
    "allowed-tools",
    "metadata",
    "compatibility",
];
