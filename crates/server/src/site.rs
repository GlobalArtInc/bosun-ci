use bosun_types::id::RunId;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Site {
    url: String,
}

impl Site {
    pub(crate) fn new(url: Option<String>) -> Site {
        Site {
            url: url
                .map(|url| url.trim_end_matches('/').to_owned())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn run_url(&self, run: RunId) -> String {
        if self.url.is_empty() {
            String::new()
        } else {
            format!("{}/runs/{run}", self.url)
        }
    }
}
