use std::num::NonZero;
use std::thread;

use argon2::password_hash::{Error as HashError, PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use tokio::sync::{OnceCell, Semaphore};
use tokio::task;

use crate::token::base62;

pub(crate) const RULE: &str = "a password is 12 to 1024 characters";

const SHORTEST: usize = 12;

const LONGEST: usize = 1024;

const GENERATED_BYTES: usize = 32;

const GENERATED_DIGITS: usize = 43;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct Failure(String);

pub(crate) struct Hasher {
    permits: Semaphore,
    decoy: OnceCell<String>,
}

impl Hasher {
    pub(crate) fn new() -> Hasher {
        Hasher {
            permits: Semaphore::new(thread::available_parallelism().map_or(1, NonZero::get)),
            decoy: OnceCell::new(),
        }
    }

    pub(crate) async fn hash(&self, password: String) -> Result<String, Failure> {
        self.run(move || {
            argon2()
                .hash_password(password.as_bytes())
                .map(|hash| hash.to_string())
                .map_err(|error| Failure(format!("argon2id could not hash a password: {error}")))
        })
        .await
    }

    pub(crate) async fn verify(
        &self,
        password: String,
        stored: Option<String>,
    ) -> Result<bool, Failure> {
        let (stored, known) = match stored {
            Some(stored) => (stored, true),
            None => (self.decoy().await?, false),
        };
        let matches = self
            .run(
                move || match argon2().verify_password(password.as_bytes(), stored.as_str()) {
                    Ok(()) => Ok(true),
                    Err(HashError::PasswordInvalid) => Ok(false),
                    Err(error) => Err(Failure(format!(
                        "a stored password hash cannot be checked: {error}"
                    ))),
                },
            )
            .await?;
        Ok(matches && known)
    }

    async fn decoy(&self) -> Result<String, Failure> {
        self.decoy
            .get_or_try_init(|| async { self.hash(generate()?).await })
            .await
            .cloned()
    }

    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
    ) -> Result<T, Failure> {
        let _permit =
            self.permits.acquire().await.map_err(|error| {
                Failure(format!("the password hashing permits closed: {error}"))
            })?;
        task::spawn_blocking(work)
            .await
            .map_err(|error| Failure(format!("the password hashing task failed: {error}")))?
    }
}

pub(crate) fn acceptable(password: &str) -> bool {
    (SHORTEST..=LONGEST).contains(&password.chars().count())
}

pub(crate) fn generate() -> Result<String, Failure> {
    let mut secret = [0; GENERATED_BYTES];
    getrandom::fill(&mut secret).map_err(|error| {
        Failure(format!(
            "the operating system gave no random bytes: {error}"
        ))
    })?;
    Ok(base62(&secret, GENERATED_DIGITS))
}

fn argon2() -> Argon2<'static> {
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::default())
}

#[cfg(test)]
mod tests {
    use super::{Hasher, acceptable, generate};

    #[tokio::test]
    async fn a_hash_is_argon2id_and_verifies_only_its_own_password() {
        let hasher = Hasher::new();
        let hash = hasher
            .hash("correct horse battery staple".to_owned())
            .await
            .expect("a hash");
        assert!(
            hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{hash}"
        );
        assert!(
            hasher
                .verify(
                    "correct horse battery staple".to_owned(),
                    Some(hash.clone())
                )
                .await
                .expect("a verification")
        );
        assert!(
            !hasher
                .verify(
                    "correct horse battery stapler".to_owned(),
                    Some(hash.clone())
                )
                .await
                .expect("a verification")
        );
        let again = hasher
            .hash("correct horse battery staple".to_owned())
            .await
            .expect("a hash");
        assert_ne!(again, hash, "two hashes of one password share a salt");
    }

    #[tokio::test]
    async fn a_user_without_a_password_never_verifies() {
        let hasher = Hasher::new();
        for password in ["", "correct horse battery staple"] {
            assert!(
                !hasher
                    .verify(password.to_owned(), None)
                    .await
                    .expect("a verification")
            );
        }
    }

    #[test]
    fn a_password_is_12_to_1024_characters_and_a_generated_one_is_acceptable() {
        assert!(!acceptable("eleven char"));
        assert!(acceptable("twelve chars"));
        assert!(acceptable(&"ä".repeat(1024)));
        assert!(!acceptable(&"a".repeat(1025)));
        let generated = generate().expect("random bytes");
        assert_eq!(generated.len(), 43);
        assert!(acceptable(&generated));
        assert_ne!(generate().expect("random bytes"), generated);
    }
}
