//! Writes one production record of each kind to `$RECORD_SAMPLES`, for
//! `make lex-validate` to check against the lexicons. Without the variable
//! it only builds them.

use threaded_auth::client::Namespace;
use threaded_auth::records::{comment_record, submission_record, vote_record, RecordRef};

#[test]
fn record_samples() {
    let ns = Namespace::PRODUCTION;
    let story = RecordRef {
        uri: "at://did:plc:ewvi7nxzyoun6zhxrhs64oiz/at.threaded.submission/3mabcdefghi2k".into(),
        cid: "bafyreie5737gdxlw5i64vzichcalba3z2v5n6icifvx5xytvske7mr3hpm".into(),
    };
    let parent = RecordRef {
        uri: "at://did:plc:ewvi7nxzyoun6zhxrhs64oiz/at.threaded.comment/3mabcdefghi3k".into(),
        cid: story.cid.clone(),
    };
    let samples = [
        (
            "submission-link",
            submission_record(
                ns,
                "Writing a B-tree",
                Some("https://example.com/b"),
                "",
                &["rust"],
                false,
                1_790_000_000,
            ),
        ),
        (
            "submission-full",
            submission_record(
                ns,
                "Show: a thing",
                Some("https://example.com/s"),
                "Notes.",
                &["show", "rust"],
                true,
                1_790_000_000,
            ),
        ),
        (
            "submission-text",
            submission_record(
                ns,
                "Ask: why?",
                None,
                "Body.",
                &["ask"],
                false,
                1_790_000_000,
            ),
        ),
        (
            "comment",
            comment_record(ns, &story, None, "A *comment*.", 1_790_000_000),
        ),
        (
            "comment-reply",
            comment_record(ns, &story, Some(&parent), "A reply.", 1_790_000_000),
        ),
        ("vote", vote_record(ns, &story, 1_790_000_000)),
    ];
    let Some(dir) = std::env::var_os("RECORD_SAMPLES") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    for (name, rec) in samples {
        let path = std::path::Path::new(&dir).join(format!("{name}.json"));
        std::fs::write(path, serde_json::to_string_pretty(&rec).unwrap()).unwrap();
    }
}
