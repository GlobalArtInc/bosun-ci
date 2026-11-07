use std::fmt;

const TOKEN: &str = "ACTIONS_RUNTIME_TOKEN";

const RUNTIME_URL: &str = "ACTIONS_RUNTIME_URL";

const RESULTS_URL: &str = "ACTIONS_RESULTS_URL";

const CACHE_URL: &str = "ACTIONS_CACHE_URL";

const CACHE_SERVICE_V2: &str = "ACTIONS_CACHE_SERVICE_V2";

const ENABLED: &str = "True";

#[derive(Clone, PartialEq, Eq)]
pub struct Runtime {
    pub token: String,
    pub runtime_url: String,
    pub results_url: String,
    pub cache_url: String,
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("runtime_url", &self.runtime_url)
            .field("results_url", &self.results_url)
            .field("cache_url", &self.cache_url)
            .finish_non_exhaustive()
    }
}

impl Runtime {
    pub(crate) fn variables(&self) -> [(&'static str, &str); 5] {
        [
            (TOKEN, &self.token),
            (RUNTIME_URL, &self.runtime_url),
            (RESULTS_URL, &self.results_url),
            (CACHE_URL, &self.cache_url),
            (CACHE_SERVICE_V2, ENABLED),
        ]
    }
}
