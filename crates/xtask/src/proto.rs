//! Generates the Rust code of the public API schema in `crates/proto/src/gen/`.
//!
//! Buf owns the schema: `buf build` compiles `proto/` to a file descriptor set,
//! and `tonic-prost-build` turns that set into message types and gRPC stubs.
//! The generated files and the descriptor set (which the server's reflection
//! service serves) are committed, so building the workspace needs neither
//! `buf` nor `protoc`. `--check` regenerates into a scratch folder and fails
//! when the committed files differ.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use prost::Message as _;

/// The folder of generated files, from the repository root.
const GEN: &str = "crates/proto/src/gen";

/// The descriptor set's file name in [`GEN`].
const DESCRIPTOR: &str = "descriptor.binpb";

/// Writes the generated files, or with `check` fails when they are out of date.
pub fn command(repository: &Path, check: bool) -> Result<ExitCode, String> {
    let scratch = repository.join("target").join("xtask-proto");
    let _ = std::fs::remove_dir_all(&scratch);
    let out = scratch.join("gen");
    std::fs::create_dir_all(&out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    generate(repository, &out)?;

    let committed = repository.join(GEN);
    if check {
        let stale = differences(&out, &committed)?;
        let _ = std::fs::remove_dir_all(&scratch);
        if !stale.is_empty() {
            return Err(format!(
                "generated API code is out of date ({}); run `cargo xtask proto`",
                stale.join(", ")
            ));
        }
        println!("ok  {GEN} matches proto/");
        return Ok(ExitCode::SUCCESS);
    }
    let _ = std::fs::remove_dir_all(&committed);
    std::fs::create_dir_all(&committed)
        .map_err(|e| format!("cannot create {}: {e}", committed.display()))?;
    for name in names(&out)? {
        std::fs::copy(out.join(&name), committed.join(&name))
            .map_err(|e| format!("cannot write {name}: {e}"))?;
    }
    let _ = std::fs::remove_dir_all(&scratch);
    println!("wrote {GEN}");
    Ok(ExitCode::SUCCESS)
}

/// Runs `buf build` and the code generator into `out`.
fn generate(repository: &Path, out: &Path) -> Result<(), String> {
    let descriptor = out.join(DESCRIPTOR);
    let status = Command::new("buf")
        .current_dir(repository)
        .arg("build")
        .arg("--as-file-descriptor-set")
        .arg("-o")
        .arg(format!("{}#format=binpb", descriptor.display()))
        .status()
        .map_err(|e| format!("cannot run buf (run `mise install`): {e}"))?;
    if !status.success() {
        return Err("buf build failed".into());
    }
    let bytes =
        std::fs::read(&descriptor).map_err(|e| format!("cannot read the descriptor set: {e}"))?;
    let set = prost_types::FileDescriptorSet::decode(bytes.as_slice())
        .map_err(|e| format!("cannot decode the descriptor set: {e}"))?;
    tonic_prost_build::configure()
        .out_dir(out)
        // Maps encode in key order, so one request always encodes to the same
        // bytes: the idempotency check compares encodings.
        .btree_map(".")
        .build_client(true)
        .build_server(true)
        .compile_fds(set)
        .map_err(|e| format!("code generation failed: {e}"))?;
    Ok(())
}

/// The file names in a folder, sorted.
fn names(dir: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        v.push(e.file_name().to_string_lossy().into_owned());
    }
    v.sort();
    Ok(v)
}

/// The names of files that differ between two folders, or exist in one only.
fn differences(fresh: &Path, committed: &Path) -> Result<Vec<String>, String> {
    let a = names(fresh)?;
    let b = if committed.is_dir() {
        names(committed)?
    } else {
        Vec::new()
    };
    let mut all: Vec<&String> = a.iter().chain(b.iter()).collect();
    all.sort();
    all.dedup();
    let read = |p: PathBuf| std::fs::read(p).ok().map(normalize);
    Ok(all
        .into_iter()
        .filter(|n| read(fresh.join(n)) != read(committed.join(n)))
        .cloned()
        .collect())
}

/// Text files compare without carriage returns, which git may add on Windows.
fn normalize(b: Vec<u8>) -> Vec<u8> {
    if b.contains(&0) {
        b
    } else {
        b.into_iter().filter(|&c| c != b'\r').collect()
    }
}
