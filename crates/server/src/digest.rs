use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, ReadBuf};

pub(crate) struct Digesting<R> {
    inner: R,
    hasher: Sha256,
    size: u64,
    expected: Option<Vec<u8>>,
    digest: Option<Vec<u8>>,
    failure: Option<String>,
}

impl<R> Digesting<R> {
    pub(crate) fn new(inner: R) -> Digesting<R> {
        Digesting {
            inner,
            hasher: Sha256::new(),
            size: 0,
            expected: None,
            digest: None,
            failure: None,
        }
    }

    pub(crate) fn expecting(inner: R, expected: Vec<u8>) -> Digesting<R> {
        Digesting {
            expected: Some(expected),
            ..Digesting::new(inner)
        }
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }

    pub(crate) fn digest(&self) -> Option<&[u8]> {
        self.digest.as_deref()
    }

    pub(crate) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    fn end(&mut self) -> io::Result<()> {
        if self.digest.is_some() {
            return Ok(());
        }
        let digest = std::mem::take(&mut self.hasher).finalize().to_vec();
        let mismatch = self
            .expected
            .as_ref()
            .filter(|expected| **expected != digest)
            .map(|expected| {
                format!(
                    "its SHA-256 is {}, and the upstream lists {}",
                    hex(&digest),
                    hex(expected)
                )
            });
        self.digest = Some(digest);
        match mismatch {
            Some(message) => {
                self.failure = Some(message.clone());
                Err(io::Error::other(message))
            }
            None => Ok(()),
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for Digesting<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if buf.remaining() == 0 {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        }
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let read = &buf.filled()[before..];
                if read.is_empty() {
                    return Poll::Ready(this.end());
                }
                this.hasher.update(read);
                this.size += read.len() as u64;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                this.failure = Some(error.to_string());
                Poll::Ready(Err(error))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn unhex(digits: &str) -> Option<Vec<u8>> {
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    digits
        .as_bytes()
        .chunks(2)
        .map(|pair| {
            let high = char::from(pair[0]).to_digit(16)?;
            let low = char::from(pair[1]).to_digit(16)?;
            u8::try_from(high * 16 + low).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;

    use super::{Digesting, hex, unhex};

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[tokio::test]
    async fn a_read_to_its_end_has_the_size_and_digest_of_what_passed() {
        let mut digesting = Digesting::new(&b"hello"[..]);
        let mut read = Vec::new();
        digesting
            .read_to_end(&mut read)
            .await
            .expect("a digest of five bytes");
        assert_eq!(read, b"hello");
        assert_eq!(digesting.size(), 5);
        assert_eq!(
            digesting.digest().map(hex).as_deref(),
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
        assert_eq!(digesting.failure(), None);
    }

    #[tokio::test]
    async fn an_end_that_does_not_match_the_expected_digest_is_an_error() {
        let expected = unhex(EMPTY).expect("a digest");
        let mut matching = Digesting::expecting(&b""[..], expected.clone());
        let mut read = Vec::new();
        assert!(matching.read_to_end(&mut read).await.is_ok());
        let mut differing = Digesting::expecting(&b"x"[..], expected);
        let refused = differing
            .read_to_end(&mut read)
            .await
            .expect_err("a mismatch");
        assert!(
            refused
                .to_string()
                .ends_with(&format!("the upstream lists {EMPTY}")),
            "{refused}"
        );
        assert_eq!(differing.failure(), Some(refused.to_string().as_str()));
    }

    #[test]
    fn hexadecimal_digits_read_back_as_the_bytes_they_spell() {
        assert_eq!(unhex("00ff7A"), Some(vec![0x00, 0xff, 0x7a]));
        assert_eq!(hex(&[0x00, 0xff, 0x7a]), "00ff7a");
        for refused in ["0", "0g", "zz", "0 1f"] {
            assert_eq!(unhex(refused), None, "{refused}");
        }
    }
}
