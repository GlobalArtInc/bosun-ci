use std::fmt;
use std::fs::{self, DirBuilder};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use bosun_db::secrets::Sealed;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::hmac;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tracing::warn;

use crate::Error;
use crate::config::Keys;

const KEK_DIGITS: usize = 64;

const KEY_BYTES: usize = KEK_DIGITS / 2;

const ID_DIGITS: usize = 16;

const ID_LABEL: &[u8] = b"bosun kek id\0";

pub(crate) struct Labels {
    value: &'static [u8],
    data_key: &'static [u8],
}

const SECRET: Labels = Labels {
    value: b"bosun secret value\0",
    data_key: b"bosun secret data key\0",
};

pub(crate) const FORGE_TOKEN: Labels = Labels {
    value: b"bosun forge token value\0",
    data_key: b"bosun forge token data key\0",
};

const SIGNING_LABEL: &[u8] = b"bosun signing key\0";

const GENERATED: &str = "keys/kek";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Generation {
    Allowed,
    Refused,
}

pub(crate) struct Kek {
    id: String,
    key: LessSafeKey,
    signing: hmac::Key,
}

impl Kek {
    pub(crate) fn parse(text: &str, origin: &str) -> Result<Kek, Error> {
        let digits = text.trim();
        let malformed = || Error::MalformedKek {
            origin: origin.to_owned(),
        };
        if digits.len() != KEK_DIGITS || !digits.bytes().all(|digit| digit.is_ascii_hexdigit()) {
            return Err(malformed());
        }
        let bytes = digits
            .as_bytes()
            .chunks(2)
            .map(|pair| {
                std::str::from_utf8(pair)
                    .ok()
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            })
            .collect::<Option<Vec<u8>>>()
            .ok_or_else(malformed)?;
        let mut hasher = Sha256::new();
        hasher.update(ID_LABEL);
        hasher.update(&bytes);
        let id: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let derived = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, &bytes), SIGNING_LABEL);
        Ok(Kek {
            id: id[..ID_DIGITS].to_owned(),
            key: cipher(&bytes).ok_or_else(malformed)?,
            signing: hmac::Key::new(hmac::HMAC_SHA256, derived.as_ref()),
        })
    }
}

pub(crate) struct Keyring {
    current: Kek,
    previous: Vec<Kek>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Unreadable {
    UnknownKek(String),
    Refused(String),
}

impl fmt::Display for Unreadable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unreadable::UnknownKek(id) => write!(
                f,
                "its data key is encrypted under the KEK `{id}`, which the configuration of this \
                 server does not hold; name the file of that KEK in keys.previous_kek_files"
            ),
            Unreadable::Refused(id) => write!(
                f,
                "it does not decrypt under the KEK `{id}`, so its ciphertext or its data key \
                 belongs to another secret or was changed"
            ),
        }
    }
}

impl Keyring {
    pub(crate) fn new(current: Kek, previous: Vec<Kek>) -> Keyring {
        let mut kept: Vec<Kek> = Vec::new();
        for kek in previous {
            if kek.id != current.id && kept.iter().all(|known| known.id != kek.id) {
                kept.push(kek);
            }
        }
        Keyring {
            current,
            previous: kept,
        }
    }

    pub(crate) fn current(&self) -> &str {
        &self.current.id
    }

    pub(crate) fn seal(&self, binding: &[u8], value: &[u8]) -> Result<Sealed, Error> {
        self.seal_as(&SECRET, binding, value)
    }

    pub(crate) fn seal_as(
        &self,
        labels: &Labels,
        binding: &[u8],
        value: &[u8],
    ) -> Result<Sealed, Error> {
        let mut data_key = [0; KEY_BYTES];
        getrandom::fill(&mut data_key).map_err(Error::Random)?;
        let cipher = cipher(&data_key).ok_or(Error::Seal)?;
        Ok(Sealed {
            ciphertext: encrypt(&cipher, &aad(labels.value, binding), value)?,
            data_key: encrypt(&self.current.key, &aad(labels.data_key, binding), &data_key)?,
            kek_id: self.current.id.clone(),
        })
    }

    pub(crate) fn open(&self, binding: &[u8], sealed: &Sealed) -> Result<Vec<u8>, Unreadable> {
        self.open_as(&SECRET, binding, sealed)
    }

    pub(crate) fn open_as(
        &self,
        labels: &Labels,
        binding: &[u8],
        sealed: &Sealed,
    ) -> Result<Vec<u8>, Unreadable> {
        let data_key = self.data_key(labels, binding, sealed)?;
        let refused = || Unreadable::Refused(sealed.kek_id.clone());
        let cipher = cipher(&data_key).ok_or_else(refused)?;
        decrypt(&cipher, &aad(labels.value, binding), &sealed.ciphertext).ok_or_else(refused)
    }

