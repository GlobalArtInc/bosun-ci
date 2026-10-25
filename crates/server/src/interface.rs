use axum::body::Body;
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

use crate::envelope;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/interface.rs"));
}

const PAGE: &str = "index.html";

const ASSETS: &str = "assets/";

const IMMUTABLE: &str = "public, max-age=31536000, immutable";

const REVALIDATE: &str = "no-cache";

const FRAMING: &str = "frame-ancestors 'none'";

const NOSNIFF: &str = "nosniff";

const RESERVED: [&str; 6] = ["/api", "/hooks", "/_runtime", "/twirp", "/mcp", "/assets"];

const UNBUILT: &str = "this bosun-server was built without its web interface; build it in web/ \
                       with `corepack pnpm install && corepack pnpm build`, then build \
                       bosun-server again\n";

#[derive(Clone, Copy)]
pub(crate) struct Interface {
    files: &'static [(&'static str, &'static [u8])],
}

impl Interface {
    pub(crate) fn embedded() -> Interface {
        Interface {
            files: embedded::FILES,
        }
    }

    pub(crate) fn is_empty(self) -> bool {
        self.files.is_empty()
    }

    fn file(self, name: &str) -> Option<&'static [u8]> {
        self.files
            .binary_search_by(|(path, _)| (*path).cmp(name))
            .ok()
            .map(|index| self.files[index].1)
    }
}

pub(crate) async fn answer(interface: Interface, method: Method, uri: Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return envelope::absent(&method, &uri).into_response();
    }
    let path = uri.path();
    let name = path.trim_start_matches('/');
    if !name.is_empty()
        && let Some(bytes) = interface.file(name)
    {
        let cache = if name.starts_with(ASSETS) {
            IMMUTABLE
        } else {
            REVALIDATE
        };
        return file(name, bytes, cache);
    }
    if reserved(path) {
        return envelope::absent(&method, &uri).into_response();
    }
    match interface.file(PAGE) {
        Some(page) => file(PAGE, page, REVALIDATE),
        None => (
            StatusCode::NOT_FOUND,
            [
                (CONTENT_TYPE, "text/plain; charset=utf-8"),
                (CACHE_CONTROL, REVALIDATE),
            ],
            UNBUILT,
        )
            .into_response(),
    }
}

fn reserved(path: &str) -> bool {
    RESERVED.iter().any(|prefix| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

fn file(name: &str, bytes: &'static [u8], cache: &'static str) -> Response {
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(media_type(name)));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static(NOSNIFF));
    if name == PAGE {
        headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(FRAMING));
    }
    response
}

