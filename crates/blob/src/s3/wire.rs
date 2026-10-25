use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct Failure {
    pub(super) code: String,
    #[serde(default)]
    pub(super) message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct Initiated {
    pub(super) upload_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct Listing {
    #[serde(default)]
    pub(super) contents: Vec<Entry>,
    #[serde(default)]
    pub(super) is_truncated: bool,
    pub(super) next_continuation_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct Uploads {
    #[serde(default)]
    pub(super) upload: Vec<Entry>,
    #[serde(default)]
    pub(super) is_truncated: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct Entry {
    pub(super) key: String,
}

pub(super) fn completion(etags: &[String]) -> String {
    let mut body = String::from("<CompleteMultipartUpload>");
    for (number, etag) in (1..).zip(etags) {
        body.push_str(&format!(
            "<Part><PartNumber>{number}</PartNumber><ETag>{}</ETag></Part>",
            escape(etag)
        ));
    }
    body.push_str("</CompleteMultipartUpload>");
    body
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