    pub(crate) fn rewrap(&self, binding: &[u8], sealed: &Sealed) -> Result<Vec<u8>, Unreadable> {
        let data_key = self.data_key(&SECRET, binding, sealed)?;
        encrypt(&self.current.key, &aad(SECRET.data_key, binding), &data_key)
            .map_err(|_| Unreadable::Refused(sealed.kek_id.clone()))
    }

    pub(crate) fn sign(&self, label: &[u8], message: &[u8]) -> Vec<u8> {
        hmac::sign(&self.current.signing, &aad(label, message))
            .as_ref()
            .to_vec()
    }

    pub(crate) fn verify(&self, label: &[u8], message: &[u8], tag: &[u8]) -> bool {
        let signed = aad(label, message);
        std::iter::once(&self.current)
            .chain(&self.previous)
            .any(|kek| hmac::verify(&kek.signing, &signed, tag).is_ok())
    }

    fn data_key(
        &self,
        labels: &Labels,
        binding: &[u8],
        sealed: &Sealed,
    ) -> Result<Vec<u8>, Unreadable> {
        let kek = std::iter::once(&self.current)
            .chain(&self.previous)
            .find(|kek| kek.id == sealed.kek_id)
            .ok_or_else(|| Unreadable::UnknownKek(sealed.kek_id.clone()))?;
        decrypt(&kek.key, &aad(labels.data_key, binding), &sealed.data_key)
            .filter(|key| key.len() == KEY_BYTES)
            .ok_or_else(|| Unreadable::Refused(sealed.kek_id.clone()))
    }
}

fn cipher(key: &[u8]) -> Option<LessSafeKey> {
    UnboundKey::new(&AES_256_GCM, key)
        .ok()
        .map(LessSafeKey::new)
}

fn aad(label: &[u8], binding: &[u8]) -> Vec<u8> {
    let mut aad = label.to_vec();
    aad.extend_from_slice(binding);
    aad
}

fn encrypt(cipher: &LessSafeKey, aad: &[u8], plain: &[u8]) -> Result<Vec<u8>, Error> {
    let mut nonce = [0; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(Error::Random)?;
    let mut body = plain.to_vec();
    cipher
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad),
            &mut body,
        )
        .map_err(|_| Error::Seal)?;
    let mut sealed = nonce.to_vec();
    sealed.extend_from_slice(&body);
    Ok(sealed)
}

fn decrypt(cipher: &LessSafeKey, aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < NONCE_LEN {
        return None;
    }
    let (nonce, body) = sealed.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
    let mut body = body.to_vec();
    cipher
        .open_in_place(nonce, Aad::from(aad), &mut body)
        .ok()
        .map(|plain| plain.to_vec())
}

pub(crate) fn load(keys: &Keys, data_dir: &Path, generation: Generation) -> Result<Keyring, Error> {
    let current = match (&keys.kek, &keys.kek_file) {
        (Some(_), Some(_)) => return Err(Error::TwoKeks),
        (Some(kek), None) => Kek::parse(kek, "keys.kek (BOSUN_KEYS_KEK)")?,
        (None, Some(path)) => file(path)?,
        (None, None) => generated(&data_dir.join(GENERATED), generation)?,
    };
    let previous = keys
        .previous_kek_files
        .iter()
        .map(|path| file(path))
        .collect::<Result<Vec<Kek>, Error>>()?;
    Ok(Keyring::new(current, previous))
}

fn generated(path: &Path, generation: Generation) -> Result<Kek, Error> {
    match fs::read_to_string(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => match generation {
            Generation::Allowed => generate(path),
            Generation::Refused => Err(Error::NoKek {
                path: path.to_owned(),
            }),
        },
        read => checked(read, path),
    }
}

fn generate(path: &Path) -> Result<Kek, Error> {
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
    let mut bytes = [0; KEY_BYTES];
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
            Kek::parse(&digits, &path.display().to_string())
        }
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => file(path),
        Err(error) => Err(failed(error.error)),
    }
}

fn file(path: &Path) -> Result<Kek, Error> {
    checked(fs::read_to_string(path), path)
}

