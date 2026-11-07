use std::fs;
use std::io;
use std::ops::RangeBounds;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::http::{Method, StatusCode, header};
use bosun_blob::{Directory, Error, Key, Object, Store};
use serde_json::json;
use tokio::io::{AsyncRead, ReadBuf};

use crate::{Harness, block, identity};

struct Broken(bool);

impl AsyncRead for Broken {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.0 {
            return Poll::Ready(Err(io::Error::other("the disk failed")));
        }
        self.0 = true;
        buf.put_slice(b"partial");
        Poll::Ready(Ok(()))
    }
}

struct Faulty {
    inner: Directory,
    unreadable: Arc<Mutex<Option<String>>>,
    unwritable: Arc<AtomicBool>,
}

impl Store for Faulty {
    async fn put<R>(&self, key: &Key, body: R) -> Result<(), Error>
    where
        R: AsyncRead + Send + Unpin,
    {
        if self.unwritable.load(Ordering::SeqCst) && key.to_string().starts_with("artifacts/") {
            return self.inner.put(key, Broken(false)).await;
        }
        self.inner.put(key, body).await
    }

    async fn get<B>(&self, key: &Key, range: B) -> Result<Object, Error>
    where
        B: RangeBounds<u64> + Send,
    {
        let unreadable = self.unreadable.lock().expect("the fault").clone();
        if let Some(suffix) = unreadable
            && key.to_string().ends_with(&suffix)
        {
            return Err(Error::Io {
                target: key.to_string(),
                source: io::Error::other("the disk failed"),
            });
        }
        self.inner.get(key, range).await
    }

    async fn delete(&self, key: &Key) -> Result<(), Error> {
        self.inner.delete(key).await
    }

    async fn list(&self, prefix: &str) -> Result<Vec<Key>, Error> {
        self.inner.list(prefix).await
    }
}

#[tokio::test]
async fn blocks_staged_out_of_order_assemble_in_block_list_order_and_a_single_put_yields_the_object()
 {
    let harness = Harness::new().await;
    let who = identity(1);
    let token = harness.token(who);

    let upload = harness.create(&token, who, "blocks").await;
    for (number, data) in [(3, "gamma"), (1, "alpha"), (2, "beta")] {
        let staged = harness
            .stage(&upload, &block(number), data.as_bytes())
            .await;
        assert_eq!(staged.status, StatusCode::CREATED);
    }
    let restaged = harness.stage(&upload, &block(2), b"BETA").await;
    assert_eq!(restaged.status, StatusCode::CREATED);
    let committed = harness
        .commit(&upload, &[block(1), block(2), block(3)])
        .await;
    assert_eq!(committed.status, StatusCode::CREATED);
    harness.finalize(&token, who, "blocks", 14).await;
    let download = harness.download_address(&token, who, "blocks").await;
    let read = harness.blob(Method::GET, &download, "", &[], b"").await;
    assert_eq!(read.body, b"alphaBETAgamma");
    assert!(
        harness
            .store()
            .await
            .list("blocks/")
            .await
            .expect("a listing")
            .is_empty()
    );

    let (_, single) = harness.publish("single", b"the whole object").await;
    let read = harness.blob(Method::GET, &single, "", &[], b"").await;
    assert_eq!(read.body, b"the whole object");
}

#[tokio::test]
async fn a_block_list_naming_a_block_never_staged_is_refused_and_no_object_appears() {
    let harness = Harness::new().await;
    let who = identity(1);
    let token = harness.token(who);
    let upload = harness.create(&token, who, "partial").await;
    harness.stage(&upload, &block(1), b"one").await;

    let refused = harness.commit(&upload, &[block(1), block(9)]).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        refused
            .headers
            .get("x-ms-error-code")
            .map(|value| value.as_bytes()),
        Some(&b"InvalidBlockList"[..])
    );
    assert!(
        harness
            .store()
            .await
            .list("artifacts/")
            .await
            .expect("a listing")
            .is_empty()
    );

    let finalized = harness
        .twirp(
            Some(&token),
            "FinalizeArtifact",
            &json!({
                "workflow_run_backend_id": who.attempt.to_string(),
                "workflow_job_run_backend_id": who.execution.to_string(),
                "name": "partial",
                "size": "3",
            }),
        )
        .await;
    assert_eq!(finalized.status, StatusCode::PRECONDITION_FAILED);
}

