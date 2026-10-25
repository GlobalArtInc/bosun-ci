use std::fs::{self, DirBuilder};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use tempfile::NamedTempFile;
use tracing::warn;

use crate::Error;
use crate::config::Keys;

const KEK_DIGITS: usize = 64;

const KEK_BYTES: usize = KEK_DIGITS / 2;

const GENERATED: &str = "keys/kek";

pub(crate) fn load(keys: &Keys, data_dir: &Path) -> Result<(), Error> {
    match (&keys.kek, &keys.kek_file) {
        (Some(_), Some(_)) => Err(Error::TwoKeks),
        (Some(kek), None) => well_formed(kek, "keys.kek (BOSUN_KEYS_KEK)"),
        (None, Some(path)) => file(path),
        (None, None) => generated(&data_dir.join(GENERATED)),
    }
}

fn generated(path: &Path) -> Result<(), Error> {
    match fs::read_to_string(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => generate(path),
        read => checked(read, path),
    }
}

fn generate(path: &Path) -> Result<(), Error> {
    let failed = |source| Error::GenerateKek {
        path: path.to_owned(),
        source,
    };
    let directory = path.parent().unwrap_or(path);
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(failed)?;
    let mut bytes = [0; KEK_BYTES];
    getrandom::fill(&mut bytes).map_err(Error::Random)?;
    let digits: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut kek = NamedTempFile::new_in(directory).map_err(failed)?;
    writeln!(kek, "{digits}")
        .and_then(|()| kek.as_file().sync_all())
        .map_err(failed)?;
    match kek.persist_noclobber(path) {
        Ok(_) => {
            warn!(
                path = %path.display(),
                "generated the KEK, under which the server keeps every secret; back the file up \
                 outside this host, because every secret is lost without it"
            );
            Ok(())
        }
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => file(path),
        Err(error) => Err(failed(error.error)),
    }
}

fn file(path: &Path) -> Result<(), Error> {
    checked(fs::read_to_string(path), path)
}

fn checked(read: io::Result<String>, path: &Path) -> Result<(), Error> {
    let kek = read.map_err(|source| Error::ReadKek {
        path: path.to_owned(),
        source,
    })?;
    well_formed(&kek, &path.display().to_string())
}

fn well_formed(kek: &str, origin: &str) -> Result<(), Error> {
    let digits = kek.trim();
    if digits.len() == KEK_DIGITS && digits.bytes().all(|digit| digit.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(Error::MalformedKek {
            origin: origin.to_owned(),
        })
    }
}
