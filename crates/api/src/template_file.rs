//! The file a `SetCampaignTemplate`, a `PutGroupFromTemplate` or a
//! `PutFromStaticTemplate` names
//! (spec/config.md, `[missions] template_dirs`): resolved, held to the
//! listed folders, and read once. What the call keeps is made from the
//! bytes read; nothing reads the path again.

use std::io::Read;
use std::path::{Component, Path, PathBuf, Prefix};

use nanoprobe_sidecar::shell::TEMPLATE_MAX;

/// The answer for every path that is not a file inside a listed folder. A
/// missing file, a folder, a path outside the folders and an empty list all
/// get it, so the call does not tell a caller what else the sidecar's
/// machine holds. The sidecar's log has the cause.
const NOT_LISTED: &str = "is not a file in a folder that [missions] template_dirs lists";

/// The Windows prefix of `p`: its drive, share or device. `None` on a
/// system whose paths have none.
fn prefix(p: &Path) -> Option<Prefix<'_>> {
    match p.components().next() {
        Some(Component::Prefix(c)) => Some(c.kind()),
        _ => None,
    }
}

/// Whether `named` is on a network share or a device that no listed folder
/// is on. Resolving such a path would connect to a machine, or open a
/// device, that the caller chose, so it is refused before anything is
/// opened. A path on a drive is resolved and compared as any other.
fn elsewhere(dirs: &[PathBuf], named: &Path) -> bool {
    match prefix(named) {
        None | Some(Prefix::Disk(_) | Prefix::VerbatimDisk(_)) => false,
        Some(p) => !dirs.iter().any(|d| prefix(d) == Some(p)),
    }
}

/// Read the template at `path`, a path as the sidecar opens it, when it
/// resolves to a file inside one of `dirs`. The error is the refusal's
/// text.
///
/// Symbolic links and `..` are resolved before the comparison, in the path
/// and in each listed folder, so a link inside a listed folder that points
/// out of it is refused. The file opened is the resolved one. A file that
/// is replaced by a link between the check and the open is not caught:
/// the listed folders are the administrator's, and the API gives no way to
/// write into them.
pub fn read(dirs: &[PathBuf], path: &str) -> Result<Vec<u8>, String> {
    read_for(
        "SetCampaignTemplate",
        "a template",
        TEMPLATE_MAX,
        dirs,
        path,
    )
}