#[tokio::test]
async fn downloads_answer_head_and_both_range_headers() {
    let harness = Harness::new().await;
    let (_, download) = harness.publish("ranges", b"0123456789").await;
    let get = |headers: Vec<(&'static str, &'static str)>| {
        let harness = &harness;
        let download = download.clone();
        async move {
            harness
                .blob(Method::GET, &download, "", &headers, b"")
                .await
        }
    };

    let whole = get(vec![]).await;
    assert_eq!(whole.status, StatusCode::OK);
    assert_eq!(whole.headers[header::CONTENT_TYPE], "application/zip");
    assert_eq!(whole.headers[header::ACCEPT_RANGES], "bytes");
    assert_eq!(whole.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");

    let head = harness.blob(Method::HEAD, &download, "", &[], b"").await;
    assert_eq!(head.status, StatusCode::OK);
    assert_eq!(head.headers[header::CONTENT_LENGTH], "10");
    assert!(head.body.is_empty());

    for (headers, status, body, content_range) in [
        (
            vec![("range", "bytes=2-4")],
            206,
            "234",
            Some("bytes 2-4/10"),
        ),
        (
            vec![("x-ms-range", "bytes=7-")],
            206,
            "789",
            Some("bytes 7-9/10"),
        ),
        (
            vec![("range", "bytes=-3")],
            206,
            "789",
            Some("bytes 7-9/10"),
        ),
        (
            vec![("range", "bytes=8-99")],
            206,
            "89",
            Some("bytes 8-9/10"),
        ),
        (
            vec![("range", "bytes=0-1"), ("x-ms-range", "bytes=5-5")],
            206,
            "5",
            Some("bytes 5-5/10"),
        ),
        (vec![("range", "bytes=10-")], 416, "", Some("bytes */10")),
        (vec![("range", "bytes=-0")], 416, "", Some("bytes */10")),
    ] {
        let reply = get(headers.clone()).await;
        assert_eq!(reply.status.as_u16(), status, "{headers:?}");
        if status == 206 {
            assert_eq!(reply.body, body.as_bytes(), "{headers:?}");
        }
        assert_eq!(
            reply
                .headers
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok()),
            content_range,
            "{headers:?}"
        );
    }
}

#[tokio::test]
async fn a_download_carries_the_content_type_declared_when_the_artifact_was_created() {
    let harness = Harness::new().await;
    let who = identity(1);
    let token = harness.token(who);
    let created = harness
        .twirp(
            Some(&token),
            "CreateArtifact",
            &json!({
                "workflow_run_backend_id": who.attempt.to_string(),
                "workflow_job_run_backend_id": who.execution.to_string(),
                "name": "notes.txt",
                "mime_type": "text/plain",
            }),
        )
        .await;
    let upload = created.json()["signed_upload_url"]
        .as_str()
        .expect("an address")
        .to_owned();
    harness
        .blob(
            Method::PUT,
            &upload,
            "",
            &[("x-ms-blob-type", "BlockBlob")],
            b"text",
        )
        .await;
    harness.finalize(&token, who, "notes.txt", 4).await;
    let download = harness.download_address(&token, who, "notes.txt").await;
    let read = harness.blob(Method::GET, &download, "", &[], b"").await;
    assert_eq!(read.headers[header::CONTENT_TYPE], "text/plain");
    assert_eq!(read.body, b"text");
}

