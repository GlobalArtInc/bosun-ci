use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const PAGE: &str = "index.html";

fn main() -> Result<(), Box<dyn Error>> {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed={}", dist.display());
    let interface = dist.join("interface");
    let mut files = Vec::new();
    if interface.join(PAGE).is_file() {
        collect(&fs::canonicalize(&interface)?, "", &mut files)?;
    }
    files.sort();
    let mut source = String::from("pub(crate) static FILES: &[(&str, &[u8])] = &[\n");
    for (name, path) in &files {
        let path = path
            .to_str()
            .ok_or("a file of the interface has a path that is not UTF-8")?;
        writeln!(source, "    ({name:?}, include_bytes!({path:?})),")?;
    }
    source.push_str("];\n");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("cargo set no OUT_DIR")?);
    fs::write(out.join("interface.rs"), source)?;
    Ok(())
}

fn collect(
    directory: &Path,
    prefix: &str,
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "a file of the interface has a name that is not UTF-8")?;
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let relative = format!("{prefix}{name}");
        if entry.file_type()?.is_dir() {
            collect(&path, &format!("{relative}/"), files)?;
        } else {
            files.push((relative, path));
        }
    }
    Ok(())
}
