use std::io::{self, Read};
use std::mem;

use ruzstd::decoding::StreamingDecoder;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use tokio::task;

pub(crate) const FRAME_LINES: u64 = 1000;

pub(crate) fn lines(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    data.split_inclusive(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\n").unwrap_or(line))
}

#[derive(Default)]
pub(crate) struct Encoder {
    object: Vec<u8>,
    frames: Vec<u64>,
    lines: u64,
    frame: Vec<u8>,
    framed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Encoded {
    pub(crate) bytes: Vec<u8>,
    pub(crate) frames: Vec<u64>,
    pub(crate) lines: u64,
}

impl Encoder {
    pub(crate) async fn push(&mut self, data: &[u8]) -> io::Result<()> {
        for line in lines(data) {
            self.frame.extend_from_slice(line);
            self.frame.push(b'\n');
            self.framed += 1;
            self.lines += 1;
            if self.framed == FRAME_LINES {
                self.seal().await?;
            }
        }
        Ok(())
    }

    async fn seal(&mut self) -> io::Result<()> {
        let raw = mem::take(&mut self.frame);
        self.framed = 0;
        let compressed =
            task::spawn_blocking(move || compress_to_vec(&raw[..], CompressionLevel::Fastest))
                .await
                .map_err(io::Error::other)?;
        self.frames.push(self.object.len() as u64);
        self.object.extend_from_slice(&compressed);
        Ok(())
    }

    pub(crate) async fn finish(mut self) -> io::Result<Option<Encoded>> {
        if self.framed > 0 {
            self.seal().await?;
        }
        if self.lines == 0 {
            return Ok(None);
        }
        Ok(Some(Encoded {
            bytes: self.object,
            frames: self.frames,
            lines: self.lines,
        }))
    }
}

pub(crate) async fn decode(frame: Vec<u8>) -> io::Result<Vec<u8>> {
    task::spawn_blocking(move || {
        let mut decoder = StreamingDecoder::new(&frame[..]).map_err(io::Error::other)?;
        let mut raw = Vec::new();
        decoder.read_to_end(&mut raw)?;
        Ok(raw)
    })
    .await
    .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use ruzstd::decoding::StreamingDecoder;

    use super::{Encoder, FRAME_LINES, decode, lines};

    fn text(count: u64) -> Vec<u8> {
        (1..=count)
            .flat_map(|number| {
                format!("2026-10-25T10:00:00.000000000Z line {number}\n").into_bytes()
            })
            .collect()
    }

    #[test]
    fn a_chunk_splits_into_its_lines_without_their_newlines() {
        let found: Vec<&[u8]> = lines(b"a\n\nb\nc").collect();
        assert_eq!(found, [&b"a"[..], b"", b"b", b"c"]);
        assert_eq!(lines(b"").count(), 0);
    }

    #[tokio::test]
    async fn an_object_is_one_zstd_stream_with_a_frame_every_thousand_lines() {
        let mut encoder = Encoder::default();
        let all = text(2 * FRAME_LINES + 5);
        let (head, tail) = all.split_at(all.len() / 3);
        let cut = head
            .iter()
            .rposition(|byte| *byte == b'\n')
            .expect("a line")
            + 1;
        encoder.push(&all[..cut]).await.expect("pushed");
        encoder.push(&all[cut..cut]).await.expect("pushed");
        let rest: Vec<u8> = head[cut..].iter().chain(tail).copied().collect();
        encoder.push(&rest).await.expect("pushed");
        let encoded = encoder.finish().await.expect("encoded").expect("an object");
        assert_eq!(encoded.lines, 2 * FRAME_LINES + 5);
        assert_eq!(encoded.frames.len(), 3);
        assert_eq!(encoded.frames[0], 0);

        let mut whole = Vec::new();
        let mut input = &encoded.bytes[..];
        while !input.is_empty() {
            let mut decoder = StreamingDecoder::new(&mut input).expect("a frame");
            decoder.read_to_end(&mut whole).expect("a decoded frame");
        }
        assert_eq!(whole, all);

        let bounds: Vec<u64> = encoded
            .frames
            .iter()
            .copied()
            .chain([encoded.bytes.len() as u64])
            .collect();
        let second = &encoded.bytes[bounds[1] as usize..bounds[2] as usize];
        let raw = decode(second.to_vec()).await.expect("the second frame");
        let numbers: Vec<String> = lines(&raw)
            .map(|line| {
                String::from_utf8_lossy(line)
                    .rsplit(' ')
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();
        assert_eq!(numbers.len() as u64, FRAME_LINES);
        assert_eq!(numbers.first().map(String::as_str), Some("1001"));
        assert_eq!(numbers.last().map(String::as_str), Some("2000"));
    }

    #[tokio::test]
    async fn an_encoder_without_lines_makes_no_object() {
        let mut encoder = Encoder::default();
        encoder.push(b"").await.expect("pushed");
        assert_eq!(encoder.finish().await.expect("finished"), None);
    }
}
