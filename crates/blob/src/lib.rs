mod directory;
mod key;

use std::fmt;
use std::future::Future;
use std::io;
use std::ops::{Bound, Range, RangeBounds};
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};

pub use directory::Directory;
pub use key::{Key, KeyError};

pub trait Store: Send + Sync {
    fn put<R>(&self, key: &Key, body: R) -> impl Future<Output = Result<(), Error>> + Send
    where
        R: AsyncRead + Send + Unpin;

    fn get<B>(&self, key: &Key, range: B) -> impl Future<Output = Result<Object, Error>> + Send
    where
        B: RangeBounds<u64> + Send;

    fn delete(&self, key: &Key) -> impl Future<Output = Result<(), Error>> + Send;

    fn list(&self, prefix: &str) -> impl Future<Output = Result<Vec<Key>, Error>> + Send;
}

pub struct Object {
    size: u64,
    body: Pin<Box<dyn AsyncRead + Send>>,
}

impl Object {
    fn new(size: u64, body: impl AsyncRead + Send + 'static) -> Self {
        Self {
            size,
            body: Box::pin(body),
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

impl AsyncRead for Object {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.body.as_mut().poll_read(cx, buf)
    }
}

impl fmt::Debug for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Object")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum Error {
    NotFound(Key),
    InvalidRange {
        key: Key,
        start: u64,
        end: Option<u64>,
        size: u64,
    },
    Io {
        target: String,
        source: io::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(key) => write!(f, "object `{key}` does not exist"),
            Self::InvalidRange {
                key,
                start,
                end: Some(end),
                size,
            } => write!(
                f,
                "range {start}..{end} does not fit object `{key}` of {size} bytes"
            ),
            Self::InvalidRange {
                key,
                start,
                end: None,
                size,
            } => write!(
                f,
                "range {start}.. does not fit object `{key}` of {size} bytes"
            ),
            Self::Io { target, source } => {
                write!(f, "object store I/O failed on `{target}`: {source}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::NotFound(_) | Self::InvalidRange { .. } => None,
        }
    }
}

fn clip(key: &Key, range: impl RangeBounds<u64>, size: u64) -> Result<Range<u64>, Error> {
    let start = match range.start_bound() {
        Bound::Included(&start) => Some(start),
        Bound::Excluded(&start) => start.checked_add(1),
        Bound::Unbounded => Some(0),
    };
    let end = match range.end_bound() {
        Bound::Included(&end) => end.checked_add(1),
        Bound::Excluded(&end) => Some(end),
        Bound::Unbounded => None,
    };
    match start {
        Some(start) if start <= size && end.is_none_or(|end| start <= end) => {
            Ok(start..end.map_or(size, |end| end.min(size)))
        }
        _ => Err(Error::InvalidRange {
            key: key.clone(),
            start: start.unwrap_or(u64::MAX),
            end,
            size,
        }),
    }
}
