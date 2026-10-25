use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

const ALGORITHM: &str = "AWS4-HMAC-SHA256";

const SERVICE: &str = "s3";

const TERMINATOR: &str = "aws4_request";

const SIGNED_HEADERS: &str = "host;x-amz-content-sha256;x-amz-date";

const HEX: &[u8; 16] = b"0123456789abcdef";

const UPPER_HEX: &[u8; 16] = b"0123456789ABCDEF";

pub(super) const EMPTY_PAYLOAD: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

pub(super) struct Signer {
    access_key_id: String,
    secret_access_key: String,
    region: String,
}

pub(super) struct Canonical<'a> {
    pub(super) method: &'a str,
    pub(super) host: &'a str,
    pub(super) path: &'a str,
    pub(super) query: &'a str,
    pub(super) payload: &'a str,
}

pub(super) struct Signature {
    pub(super) date: String,
    pub(super) authorization: String,
}

impl Signer {
    pub(super) fn new(access_key_id: String, secret_access_key: String, region: String) -> Signer {
        Signer {
            access_key_id,
            secret_access_key,
            region,
        }
    }

    pub(super) fn sign(&self, request: &Canonical<'_>, at: DateTime<Utc>) -> Signature {
        let date = at.format("%Y%m%dT%H%M%SZ").to_string();
        let day = &date[..8];
        let scope = format!("{day}/{}/{SERVICE}/{TERMINATOR}", self.region);
        let canonical = format!(
            "{}\n{}\n{}\nhost:{}\nx-amz-content-sha256:{}\nx-amz-date:{date}\n\n{SIGNED_HEADERS}\n{}",
            request.method,
            request.path,
            request.query,
            request.host,
            request.payload,
            request.payload,
        );
        let text = format!(
            "{ALGORITHM}\n{date}\n{scope}\n{}",
            digest(canonical.as_bytes())
        );
        let mut key = hmac(
            format!("AWS4{}", self.secret_access_key).as_bytes(),
            day.as_bytes(),
        );
        for part in [&self.region, SERVICE, TERMINATOR] {
            key = hmac(&key, part.as_bytes());
        }
        let signature = hex(&hmac(&key, text.as_bytes()));
        Signature {
            authorization: format!(
                "{ALGORITHM} Credential={}/{scope}, SignedHeaders={SIGNED_HEADERS}, \
                 Signature={signature}",
                self.access_key_id
            ),
            date,
        }
    }
}

pub(super) fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(super) fn encode(text: &str, keep_slash: bool) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && byte == b'/')
        {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(UPPER_HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(UPPER_HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

pub(super) fn query(pairs: &[(&str, String)]) -> String {
    let mut encoded: Vec<(String, String)> = pairs
        .iter()
        .map(|(name, value)| (encode(name, false), encode(value, false)))
        .collect();
    encoded.sort();
    encoded
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{Canonical, EMPTY_PAYLOAD, Signer, digest, encode, query};

    #[test]
    fn a_listing_is_signed_as_the_s3_documentation_signs_it() {
        let signer = Signer::new(
            "AKIAIOSFODNN7EXAMPLE".to_owned(),
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned(),
            "us-east-1".to_owned(),
        );
        let signature = signer.sign(
            &Canonical {
                method: "GET",
                host: "examplebucket.s3.amazonaws.com",
                path: "/",
                query: &query(&[("max-keys", "2".to_owned()), ("prefix", "J".to_owned())]),
                payload: EMPTY_PAYLOAD,
            },
            Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap(),
        );
        assert_eq!(signature.date, "20130524T000000Z");
        assert_eq!(
            signature.authorization,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
             Signature=34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        );
    }

    #[test]
    fn the_empty_payload_is_the_digest_of_nothing() {
        assert_eq!(digest(b""), EMPTY_PAYLOAD);
    }

    #[test]
    fn everything_but_unreserved_characters_is_percent_encoded() {
        assert_eq!(
            encode("a-b_c.d~e/f g+h%é", true),
            "a-b_c.d~e/f%20g%2Bh%25%C3%A9"
        );
        assert_eq!(encode("a/b", false), "a%2Fb");
        assert_eq!(
            query(&[
                ("uploadId", "x/y=".to_owned()),
                ("partNumber", "10".to_owned()),
                ("uploads", String::new()),
            ]),
            "partNumber=10&uploadId=x%2Fy%3D&uploads="
        );
    }
}
