use std::fs;

use crate::Error;
use crate::config::Keys;

const KEK_DIGITS: usize = 64;

pub(crate) fn verify(keys: &Keys) -> Result<(), Error> {
    match (&keys.kek, &keys.kek_file) {
        (None, None) => Err(Error::NoKek),
        (Some(_), Some(_)) => Err(Error::TwoKeks),
        (Some(kek), None) => well_formed(kek, "keys.kek (BOSUN_KEYS_KEK)"),
        (None, Some(path)) => {
            let kek = fs::read_to_string(path).map_err(|source| Error::ReadKek {
                path: path.clone(),
                source,
            })?;
            well_formed(&kek, &path.display().to_string())
        }
    }
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