fn media_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::{self, Body};
    use axum::http::{HeaderMap, Method, Request, StatusCode, Uri};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::{Interface, UNBUILT, answer};

    const PAGE: &[u8] = b"<!doctype html><div id=\"root\"></div>";

    const SCRIPT: &[u8] = b"export const bosun = 1;";

    static BUILT: Interface = Interface {
        files: &[
            ("assets/index-B2kq9xZt.js", SCRIPT),
            ("assets/index-DmP4c1Lw.css", b"body{margin:0}"),
            (
                "favicon.svg",
                b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
            ),
            ("index.html", PAGE),
        ],
    };

    static UNBUILT_INTERFACE: Interface = Interface { files: &[] };

    struct Answer {
        status: StatusCode,
        headers: HeaderMap,
        body: Vec<u8>,
    }

    impl Answer {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .get(name)
                .map(|value| value.to_str().expect("a textual header"))
        }
    }

    async fn ask(interface: Interface, method: Method, path: &str) -> Answer {
        let router =
            Router::new().fallback(move |method: Method, uri: Uri| answer(interface, method, uri));
        let response = router
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await
            .expect("an answer");
        let status = response.status();
        let headers = response.headers().clone();
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("a body")
            .to_vec();
        Answer {
            status,
            headers,
            body,
        }
    }

    fn not_found(method: &str, path: &str) -> Value {
        json!({
            "error": {
                "code": "not_found",
                "message": format!("there is no {method} {path}"),
                "fields": [],
                "diagnostics": [],
            }
        })
    }

    #[tokio::test]
    async fn a_hashed_asset_is_served_as_immutable() {
        let script = ask(BUILT, Method::GET, "/assets/index-B2kq9xZt.js").await;
        assert_eq!(script.status, StatusCode::OK);
        assert_eq!(
            script.header("cache-control"),
            Some("public, max-age=31536000, immutable")
        );
        assert_eq!(
            script.header("content-type"),
            Some("text/javascript; charset=utf-8")
        );
        assert_eq!(script.header("x-content-type-options"), Some("nosniff"));
        assert_eq!(script.body, SCRIPT);
        let style = ask(BUILT, Method::GET, "/assets/index-DmP4c1Lw.css").await;
        assert_eq!(
            style.header("content-type"),
            Some("text/css; charset=utf-8")
        );
        assert_eq!(
            style.header("cache-control"),
            Some("public, max-age=31536000, immutable")
        );
    }

    #[tokio::test]
    async fn the_page_and_every_client_route_are_the_page_revalidated_on_each_navigation() {
        for path in [
            "/",
            "/index.html",
            "/sign-in",
            "/runs/0192f3a4-5b6c-7d8e-9f01-000000000003",
            "/projects/globalart/keel?branch=main",
            "/apiary",
        ] {
            let page = ask(BUILT, Method::GET, path).await;
            assert_eq!(page.status, StatusCode::OK, "{path}");
            assert_eq!(page.header("cache-control"), Some("no-cache"), "{path}");
            assert_eq!(
                page.header("content-type"),
                Some("text/html; charset=utf-8"),
                "{path}"
            );
            assert_eq!(
                page.header("content-security-policy"),
                Some("frame-ancestors 'none'"),
                "{path}"
            );
            assert_eq!(page.body, PAGE, "{path}");
        }
        let head = ask(
            BUILT,
            Method::HEAD,
            "/runs/0192f3a4-5b6c-7d8e-9f01-000000000003",
        )
        .await;
        assert_eq!(head.status, StatusCode::OK);
        assert_eq!(head.header("cache-control"), Some("no-cache"));
    }

    #[tokio::test]
    async fn a_file_outside_the_assets_is_served_revalidated() {
        let icon = ask(BUILT, Method::GET, "/favicon.svg").await;
        assert_eq!(icon.status, StatusCode::OK);
        assert_eq!(icon.header("content-type"), Some("image/svg+xml"));
        assert_eq!(icon.header("cache-control"), Some("no-cache"));
        assert_eq!(icon.header("content-security-policy"), None);
    }

    #[tokio::test]
    async fn the_paths_of_the_server_keep_their_own_answers() {
        for path in [
            "/api",
            "/api/v1/nothing",
            "/hooks/github",
            "/_runtime/cache/",
            "/twirp/github.actions.results.api.v1.CacheService/GetCacheEntryDownloadURL",
            "/mcp",
            "/assets/index-0ld0ne00.js",
        ] {
            let answer = ask(BUILT, Method::GET, path).await;
            assert_eq!(answer.status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(
                answer.header("content-type"),
                Some("application/json"),
                "{path}"
            );
            let body: Value = serde_json::from_slice(&answer.body).expect("a JSON body");
            assert_eq!(body, not_found("GET", path), "{path}");
        }
        let posted = ask(
            BUILT,
            Method::POST,
            "/runs/0192f3a4-5b6c-7d8e-9f01-000000000003",
        )
        .await;
        assert_eq!(posted.status, StatusCode::NOT_FOUND);
        let body: Value = serde_json::from_slice(&posted.body).expect("a JSON body");
        assert_eq!(
            body,
            not_found("POST", "/runs/0192f3a4-5b6c-7d8e-9f01-000000000003")
        );
    }

    #[tokio::test]
    async fn a_server_built_without_the_interface_says_how_to_build_it() {
        let page = ask(UNBUILT_INTERFACE, Method::GET, "/runs/1").await;
        assert_eq!(page.status, StatusCode::NOT_FOUND);
        assert_eq!(
            page.header("content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(page.body, UNBUILT.as_bytes());
        let api = ask(UNBUILT_INTERFACE, Method::GET, "/api/v1/nothing").await;
        let body: Value = serde_json::from_slice(&api.body).expect("a JSON body");
        assert_eq!(body, not_found("GET", "/api/v1/nothing"));
    }
}
