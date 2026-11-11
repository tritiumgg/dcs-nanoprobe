//! Symbols for crash records: every DLL that xtask deploys is kept with its PDB
//! under `target/symbols/<pdb>/`, keyed by the PDB signature the DLL carries,
//! and `symbolize` turns the module offsets in `logs/crash.log` into functions
//! and source lines with `llvm-symbolizer`.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use serde_json::Value;

/// The folder that holds every kept build.
fn folder() -> PathBuf {
    crate::repository().join("target/symbols")
}

/// A little-endian integer at `at` in `bytes`.
fn le(bytes: &[u8], at: usize, n: usize) -> Option<u64> {
    let slice = bytes.get(at..at + n)?;
    Some(
        slice
            .iter()
            .rev()
            .fold(0u64, |acc, &b| (acc << 8) | u64::from(b)),
    )
}

/// The facts of a PE32+ file that symbolizing needs.
#[derive(Debug, PartialEq, Eq)]
pub struct Image {
    /// The preferred load address.
    pub base: u64,
    /// The CodeView signature: the PDB GUID in 32 hex digits, then the age,
    /// as the agent's crash record writes it.
    pub pdb: String,
}

/// Read the image base and the PDB signature of a PE32+ file.
pub fn image(bytes: &[u8]) -> Option<Image> {
    let nt = le(bytes, 0x3c, 4)? as usize;
    if le(bytes, nt, 4)? != 0x0000_4550 {
        return None;
    }
    let sections = le(bytes, nt + 6, 2)? as usize;
    let optional_size = le(bytes, nt + 20, 2)? as usize;
    let optional = nt + 24;
    if le(bytes, optional, 2)? != 0x20b {
        return None;
    }
    let base = le(bytes, optional + 24, 8)?;
    let debug_rva = le(bytes, optional + 112 + 6 * 8, 4)? as usize;
    let debug_size = le(bytes, optional + 112 + 6 * 8 + 4, 4)? as usize;
    let table = optional + optional_size;
    // A relative virtual address to a file offset, through the section table.
    let offset = |rva: usize| -> Option<usize> {
        (0..sections).find_map(|i| {
            let s = table + i * 40;
            let virtual_address = le(bytes, s + 12, 4)? as usize;
            let raw_size = le(bytes, s + 16, 4)? as usize;
            let raw = le(bytes, s + 20, 4)? as usize;
            (rva >= virtual_address && rva < virtual_address + raw_size)
                .then(|| raw + rva - virtual_address)
        })
    };
    let start = offset(debug_rva)?;
    for entry in (start..start + debug_size).step_by(28) {
        if le(bytes, entry + 12, 4)? != 2 {
            continue;
        }
        let data = le(bytes, entry + 24, 4)? as usize;
        if le(bytes, data, 4)? != 0x5344_5352 {
            continue;
        }
        let guid = data + 4;
        let mut pdb = format!(
            "{:08X}{:04X}{:04X}",
            le(bytes, guid, 4)?,
            le(bytes, guid + 4, 2)?,
            le(bytes, guid + 6, 2)?
        );
        for i in 8..16 {
            pdb.push_str(&format!("{:02X}", bytes.get(guid + i)?));
        }
        pdb.push_str(&format!("{:X}", le(bytes, data + 20, 4)?));
        return Some(Image { base, pdb });
    }
    None
}

/// Keep `dll` and the PDB beside it under `target/symbols/<pdb>/`. A DLL
/// with no PDB beside it is kept alone; one with no CodeView record is
/// skipped. Returns the folder.
pub fn keep(dll: &Path) -> Result<Option<PathBuf>, String> {
    let bytes = std::fs::read(dll).map_err(|e| format!("{}: {e}", dll.display()))?;
    let Some(image) = image(&bytes) else {
        return Ok(None);
    };
    let to = folder().join(&image.pdb);
    std::fs::create_dir_all(&to).map_err(|e| format!("{}: {e}", to.display()))?;
    std::fs::write(to.join("nanoprobe.dll"), &bytes).map_err(|e| e.to_string())?;
    let pdb = dll.with_extension("pdb");
    if pdb.is_file() {
        std::fs::copy(&pdb, to.join("nanoprobe.pdb")).map_err(|e| e.to_string())?;
    }
    Ok(Some(to))
}

