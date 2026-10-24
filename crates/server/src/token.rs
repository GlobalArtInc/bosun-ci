use crc::{CRC_32_ISO_HDLC, Crc};
use sha2::{Digest, Sha256};

use crate::Error;

pub(crate) const PERSONAL: &str = "bst_";

const SECRET_BYTES: usize = 32;

const SECRET_DIGITS: usize = 43;

const CHECKSUM_DIGITS: usize = 6;

const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

pub(crate) struct Token(String);

impl Token {
    pub(crate) fn generate(prefix: &'static str) -> Result<Token, Error> {
        let mut secret = [0; SECRET_BYTES];
        getrandom::fill(&mut secret).map_err(Error::Random)?;
        let mut text = String::with_capacity(prefix.len() + SECRET_DIGITS + CHECKSUM_DIGITS);
        text.push_str(prefix);
        text.push_str(&base62(&secret, SECRET_DIGITS));
        let checksum = checksum(&text);
        text.push_str(&checksum);
        Ok(Token(text))
    }

    pub(crate) fn parse(prefix: &str, text: &str) -> Option<Token> {
        let digits = text.strip_prefix(prefix)?;
        if digits.len() != SECRET_DIGITS + CHECKSUM_DIGITS
            || !digits.bytes().all(|digit| digit.is_ascii_alphanumeric())
        {
            return None;
        }
        let (signed, given) = text.split_at(text.len() - CHECKSUM_DIGITS);
        (checksum(signed) == given).then(|| Token(text.to_owned()))
    }

    pub(crate) fn sha256(&self) -> Vec<u8> {
        Sha256::digest(self.0.as_bytes()).to_vec()
    }

    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

fn checksum(signed: &str) -> String {
    base62(
        &CRC32.checksum(signed.as_bytes()).to_be_bytes(),
        CHECKSUM_DIGITS,
    )
}

fn base62(bytes: &[u8], width: usize) -> String {
    let mut number = bytes.to_vec();
    let mut digits = vec![BASE62[0]; width];
    for digit in digits.iter_mut().rev() {
        let mut remainder = 0;
        for byte in &mut number {
            let value = (remainder << 8) | usize::from(*byte);
            *byte = u8::try_from(value / BASE62.len()).unwrap_or(u8::MAX);
            remainder = value % BASE62.len();
        }
        *digit = BASE62[remainder];
    }
    digits.into_iter().map(char::from).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{PERSONAL, Token, base62, checksum};

    #[test]
    fn base62_writes_big_endian_numbers_in_a_fixed_width() {
        assert_eq!(base62(&[0; 32], 43), "0".repeat(43));
        assert_eq!(base62(&[0, 0, 0, 61], 6), "00000z");
        assert_eq!(base62(&[0, 0, 0, 62], 6), "000010");
        assert_eq!(base62(&u32::MAX.to_be_bytes(), 6), "4gfFC3");
        assert_eq!(
            base62(&[0xff; 32], 43),
            "yhjskwdA6OZ1AL1YmHWZWm8LLG7HjnuCA2j5rOw8Xp1"
        );
    }

    #[test]
    fn a_generated_token_has_the_prefix_43_secret_digits_and_6_checksum_digits() {
        let token = Token::generate(PERSONAL)
            .expect("random bytes")
            .into_string();
        assert_eq!(token.len(), 4 + 43 + 6, "{token}");
        assert!(token.starts_with("bst_"), "{token}");
        assert!(
            token[4..]
                .bytes()
                .all(|digit| digit.is_ascii_alphanumeric())
        );
        assert_eq!(&token[47..], checksum(&token[..47]));
        assert!(Token::parse(PERSONAL, &token).is_some());
    }

    #[test]
    fn generated_tokens_differ() {
        let tokens: HashSet<String> = (0..64)
            .map(|_| {
                Token::generate(PERSONAL)
                    .expect("random bytes")
                    .into_string()
            })
            .collect();
        assert_eq!(tokens.len(), 64);
    }

    #[test]
    fn a_token_whose_checksum_does_not_hold_is_refused() {
        let token = Token::generate(PERSONAL)
            .expect("random bytes")
            .into_string();
        for position in 4..token.len() {
            let mut changed = token.clone().into_bytes();
            changed[position] = if changed[position] == b'a' {
                b'b'
            } else {
                b'a'
            };
            let changed = String::from_utf8(changed).expect("ASCII");
            assert!(
                Token::parse(PERSONAL, &changed).is_none(),
                "{changed} passed the checksum"
            );
        }
    }

    #[test]
    fn a_token_of_another_kind_or_shape_is_refused() {
        let token = Token::generate(PERSONAL)
            .expect("random bytes")
            .into_string();
        let digits = &token[4..];
        for refused in [
            String::new(),
            "bst_".to_owned(),
            format!("brt_{digits}"),
            format!("BST_{digits}"),
            format!("{token}0"),
            token[..token.len() - 1].to_owned(),
            format!("bst_{}-{}", &digits[..20], &digits[21..]),
            format!(" {token}"),
        ] {
            assert!(
                Token::parse(PERSONAL, &refused).is_none(),
                "`{refused}` was accepted"
            );
        }
    }

    #[test]
    fn the_checksum_covers_the_prefix() {
        let token = Token::generate("brt_").expect("random bytes").into_string();
        let renamed = format!("bst_{}", &token[4..]);
        assert!(Token::parse("brt_", &token).is_some());
        assert!(Token::parse(PERSONAL, &renamed).is_none());
    }

    #[test]
    fn the_hash_is_the_sha256_of_the_whole_text() {
        let token = Token::parse(PERSONAL, &{
            let text = format!("bst_{}", "0".repeat(43));
            let checksum = checksum(&text);
            format!("{text}{checksum}")
        })
        .expect("a well-formed token");
        let text = token.0.clone();
        assert_eq!(token.sha256().len(), 32);
        assert_eq!(
            token.sha256(),
            <sha2::Sha256 as sha2::Digest>::digest(text.as_bytes()).to_vec()
        );
    }
}