#[tokio::test]
async fn malformed_blob_requests_are_refused_without_a_panic() {
    let harness = Harness::new().await;
    let who = identity(1);
    let token = harness.token(who);
    let upload = harness.create(&token, who, "bad").await;
    let (_, download) = harness.publish("good", b"0123456789").await;
    let typed = [("x-ms-blob-type", "BlockBlob")];

    let queries = [
        "comp=block",
        "comp=block&blockid=",
        "comp=block&blockid=%00%00",
        "comp=block&blockid=a%20b",
        "comp=block&blockid=a&blockid=b",
        "comp=block&blockid=%FF%FE",
        "comp=blocklist&blockid=YQ%3D%3D",
        "comp=lease",
        "comp=page",
        "comp=metadata",
        "comp=",
        "",
    ];
    let long = "A".repeat(89);
    for query in queries.iter().map(|query| (*query).to_owned()).chain([
        format!("comp=block&blockid={long}"),
        format!("comp=block&blockid={}", "A".repeat(60_000)),
    ]) {
        let reply = harness.blob(Method::PUT, &upload, &query, &[], b"x").await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{:.60}", query);
    }
    let wrong_type = harness
        .blob(
            Method::PUT,
            &upload,
            "",
            &[("x-ms-blob-type", "PageBlob")],
            b"x",
        )
        .await;
    assert_eq!(wrong_type.status, StatusCode::BAD_REQUEST);
    let unknown = harness
        .blob(Method::PUT, &upload, "comp=lease", &typed, b"x")
        .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);

    let lists: [&[u8]; 14] = [
        b"",
        b"not xml",
        b"<BlockList>",
        b"<Other><Latest>YQ==</Latest></Other>",
        b"<BlockList><Latest>YQ==</Latest><Latest>YQ==</Latest></BlockList>",
        b"<BlockList><Committed>YQ==</Committed></BlockList>",
        b"<BlockList><Latest></Latest></BlockList>",
        b"<BlockList><Latest>a b</Latest></BlockList>",
        b"<BlockList><Latest>&amp;</Latest></BlockList>",
        b"<BlockList><Unknown>YQ==</Unknown></BlockList>",
        b"<!DOCTYPE x [<!ENTITY a \"b\">]><BlockList><Latest>&a;</Latest></BlockList>",
        b"<BlockList><Latest><Latest>YQ==</Latest></Latest></BlockList>",
        b"\xff\xfe<BlockList/>",
        b"<BlockList><Latest>YQ==</Latest></BlockList>",
    ];
    for list in lists {
        let reply = harness
            .blob(Method::PUT, &upload, "comp=blocklist", &[], list)
            .await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{}",
            String::from_utf8_lossy(list)
        );
    }

    let many: String = (0..50_001)
        .map(|n| format!("<Latest>{n:08}</Latest>"))
        .collect();
    let oversized = format!("<BlockList>{many}</BlockList>");
    let reply = harness
        .blob(
            Method::PUT,
            &upload,
            "comp=blocklist",
            &[],
            oversized.as_bytes(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let huge = vec![b' '; 7_000_000];
    let reply = harness
        .blob(Method::PUT, &upload, "comp=blocklist", &[], &huge)
        .await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);

    for range in [
        "bytes=5-2",
        "bytes=a-b",
        "bytes=-",
        "bytes=",
        "bytes",
        "items=0-1",
        "bytes=0-1,3-4",
        "bytes=99999999999999999999-",
        "bytes=-99999999999999999999",
        "bytes=1-2-3",
        "bytes= 1 -2",
    ] {
        let reply = harness
            .blob(Method::GET, &download, "", &[("range", range)], b"")
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{range}");
    }
    let opaque = harness
        .blob(Method::GET, &download, "", &[("x-ms-range", "\u{e9}")], b"")
        .await;
    assert_eq!(opaque.status, StatusCode::BAD_REQUEST);
    let listing = harness
        .blob(Method::GET, &download, "comp=blocklist", &[], b"")
        .await;
    assert_eq!(listing.status, StatusCode::BAD_REQUEST);
    let delete = harness.blob(Method::DELETE, &download, "", &[], b"").await;
    assert_eq!(delete.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn a_block_list_that_fails_while_assembling_answers_500_and_leaves_no_object_or_staging_file()
{
    let unreadable = Arc::new(Mutex::new(None));
    let unwritable = Arc::new(AtomicBool::new(false));
    let (blocks, writes) = (Arc::clone(&unreadable), Arc::clone(&unwritable));
    let harness = Harness::configured(
        move |inner| Faulty {
            inner,
            unreadable: blocks,
            unwritable: writes,
        },
        bosun_services::MAX_BLOCKS,
    )
    .await;
    let who = identity(1);
    let token = harness.token(who);
    let upload = harness.create(&token, who, "faulty").await;
    for number in [1, 2] {
        let staged = harness.stage(&upload, &block(number), b"data").await;
        assert_eq!(staged.status, StatusCode::CREATED);
    }
    let list = [block(1), block(2)];
    let hex: String = block(2).bytes().map(|byte| format!("{byte:02x}")).collect();
    *unreadable.lock().expect("the fault") = Some(hex);
    let store = harness.store().await;

    for writing in [false, true] {
        if writing {
            *unreadable.lock().expect("the fault") = None;
            unwritable.store(true, Ordering::SeqCst);
        }
        let reply = tokio::time::timeout(Duration::from_secs(10), harness.commit(&upload, &list))
            .await
            .expect("the commit does not hang");
        assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR, "{writing}");
        assert!(
            store
                .list("artifacts/")
                .await
                .expect("a listing")
                .is_empty()
        );
        let staging = fs::read_dir(harness.root().join("staging")).expect("the staging directory");
        assert_eq!(staging.count(), 0, "{writing}");
    }
}

#[tokio::test]
async fn a_download_names_the_file_after_the_artifact_and_never_as_a_path() {
    let harness = Harness::new().await;
    for (name, expected) in [
        (
            "report.txt",
            r#"attachment; filename="report.txt"; filename*=UTF-8''report.txt"#,
        ),
        (
            "na\u{ef}ve.txt",
            r#"attachment; filename="na_ve.txt"; filename*=UTF-8''na%C3%AFve.txt"#,
        ),
        (
            r#"a/b/../we"ird.txt"#,
            r#"attachment; filename="we\"ird.txt"; filename*=UTF-8''we%22ird.txt"#,
        ),
        (
            "up/..",
            r#"attachment; filename="artifact"; filename*=UTF-8''artifact"#,
        ),
    ] {
        let (_, download) = harness.publish(name, b"content").await;
        let read = harness.blob(Method::GET, &download, "", &[], b"").await;
        assert_eq!(
            read.headers[header::CONTENT_DISPOSITION]
                .to_str()
                .expect("ASCII"),
            expected,
            "{name}"
        );
    }
}

#[tokio::test]
async fn the_fifty_thousand_block_cap_is_enforced_per_distinct_block_id_with_a_409() {
    let harness = Harness::limited(3).await;
    let who = identity(1);
    let token = harness.token(who);
    let upload = harness.create(&token, who, "capped").await;
    for number in [1, 2, 3] {
        let staged = harness.stage(&upload, &block(number), b"ab").await;
        assert_eq!(staged.status, StatusCode::CREATED);
    }
    let over = harness.stage(&upload, &block(4), b"ab").await;
    assert_eq!(over.status, StatusCode::CONFLICT);
    assert_eq!(
        over.headers
            .get("x-ms-error-code")
            .map(|value| value.as_bytes()),
        Some(&b"BlockCountExceedsLimit"[..])
    );
    let retry = harness.stage(&upload, &block(2), b"cd").await;
    assert_eq!(retry.status, StatusCode::CREATED);
    let staged = harness
        .store()
        .await
        .list("blocks/")
        .await
        .expect("a listing");
    assert_eq!(staged.len(), 3);

    let committed = harness
        .commit(&upload, &[block(1), block(2), block(3)])
        .await;
    assert_eq!(committed.status, StatusCode::CREATED);
    harness.finalize(&token, who, "capped", 6).await;
}
