use bosun_types::id::{JobId, RunId};
use url::Url;

const GRAPHQL: &str = "graphql";

const ENTERPRISE_API: &str = "v3";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Site {
    url: String,
    forge: Option<ForgeSite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForgeSite {
    pub(crate) server_url: String,
    pub(crate) api_url: String,
    pub(crate) graphql_url: String,
}

impl Site {
    pub(crate) fn new(url: Option<String>) -> Site {
        Site {
            url: url
                .map(|url| url.trim_end_matches('/').to_owned())
                .unwrap_or_default(),
            forge: None,
        }
    }

    pub(crate) fn with_forge(mut self, server_url: &Url, api_url: &Url) -> Site {
        let mut graphql = api_url.clone();
        if let Ok(mut segments) = graphql.path_segments_mut() {
            segments.pop_if_empty();
        }
        let enterprise = graphql
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            == Some(ENTERPRISE_API);
        if let Ok(mut segments) = graphql.path_segments_mut() {
            if enterprise {
                segments.pop();
            }
            segments.push(GRAPHQL);
        }
        self.forge = Some(ForgeSite {
            server_url: trimmed(server_url),
            api_url: trimmed(api_url),
            graphql_url: trimmed(&graphql),
        });
        self
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn forge(&self) -> Option<&ForgeSite> {
        self.forge.as_ref()
    }

    pub(crate) fn run_url(&self, run: RunId) -> String {
        if self.url.is_empty() {
            String::new()
        } else {
            format!("{}/runs/{run}", self.url)
        }
    }

    pub(crate) fn job_url(&self, run: RunId, job: JobId) -> String {
        if self.url.is_empty() {
            String::new()
        } else {
            format!("{}/runs/{run}/jobs/{job}", self.url)
        }
    }
}

fn trimmed(url: &Url) -> String {
    url.as_str().trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::Site;

    fn forge(server_url: &str, api_url: &str) -> [String; 3] {
        let parse = |text: &str| Url::parse(text).unwrap_or_else(|error| panic!("{error}"));
        let site = Site::default().with_forge(&parse(server_url), &parse(api_url));
        let forge = site.forge().unwrap_or_else(|| panic!("a forge"));
        [
            forge.server_url.clone(),
            forge.api_url.clone(),
            forge.graphql_url.clone(),
        ]
    }

    #[test]
    fn the_graphql_address_of_the_forge_follows_its_api_address() {
        assert_eq!(
            forge("https://github.com", "https://api.github.com"),
            [
                "https://github.com",
                "https://api.github.com",
                "https://api.github.com/graphql"
            ]
        );
        assert_eq!(
            forge(
                "https://forge.example.org/",
                "https://forge.example.org/api/v3/"
            ),
            [
                "https://forge.example.org",
                "https://forge.example.org/api/v3",
                "https://forge.example.org/api/graphql"
            ]
        );
    }
}
