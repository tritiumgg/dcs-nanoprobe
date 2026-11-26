//! Every `pb::Stamp` in this crate is built by `convert::stamp`, which takes
//! the agent boot (spec/records.md, "Stamps"). A struct literal elsewhere
//! could leave the boot out, so this test rejects one.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).expect("read dir") {
        let p = e.expect("entry").path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The byte offsets of `Stamp {` that are not part of a longer name, such
/// as `ReadStamp {`, nor a return type, `-> pb::Stamp {`.
fn literals(text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("Stamp") {
        let at = from + i;
        from = at + "Stamp".len();
        let before = text[..at].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let path =
            text[..at].trim_end_matches(|c: char| c.is_alphanumeric() || c == '_' || c == ':');
        if path.trim_end().ends_with("->") {
            continue;
        }
        if text[from..].trim_start().starts_with('{') {
            found.push(at);
        }
    }
    found
}

#[test]
fn the_scan_finds_a_literal_and_skips_longer_names() {
    assert_eq!(literals("let s = pb::Stamp {"), vec![12]);
    assert_eq!(literals("pb::Stamp\n    {"), vec![4]);
    assert!(literals("ReadStamp { x }").is_empty());
    assert!(literals("pb::Stamp::default()").is_empty());
    assert!(literals("fn f() -> pb::Stamp {").is_empty());
}

#[test]
fn no_stamp_is_built_outside_the_helper() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(!files.is_empty());
    let mut bad = Vec::new();
    for f in files {
        let text = fs::read_to_string(&f).expect("read");
        let mut hits = literals(&text);
        if f.ends_with("convert.rs") {
            // The helper's own literal.
            let helper = text.find("pub fn stamp(").expect("the helper");
            hits.retain(|&at| at < helper || at > helper + 400);
        }
        for at in hits {
            let line = text[..at].lines().count();
            bad.push(format!("{}:{line}", f.display()));
        }
    }
    assert!(bad.is_empty(), "build stamps with convert::stamp: {bad:?}");
}