fn checked(read: io::Result<String>, path: &Path) -> Result<Kek, Error> {
    let kek = read.map_err(|source| Error::ReadKek {
        path: path.to_owned(),
        source,
    })?;
    Kek::parse(&kek, &path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::{FORGE_TOKEN, Kek, Keyring, Unreadable};

    const FIRST: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    const SECOND: &str = "f0e1d2c3b4a5968778695a4b3c2d1e0f00112233445566778899aabbccddeeff";

    fn kek(text: &str) -> Kek {
        Kek::parse(text, "the test").expect("a KEK")
    }

    #[test]
    fn a_kek_is_named_by_a_hash_of_itself_and_its_text_must_be_64_hexadecimal_digits() {
        let first = kek(FIRST);
        assert_eq!(first.id.len(), 16);
        assert_eq!(kek(&format!("{FIRST}\n")).id, first.id);
        assert_eq!(kek(&FIRST.to_uppercase()).id, first.id);
        assert_ne!(kek(SECOND).id, first.id);
        for malformed in [
            &FIRST[..62],
            &format!("{FIRST}00"),
            &FIRST.replace('0', "g"),
        ] {
            assert!(Kek::parse(malformed, "the test").is_err(), "{malformed}");
        }
    }

    #[test]
    fn a_signature_holds_under_its_label_and_the_keks_that_the_keyring_keeps() {
        let keys = Keyring::new(kek(FIRST), Vec::new());
        let tag = keys.sign(b"label\0", b"message");
        assert!(keys.verify(b"label\0", b"message", &tag));
        assert!(!keys.verify(b"label\0", b"massage", &tag));
        assert!(!keys.verify(b"other\0", b"message", &tag));
        assert!(!Keyring::new(kek(SECOND), Vec::new()).verify(b"label\0", b"message", &tag));
        assert!(Keyring::new(kek(SECOND), vec![kek(FIRST)]).verify(b"label\0", b"message", &tag));
    }

    #[test]
    fn a_value_opens_only_with_the_binding_it_was_sealed_with() {
        let keys = Keyring::new(kek(FIRST), Vec::new());
        let sealed = keys
            .seal(b"globalart/keel/A", b"hunter2")
            .expect("a sealed value");
        assert_eq!(sealed.kek_id, keys.current());
        assert!(
            !sealed
                .ciphertext
                .windows(7)
                .any(|window| window == b"hunter2")
        );
        assert_eq!(
            keys.open(b"globalart/keel/A", &sealed),
            Ok(b"hunter2".to_vec())
        );
        assert_eq!(
            keys.open(b"globalart/keel/B", &sealed),
            Err(Unreadable::Refused(keys.current().to_owned()))
        );
        let other = keys
            .seal(b"globalart/keel/B", b"other")
            .expect("a sealed value");
        let mut swapped = other.clone();
        swapped.ciphertext = sealed.ciphertext.clone();
        assert!(keys.open(b"globalart/keel/B", &swapped).is_err());
        let mut tampered = sealed.clone();
        let last = tampered.ciphertext.len() - 1;
        tampered.ciphertext[last] ^= 1;
        assert!(keys.open(b"globalart/keel/A", &tampered).is_err());
    }

    #[test]
    fn a_secret_and_a_forge_token_never_open_as_each_other() {
        let keys = Keyring::new(kek(FIRST), Vec::new());
        let secret = keys.seal(b"binding", b"value").expect("a sealed secret");
        let token = keys
            .seal_as(&FORGE_TOKEN, b"binding", b"ghs_value")
            .expect("a sealed token");
        assert_eq!(
            keys.open_as(&FORGE_TOKEN, b"binding", &token),
            Ok(b"ghs_value".to_vec())
        );
        assert!(keys.open_as(&FORGE_TOKEN, b"binding", &secret).is_err());
        assert!(keys.open(b"binding", &token).is_err());
    }

    #[test]
    fn a_rewrapped_data_key_opens_under_the_new_kek_alone() {
        let old = Keyring::new(kek(FIRST), Vec::new());
        let sealed = old.seal(b"binding", b"value").expect("a sealed value");
        let rotating = Keyring::new(kek(SECOND), vec![kek(FIRST), kek(SECOND)]);
        assert_eq!(rotating.open(b"binding", &sealed), Ok(b"value".to_vec()));
        let mut rewrapped = sealed.clone();
        rewrapped.data_key = rotating
            .rewrap(b"binding", &sealed)
            .expect("a new data key");
        rewrapped.kek_id = rotating.current().to_owned();
        let new = Keyring::new(kek(SECOND), Vec::new());
        assert_eq!(new.open(b"binding", &rewrapped), Ok(b"value".to_vec()));
        assert_eq!(
            new.open(b"binding", &sealed),
            Err(Unreadable::UnknownKek(old.current().to_owned()))
        );
    }
}
