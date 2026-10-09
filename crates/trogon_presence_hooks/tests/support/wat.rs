use std::path::{Path, PathBuf};

pub const OUTPUT: &str = r#"{"wat":true}"#;

pub fn component(imports: &[&str]) -> String {
    let imports: String = imports
        .iter()
        .map(|name| format!("  (import \"{name}\" (instance))\n"))
        .collect();
    format!(
        r#"(component
{imports}  (import "trogon:presence/types@0.1.0" (instance $types
    (type $op (enum "track" "update" "retrack"))
    (export "op" (type (eq $op)))
    (type $hook-error (variant (case "reject" string) (case "error" string)))
    (export "hook-error" (type (eq $hook-error)))))
  (alias export $types "op" (type $op))
  (alias export $types "hook-error" (type $hook-error))
  (core module $m
    (memory (export "memory") 1)
    (global $next (mut i32) (i32.const 1024))
    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      (local $ptr i32)
      (local.set $ptr (i32.and (i32.add (global.get $next) (i32.const 7)) (i32.const -8)))
      (global.set $next (i32.add (local.get $ptr) (local.get 3)))
      (local.get $ptr))
    (data (i32.const 16) "{output}")
    (data (i32.const 64) "\00\00\00\00\10\00\00\00\{len:02x}\00\00\00")
    (func (export "enrich") (param i32 i32 i32 i32 i32 i32 i32) (result i32)
      (i32.const 64)))
  (core instance $i (instantiate $m))
  (func (export "enrich")
    (param "op" $op) (param "topic" string) (param "key" string) (param "meta" (list u8))
    (result (result (list u8) (error $hook-error)))
    (canon lift (core func $i "enrich") (memory (core memory $i "memory")) (realloc (core func $i "realloc")))))
"#,
        output = OUTPUT.replace('"', "\\\""),
        len = OUTPUT.len(),
    )
}

pub fn write(dir: &Path, name: &str, imports: &[&str]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{name}.wat"));
    std::fs::write(&path, component(imports))?;
    Ok(path)
}