/// The crash records in `path`: a `crash.log`, or a bundle zip that holds
/// `logs/crash.log`.
fn records(path: &Path) -> Result<Vec<Value>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = if bytes.starts_with(b"PK") {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut entry = archive
            .by_name("logs/crash.log")
            .map_err(|_| format!("{} holds no logs/crash.log", path.display()))?;
        let mut text = String::new();
        entry.read_to_string(&mut text).map_err(|e| e.to_string())?;
        text
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// Each location's function and source line. Locations outside the agent,
/// or whose build is not kept, are returned as they are.
fn resolve(pdb: &str, locations: &[&str]) -> Result<Vec<String>, String> {
    let dir = folder().join(pdb);
    let dll = dir.join("nanoprobe.dll");
    let mut out: Vec<String> = locations.iter().map(|l| (*l).to_owned()).collect();
    if !dll.is_file() {
        return Ok(out);
    }
    let base = image(&std::fs::read(&dll).map_err(|e| e.to_string())?)
        .map(|i| i.base)
        .unwrap_or(0x1_8000_0000);
    let mut wanted = Vec::new();
    for (i, location) in locations.iter().enumerate() {
        let Some((module, offset)) = location.split_once("+0x") else {
            continue;
        };
        if !module.starts_with("nanoprobe_agent_") {
            continue;
        }
        if let Ok(offset) = u64::from_str_radix(offset, 16) {
            wanted.push((i, format!("0x{:X}", base + offset)));
        }
    }
    if wanted.is_empty() {
        return Ok(out);
    }
    // stdin is closed: llvm-symbolizer reads addresses from it when it can,
    // and would wait.
    let output = Command::new("llvm-symbolizer")
        .arg("--no-debuginfod")
        .arg(format!("--obj={}", dll.display()))
        .args(wanted.iter().map(|(_, a)| a))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run llvm-symbolizer (run through mise exec): {e}"))?;
    // Paths print relative to the checkout, and the standard library's from
    // `rust/`, so no build machine's folders show.
    let root = format!("{}/", crate::repository().display());
    let text = String::from_utf8_lossy(&output.stdout).replace(&root, "");
    let text = regex::Regex::new(r"\S*/rustlib/src/rust/")
        .expect("a valid pattern")
        .replace_all(&text, "rust/")
        .replace('\\', "/");
    // One block per address, blocks separated by a blank line; inlined
    // frames add a function and line pair each.
    for ((i, _), block) in wanted.iter().zip(text.split("\n\n")) {
        let lines: Vec<&str> = block.lines().filter(|l| !l.is_empty()).collect();
        let pairs: Vec<String> = lines.chunks(2).map(|pair| pair.join(" at ")).collect();
        out[*i] = format!("{}  {}", locations[*i], pairs.join(" <- "));
    }
    Ok(out)
}

/// `symbolize <file>`: print each crash record with its frames resolved.
pub fn symbolize(path: &Path) -> Result<ExitCode, String> {
    let records = records(path)?;
    if records.is_empty() {
        println!("no crash records in {}", path.display());
        return Ok(ExitCode::SUCCESS);
    }
    for record in &records {
        let data = &record["data"];
        let pdb = data["pdb"].as_str().unwrap_or_default();
        println!(
            "{}  code {}  thread {}  in agent {}  depth {}",
            record["wall"].as_str().unwrap_or("?"),
            data["code"].as_str().unwrap_or("?"),
            record["thread"],
            data["in_agent"],
            data["depth"],
        );
        println!(
            "  breadcrumb {} {:?}",
            data["crumb_code"],
            data["crumb"].as_str().unwrap_or("")
        );
        if !folder().join(pdb).join("nanoprobe.dll").is_file() {
            println!("  no kept build for pdb {pdb}; frames are unresolved");
        }
        let at = data["module"].as_str().unwrap_or("?");
        println!("  at {}", resolve(pdb, &[at])?.remove(0));
        let frames: Vec<&str> = data["frames"]
            .as_str()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        for (i, frame) in resolve(pdb, &frames)?.iter().enumerate() {
            println!("  #{i:<2} {frame}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal PE32+ image: headers, one section, and a CodeView record.
    fn sample() -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        let nt = 0x80;
        b[0x3c..0x40].copy_from_slice(&(nt as u32).to_le_bytes());
        b[nt..nt + 4].copy_from_slice(b"PE\0\0");
        b[nt + 6..nt + 8].copy_from_slice(&1u16.to_le_bytes());
        let optional_size = 240u16;
        b[nt + 20..nt + 22].copy_from_slice(&optional_size.to_le_bytes());
        let optional = nt + 24;
        b[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        b[optional + 24..optional + 32].copy_from_slice(&0x1_8000_0000u64.to_le_bytes());
        // Debug directory at RVA 0x1000, one entry.
        let dir = optional + 112 + 48;
        b[dir..dir + 4].copy_from_slice(&0x1000u32.to_le_bytes());
        b[dir + 4..dir + 8].copy_from_slice(&28u32.to_le_bytes());
        // One section: RVA 0x1000, raw at 0x200, 0x200 bytes.
        let s = optional + optional_size as usize;
        b[s + 12..s + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        b[s + 16..s + 20].copy_from_slice(&0x200u32.to_le_bytes());
        b[s + 20..s + 24].copy_from_slice(&0x200u32.to_le_bytes());
        // The debug entry: CodeView, raw data at file offset 0x240.
        let e = 0x200;
        b[e + 12..e + 16].copy_from_slice(&2u32.to_le_bytes());
        b[e + 24..e + 28].copy_from_slice(&0x240u32.to_le_bytes());
        let d = 0x240;
        b[d..d + 4].copy_from_slice(b"RSDS");
        b[d + 4..d + 8].copy_from_slice(&0xC7BE_7C2Au32.to_le_bytes());
        b[d + 8..d + 10].copy_from_slice(&0xF26Au16.to_le_bytes());
        b[d + 10..d + 12].copy_from_slice(&0x9FA8u16.to_le_bytes());
        b[d + 12..d + 20].copy_from_slice(&[0x4C, 0x4C, 0x44, 0x20, 0x50, 0x44, 0x42, 0x2E]);
        b[d + 20..d + 24].copy_from_slice(&1u32.to_le_bytes());
        b
    }

    #[test]
    fn reads_the_codeview_signature_as_the_agent_writes_it() {
        assert_eq!(
            image(&sample()),
            Some(Image {
                base: 0x1_8000_0000,
                pdb: "C7BE7C2AF26A9FA84C4C44205044422E1".into(),
            })
        );
    }

    #[test]
    fn refuses_what_is_not_pe32_plus() {
        assert_eq!(image(b"MZ"), None);
        let mut b = sample();
        b[0x80] = b'X';
        assert_eq!(image(&b), None);
    }
}
