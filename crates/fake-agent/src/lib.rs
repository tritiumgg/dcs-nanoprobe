//! Test peers for the transport: a fake agent ([`agent`]), a stub sidecar
//! ([`stub`], over [`nanoprobe_sidecar::link`]) and a verifier for the
//! stub's journals ([`verify`]). Test tooling only: built from source, never
//! released.

pub mod agent;
pub mod content;
pub mod stub;
pub mod verify;

/// `s` as a JSON string, with its quotes.
pub fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Read a token file: its first line, without surrounding white space.
pub fn read_token(path: &std::path::Path) -> std::io::Result<String> {
    Ok(std::fs::read_to_string(path)?.trim().to_owned())
}

/// The value after `--name` in `args`.
pub fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Every value after `--name` in `args`.
pub fn args_all(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|w| w[0] == name)
        .map(|w| w[1].clone())
        .collect()
}
