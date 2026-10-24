use bosun_blob::Key;

#[test]
fn keys_of_the_storage_layout_are_accepted() {
    for key in [
        "logs/0192e4a0-7c1d-7000-8000-000000000001/1/0192e4a0-7c1d-7000-8000-000000000002/3.log.zst",
        "summaries/0192e4a0-7c1d-7000-8000-000000000002/3.md",
        "artifacts/0192e4a0-7c1d-7000-8000-000000000003/0192e4a0-7c1d-7000-8000-000000000004",
        "blocks/0192e4a0-7c1d-7000-8000-000000000005/000001",
        "actions/GlobalArtInc/.github/1f2e3d4c5b6a.tar.gz",
        "tools/node/20.11.1/linux-x64.tar.xz",
        "terminals/0192e4a0-7c1d-7000-8000-000000000006.cast",
        "a",
        ".a/..b/...",
    ] {
        let parsed = Key::new(key).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(parsed.to_string(), key);
    }
}

#[test]
fn keys_at_the_length_limits_are_accepted() {
    let segment = "s".repeat(255);
    Key::new(segment.clone()).unwrap();
    let longest = [segment.as_str(), &segment, &segment, &"t".repeat(254), "u"].join("/");
    assert_eq!(longest.len(), 1024);
    Key::new(longest).unwrap();
}

#[test]
fn malformed_keys_are_refused_with_the_reason() {
    let long_segment = "s".repeat(256);
    let long_key = ["s"; 513].join("/");
    let cases = [
        ("", "it is empty"),
        ("/a", "it has an empty segment"),
        ("a/", "it has an empty segment"),
        ("a//b", "it has an empty segment"),
        (".", "it has a `.` or `..` segment"),
        ("..", "it has a `.` or `..` segment"),
        ("a/./b", "it has a `.` or `..` segment"),
        ("a/../b", "it has a `.` or `..` segment"),
        ("a\0b", "it contains a control character"),
        ("a\nb", "it contains a control character"),
        ("a\u{7f}", "it contains a control character"),
        (
            long_segment.as_str(),
            "it has a segment longer than 255 bytes",
        ),
        (long_key.as_str(), "it is longer than 1024 bytes"),
    ];
    for (key, reason) in cases {
        let error = Key::new(key).expect_err(key);
        assert!(
            error.to_string().ends_with(reason),
            "{key:?}: {error} does not say {reason}"
        );
    }
}
