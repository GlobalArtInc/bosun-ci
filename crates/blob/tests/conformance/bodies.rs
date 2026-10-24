use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bosun_blob::Object;
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};
use tokio::sync::Notify;

pub fn bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut bytes = Vec::with_capacity(len + 8);
    while bytes.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.extend_from_slice(&state.to_le_bytes());
    }
    bytes.truncate(len);
    bytes
}

pub async fn read(mut object: Object) -> Vec<u8> {
    let mut body = Vec::new();
    object
        .read_to_end(&mut body)
        .await
        .expect("the object reads to its end");
    body
}

pub struct Failing {
    data: Vec<u8>,
    position: usize,
}

impl Failing {
    pub fn after(data: Vec<u8>) -> Self {
        Self { data, position: 0 }
    }
}

impl AsyncRead for Failing {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.position == self.data.len() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "the sender went away",
            )));
        }
        let end = self.data.len().min(self.position + buf.remaining());
        buf.put_slice(&self.data[self.position..end]);
        self.position = end;
        Poll::Ready(Ok(()))
    }
}

pub struct Stalling {
    data: Vec<u8>,
    position: usize,
    stalled: Arc<Notify>,
}

impl Stalling {
    pub fn after(data: Vec<u8>, stalled: Arc<Notify>) -> Self {
        Self {
            data,
            position: 0,
            stalled,
        }
    }
}

impl AsyncRead for Stalling {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.position == self.data.len() {
            self.stalled.notify_one();
            return Poll::Pending;
        }
        let end = self.data.len().min(self.position + buf.remaining());
        buf.put_slice(&self.data[self.position..end]);
        self.position = end;
        Poll::Ready(Ok(()))
    }
}
