use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use reqwest::header::HeaderValue;
use url::Url;

const ENTRIES: usize = 1024;

const BODY: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Holder {
    App,
    Installation(u64),
    Anonymous,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Key {
    holder: Holder,
    accept: &'static str,
    url: String,
}

impl Key {
    pub(super) fn new(holder: Holder, accept: &'static str, url: &Url) -> Key {
        Key {
            holder,
            accept,
            url: url.as_str().to_owned(),
        }
    }
}

#[derive(Clone)]
pub(super) struct Stored {
    pub(super) etag: HeaderValue,
    pub(super) body: Arc<[u8]>,
    pub(super) next: Option<String>,
}

#[derive(Default)]
pub(super) struct Cache {
    entries: Mutex<Entries>,
}

#[derive(Default)]
struct Entries {
    stored: HashMap<Key, Stored>,
    order: VecDeque<Key>,
}

impl Cache {
    pub(super) fn get(&self, key: &Key) -> Option<Stored> {
        self.lock().stored.get(key).cloned()
    }

    pub(super) fn put(&self, key: Key, stored: Stored) {
        if stored.body.len() > BODY {
            return;
        }
        let mut entries = self.lock();
        if entries.stored.insert(key.clone(), stored).is_none() {
            entries.order.push_back(key);
        }
        while entries.order.len() > ENTRIES {
            if let Some(oldest) = entries.order.pop_front() {
                entries.stored.remove(&oldest);
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, Entries> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
