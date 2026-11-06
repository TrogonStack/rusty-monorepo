#![forbid(unsafe_code)]

#[derive(Debug, thiserror::Error)]
#[error("UUID recipe generation is not implemented")]
pub struct BuildError;

#[derive(Debug, Clone)]
pub struct Template {
    enum_name: String,
    value_name: String,
    rust_name: String,
}

impl Template {
    pub fn new(enum_name: impl Into<String>, value_name: impl Into<String>, rust_name: impl Into<String>) -> Self {
        Self { enum_name: enum_name.into(), value_name: value_name.into(), rust_name: rust_name.into() }
    }
}

pub fn generate(_descriptors: &[u8], templates: &[Template]) -> Result<String, BuildError> {
    let _selection = templates.iter().map(|t| (&t.enum_name, &t.value_name, &t.rust_name));
    Ok(String::new())
}

#[derive(Debug, Clone)]
pub struct Config {
    runtime_path: String,
}

impl Default for Config {
    fn default() -> Self { Self { runtime_path: "::trogon_uuid".to_owned() } }
}

impl Config {
    pub fn new() -> Self { Self::default() }
    pub fn runtime_path(mut self, path: impl Into<String>) -> Self {
        self.runtime_path = path.into();
        self
    }
    pub fn generate(&self, descriptors: &[u8], templates: &[Template]) -> Result<String, BuildError> {
        let _runtime_path = &self.runtime_path;
        generate(descriptors, templates)
    }
}
