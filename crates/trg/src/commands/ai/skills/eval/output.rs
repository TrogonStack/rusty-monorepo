use std::path::Path;

pub fn print_report_dir(dir: &Path) {
    println!("{}", dir.display());
}