/// `read` for the call `call`, which the sidecar's log names with the cause
/// of a refusal, of a file of at most `max` bytes. `what` names the file in
/// the refusal of a larger one.
pub fn read_for(
    call: &str,
    what: &str,
    max: usize,
    dirs: &[PathBuf],
    path: &str,
) -> Result<Vec<u8>, String> {
    let named = Path::new(path);
    if !named.is_absolute() {
        return Err(format!(
            "{path:?} is not an absolute path as the sidecar's system writes one"
        ));
    }
    let refuse = |why: &str| {
        eprintln!("{call} refused {path:?}: {why}");
        format!("{path:?} {NOT_LISTED}")
    };
    if elsewhere(dirs, named) {
        return Err(refuse(
            "it is on a share or a device no listed folder is on",
        ));
    }
    let resolved = std::fs::canonicalize(named).map_err(|e| refuse(&e.to_string()))?;
    let listed = dirs
        .iter()
        .filter_map(|d| std::fs::canonicalize(d).ok())
        .any(|d| resolved != d && resolved.starts_with(&d));
    if !listed {
        return Err(refuse("it resolves outside the listed folders"));
    }
    // Checked before the open: opening a FIFO would wait for a writer.
    let is_file = |m: std::io::Result<std::fs::Metadata>| m.is_ok_and(|m| m.is_file());
    if !is_file(std::fs::metadata(&resolved)) {
        return Err(refuse("it is not a regular file"));
    }
    let unreadable = |e: std::io::Error| format!("{path:?} could not be read: {}", e.kind());
    let file = std::fs::File::open(&resolved).map_err(unreadable)?;
    let size = file.metadata().map_err(unreadable)?.len();
    let too_large = || format!("{what} is at most {max} bytes");
    if size > max as u64 {
        return Err(too_large());
    }
    // The limit holds for a file that grows while it is read.
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() > max {
        return Err(too_large());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(dirs: &[PathBuf], path: &Path) -> String {
        read(dirs, path.to_str().expect("utf-8")).expect_err("refused")
    }

    #[test]
    fn a_file_in_a_listed_folder_is_read() {
        let d = tempfile::tempdir().expect("dir");
        let listed = d.path().join("templates");
        std::fs::create_dir_all(listed.join("sub")).expect("dirs");
        std::fs::write(listed.join("a.miz"), b"a").expect("a");
        std::fs::write(listed.join("sub/b.miz"), b"b").expect("b");
        let dirs = [d.path().join("other"), listed.clone()];
        let read_at = |p: PathBuf| read(&dirs, p.to_str().expect("utf-8"));
        assert_eq!(read_at(listed.join("a.miz")).as_deref(), Ok(&b"a"[..]));
        assert_eq!(read_at(listed.join("sub/b.miz")).as_deref(), Ok(&b"b"[..]));
        // `..` that stays inside.
        assert_eq!(
            read_at(listed.join("sub/../a.miz")).as_deref(),
            Ok(&b"a"[..])
        );
        // An empty file is a file: the caller's checks refuse it, and it is
        // not the empty path that removes a template.
        std::fs::write(listed.join("empty.miz"), b"").expect("empty");
        assert_eq!(read_at(listed.join("empty.miz")).as_deref(), Ok(&b""[..]));
    }

    #[test]
    fn a_path_outside_the_listed_folders_is_refused() {
        let d = tempfile::tempdir().expect("dir");
        let listed = d.path().join("templates");
        let other = d.path().join("other");
        std::fs::create_dir_all(listed.join("sub")).expect("listed");
        std::fs::create_dir_all(&other).expect("other");
        std::fs::write(listed.join("a.miz"), b"a").expect("a");
        std::fs::write(other.join("secret.miz"), b"s").expect("secret");
        // A folder whose name starts with a listed folder's name.
        let sibling = d.path().join("templates-old");
        std::fs::create_dir_all(&sibling).expect("sibling");
        std::fs::write(sibling.join("a.miz"), b"a").expect("a");
        let dirs = [listed.clone()];
        let outside = [
            other.join("secret.miz"),
            listed.join("../other/secret.miz"),
            sibling.join("a.miz"),
            // Missing, a folder outside, the listed folder itself, and a
            // folder in it, which only the regular-file check refuses.
            listed.join("missing.miz"),
            other.clone(),
            listed.clone(),
            listed.join("sub"),
        ];
        let texts: Vec<String> = outside.iter().map(|p| refused(&dirs, p)).collect();
        for (p, t) in outside.iter().zip(&texts) {
            assert!(t.ends_with(NOT_LISTED), "{}: {t}", p.display());
        }
        // An empty list refuses every path, and so does a listed folder
        // that does not exist.
        assert!(refused(&[], &listed.join("a.miz")).ends_with(NOT_LISTED));
        assert!(refused(&[d.path().join("gone")], &listed.join("a.miz")).ends_with(NOT_LISTED));
        // A relative path is not taken from any folder.
        let e = read(&dirs, "a.miz").expect_err("relative");
        assert!(e.contains("not an absolute path"), "{e}");
        let e = read(&dirs, "").expect_err("empty");
        assert!(e.contains("not an absolute path"), "{e}");
    }

    /// A native sidecar beside a DCS under Wine opens its own system's
    /// paths. The path DCS has for the same file is no path there.
    #[cfg(unix)]
    #[test]
    fn a_path_as_dcs_under_wine_writes_it_is_refused() {
        let d = tempfile::tempdir().expect("dir");
        std::fs::write(d.path().join("a.miz"), b"a").expect("a");
        let dirs = [d.path().to_path_buf()];
        let unix = d.path().join("a.miz");
        let unix = unix.to_str().expect("utf-8");
        assert_eq!(read(&dirs, unix).as_deref(), Ok(&b"a"[..]));
        for wine in [
            format!("Z:{}", unix.replace('/', "\\")),
            format!("Z:{unix}"),
            "C:\\users\\dcs\\Saved Games\\DCS\\a.miz".to_owned(),
        ] {
            let e = read(&dirs, &wine).expect_err("a Windows path");
            assert!(e.contains("not an absolute path"), "{wine}: {e}");
        }
    }

    /// A sidecar on Windows opens Windows paths, with a drive.
    #[cfg(windows)]
    #[test]
    fn a_windows_sidecar_takes_a_path_with_a_drive() {
        let d = tempfile::tempdir().expect("dir");
        std::fs::write(d.path().join("a.miz"), b"a").expect("a");
        let dirs = [d.path().to_path_buf()];
        let file = d.path().join("a.miz");
        assert_eq!(
            read(&dirs, file.to_str().expect("utf-8")).as_deref(),
            Ok(&b"a"[..])
        );
        let e = read(&dirs, "/missions/a.miz").expect_err("no drive");
        assert!(e.contains("not an absolute path"), "{e}");
        // A share or a device the caller names is refused unopened: the
        // host does not exist, and resolving it would take a network
        // timeout, not the refusal's text.
        for foreign in [
            r"\\nanoprobe-test.invalid\share\a.miz",
            r"\\?\UNC\nanoprobe-test.invalid\share\a.miz",
            r"\\.\pipe\nanoprobe-test",
        ] {
            assert!(elsewhere(&dirs, Path::new(foreign)), "{foreign}");
            let e = read(&dirs, foreign).expect_err("a share or a device");
            assert!(e.ends_with(NOT_LISTED), "{foreign}: {e}");
        }
        // A listed folder on a share allows that share, and no other.
        let share = [PathBuf::from(r"\\nanoprobe-test.invalid\share\templates")];
        assert!(!elsewhere(
            &share,
            Path::new(r"\\nanoprobe-test.invalid\share\templates\a.miz")
        ));
        assert!(elsewhere(
            &share,
            Path::new(r"\\nanoprobe-test.invalid\other\a.miz")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn links_are_resolved_before_the_folders_are_compared() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().expect("dir");
        let listed = d.path().join("templates");
        let other = d.path().join("other");
        std::fs::create_dir_all(&listed).expect("listed");
        std::fs::create_dir_all(&other).expect("other");
        std::fs::write(other.join("secret.miz"), b"s").expect("secret");
        std::fs::write(listed.join("a.miz"), b"a").expect("a");
        // A link in the listed folder to a file and to a folder outside it.
        symlink(other.join("secret.miz"), listed.join("link.miz")).expect("link");
        symlink(&other, listed.join("out")).expect("link");
        let dirs = [listed.clone()];
        assert!(refused(&dirs, &listed.join("link.miz")).ends_with(NOT_LISTED));
        assert!(refused(&dirs, &listed.join("out/secret.miz")).ends_with(NOT_LISTED));
        // A link from outside into the listed folder resolves inside it.
        symlink(listed.join("a.miz"), other.join("in.miz")).expect("link");
        assert_eq!(
            read(&dirs, other.join("in.miz").to_str().expect("utf-8")).as_deref(),
            Ok(&b"a"[..])
        );
        // A listed folder that is itself a link is the folder it points to.
        symlink(&listed, d.path().join("alias")).expect("link");
        assert_eq!(
            read(
                &[d.path().join("alias")],
                listed.join("a.miz").to_str().expect("utf-8")
            )
            .as_deref(),
            Ok(&b"a"[..])
        );
    }

    /// A FIFO in a listed folder is refused before it is opened: an open
    /// for reading waits for a writer. The read runs on a thread of its
    /// own, so a read that does open the FIFO fails the test and does not
    /// hang it.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_being_opened() {
        let d = tempfile::tempdir().expect("dir");
        let fifo = d.path().join("pipe.miz");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "mkfifo");
        let (tx, rx) = std::sync::mpsc::channel();
        let (dirs, path) = ([d.path().to_path_buf()], fifo.clone());
        let reader = std::thread::spawn(move || {
            let _ = tx.send(read(&dirs, path.to_str().expect("utf-8")));
        });
        // Far longer than a refusal takes; only a blocked open waits it out.
        let got = rx.recv_timeout(std::time::Duration::from_secs(10));
        if got.is_err() {
            // Release the blocked open: a writer lets it return.
            drop(std::fs::OpenOptions::new().write(true).open(&fifo));
        }
        reader.join().expect("join");
        let e = got
            .expect("the read opened the FIFO and waited for a writer")
            .expect_err("refused");
        assert!(e.ends_with(NOT_LISTED), "{e}");
    }

    /// A file in a listed folder that the sidecar may not open.
    #[cfg(unix)]
    #[test]
    fn a_file_that_cannot_be_opened_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().expect("dir");
        let locked = d.path().join("locked.miz");
        std::fs::write(&locked, b"a").expect("write");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("mode");
        // A process that opens it anyway, as root does, has no such refusal.
        if std::fs::File::open(&locked).is_ok() {
            return;
        }
        let e = refused(&[d.path().to_path_buf()], &locked);
        assert!(e.contains("could not be read"), "{e}");
    }

    #[test]
    fn a_file_over_the_limit_is_refused() {
        let d = tempfile::tempdir().expect("dir");
        let big = d.path().join("big.miz");
        // A sparse file: the size is checked, and no 16 MiB is written.
        let f = std::fs::File::create(&big).expect("create");
        f.set_len(TEMPLATE_MAX as u64 + 1).expect("len");
        drop(f);
        let e = refused(&[d.path().to_path_buf()], &big);
        assert!(e.contains("at most"), "{e}");
        let at = d.path().join("at.miz");
        let f = std::fs::File::create(&at).expect("create");
        f.set_len(TEMPLATE_MAX as u64).expect("len");
        drop(f);
        let got = read(&[d.path().to_path_buf()], at.to_str().expect("utf-8")).expect("at limit");
        assert_eq!(got.len(), TEMPLATE_MAX);
    }

    #[test]
    fn a_call_gives_its_own_limit_and_its_own_name_for_the_file() {
        let d = tempfile::tempdir().expect("dir");
        let dirs = [d.path().to_path_buf()];
        let at = d.path().join("at.lua");
        std::fs::write(&at, [b'a'; 16]).expect("at");
        let over = d.path().join("over.lua");
        std::fs::write(&over, [b'a'; 17]).expect("over");
        let path = |p: &Path| p.to_str().expect("utf-8").to_owned();
        assert_eq!(
            read_for("X", "a thing", 16, &dirs, &path(&at)).as_deref(),
            Ok(&[b'a'; 16][..])
        );
        assert_eq!(
            read_for("X", "a thing", 16, &dirs, &path(&over)),
            Err("a thing is at most 16 bytes".to_owned())
        );
        // The same file under the template's own limit is read.
        assert_eq!(read(&dirs, &path(&over)).map(|b| b.len()), Ok(17));
    }
}
