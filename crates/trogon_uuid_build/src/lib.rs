#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

mod descriptor;
mod emit;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use prost_reflect::DescriptorPool;

/// A build-time recipe selection and the Rust marker name to generate.
#[derive(Debug, Clone)]
pub struct Template {
    enum_name: String,
    value_name: String,
    rust_name: String,
}

impl Template {
    pub fn new(enum_name: impl Into<String>, value_name: impl Into<String>, rust_name: impl Into<String>) -> Self {
        Self {
            enum_name: enum_name.into(),
            value_name: value_name.into(),
            rust_name: rust_name.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    #[error("invalid protobuf descriptor set: {0}")]
    Descriptor(#[from] prost_reflect::DescriptorError),
    #[error("invalid UUID code generation configuration: {0}")]
    Configuration(String),
    #[error("invalid UUID recipe {target}: {reason}")]
    Recipe { target: String, reason: String },
    #[error("invalid Rust syntax for {context}: {source}")]
    RustSyntax {
        context: &'static str,
        #[source]
        source: syn::Error,
    },
    #[error("invalid namespace UUID for {target}: {source}")]
    NamespaceUuid {
        target: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("cannot write generated UUID recipes to {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl Template {
    fn target(&self) -> String {
        format!("{}.{}", self.enum_name, self.value_name)
    }
    fn error(&self, reason: impl Into<String>) -> BuildError {
        BuildError::Recipe {
            target: self.target(),
            reason: reason.into(),
        }
    }
}

/// Configures the generated code's runtime crate path.
#[derive(Debug, Clone)]
pub struct Config {
    runtime_path: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            runtime_path: "::trogon_uuid".to_owned(),
        }
    }
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// Supports a renamed dependency, for example `::uuid_runtime`.
    pub fn runtime_path(mut self, path: impl Into<String>) -> Self {
        self.runtime_path = path.into();
        self
    }

    /// Generates Rust recipes from a protobuf descriptor set containing the UUID
    /// option schema and selected enums. Empty templates produce constant UUIDs;
    /// missing format options are rejected.
    ///
    /// Returns [`BuildError`] for malformed descriptors, incompatible option
    /// schemas, invalid selections or Rust names, and malformed placeholders.
    pub fn generate(&self, descriptors: &[u8], templates: &[Template]) -> Result<String, BuildError> {
        if templates.is_empty() {
            return Err(BuildError::Configuration(
                "select at least one identity recipe".to_owned(),
            ));
        }
        let runtime_path =
            syn::parse_str::<syn::Path>(&self.runtime_path).map_err(|source| BuildError::RustSyntax {
                context: "runtime crate path",
                source,
            })?;
        if runtime_path
            .segments
            .iter()
            .any(|segment| !matches!(segment.arguments, syn::PathArguments::None))
        {
            return Err(BuildError::Configuration(
                "runtime crate path cannot contain generic arguments".to_owned(),
            ));
        }
        let pool = DescriptorPool::decode(descriptors)?;
        let schema = descriptor::Schema::new(&pool)?;
        let mut names = HashSet::new();
        let mut output = proc_macro2::TokenStream::new();
        for selection in templates {
            let marker = emit::identifier(&selection.rust_name, "recipe marker name")?;
            let input = emit::identifier(&format!("{}Input", selection.rust_name), "recipe input name")?;
            for name in [&marker, &input] {
                if !names.insert(name.to_string()) {
                    return Err(BuildError::Configuration(format!(
                        "duplicate generated Rust name {name}"
                    )));
                }
            }
            let recipe = schema.resolve(&pool, selection)?;
            output.extend(emit::recipe(&recipe, &marker, &input, &runtime_path)?);
        }
        Ok(output.to_string())
    }

    /// Writes into an existing output directory, such as Cargo's `OUT_DIR`.
    pub fn write(&self, descriptors: &[u8], templates: &[Template], path: impl AsRef<Path>) -> Result<(), BuildError> {
        let source = self.generate(descriptors, templates)?;
        let path = path.as_ref();
        std::fs::write(path, source).map_err(|source| BuildError::Write {
            path: path.to_owned(),
            source,
        })
    }
}

/// Generates recipes using the conventional `::trogon_uuid` runtime path.
pub fn generate(descriptors: &[u8], templates: &[Template]) -> Result<String, BuildError> {
    Config::new().generate(descriptors, templates)
}
