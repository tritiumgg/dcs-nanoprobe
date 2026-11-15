//! The `autoexec.cfg` merge (spec/install.md, "autoexec.cfg").
//!
//! Install appends one block, between marker lines, to
//! `<profile>/Config/autoexec.cfg`. The block adds the names the platform needs
//! to `net.allow_unsafe_api` and `net.allow_dostring_in` when DCS runs the
//! file, and keeps whatever earlier lines put there. Uninstall removes the
//! block and the line break install added before it, and the file and folder
//! when install created them and nothing else is in them. Every other byte of
//! the file stays as it was.
//!
//! The file's bytes are handled only to find and cut the block. Nothing here
//! prints, logs or returns any of them; [`gates`] returns only what concerns
//! the two keys.

use crate::Profile;

/// The file, relative to the profile.
pub const AUTOEXEC: &str = "Config/autoexec.cfg";
/// Its folder.
pub const CONFIG: &str = "Config";

/// The block's first line starts with this.
const BEGIN: &str = "-- nanoprobe: begin";
/// The block's last line.
const END: &str = "-- nanoprobe: end";
/// Begin-line flags: install created the file; install added a line break
/// before the block because the file did not end with one.
const NEW_FILE: &str = " [new file]";
const ADDED_BREAK: &str = " [added line break]";
/// The rest of the begin line.
const BEGIN_TAIL: &str = ". `nanoprobe uninstall` removes this block.";

/// The two keys and the name each must hold for the platform: the hook state
/// (`userhooks`) may call `net.dostring_in`, and may reach the `server` state.
pub const NEEDED: [(&str, &str); 2] = [
    ("net.allow_unsafe_api", "userhooks"),
    ("net.allow_dostring_in", "server"),
];

/// The block's Lua, between the marker lines. It runs in DCS's `config`
/// state, which `autoexec.cfg` runs in, and uses only `type`, `#` and table
/// indexing.
const BODY: &str = "\
do
  local function allow(list, name)
    if type(list) ~= 'table' then list = {} end
    for i = 1, #list do
      if list[i] == name then return list end
    end
    list[#list + 1] = name
    return list
  end
  net.allow_unsafe_api = allow(net.allow_unsafe_api, 'userhooks')
  net.allow_dostring_in = allow(net.allow_dostring_in, 'server')
end
";

/// What [`merge`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Merged {
    /// The block was added.
    Added,
    /// An intact block was already there.
    Present,
}

/// Where the block sits in a file, by byte offsets.
#[derive(Debug, PartialEq, Eq)]
struct Block {
    /// Start of the begin line, or of the added line break before it.
    start: usize,
    /// One past the end line's line break.
    end: usize,
    new_file: bool,
    /// The lines between the markers equal [`BODY`].
    intact: bool,
}

/// The block in `text`, the last one when there are several; None when no
/// begin line has an end line after it.
fn find(text: &[u8]) -> Option<Block> {
    let mut lines = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive(|&b| b == b'\n') {
        lines.push((offset, line));
        offset += line.len();
    }
    let begin = lines
        .iter()
        .rposition(|(_, line)| strip_eol(line).starts_with(BEGIN.as_bytes()))?;
    let end = lines[begin..]
        .iter()
        .position(|(_, line)| strip_eol(line) == END.as_bytes())?
        + begin;
    let (begin_at, begin_line) = lines[begin];
    let flags = strip_eol(begin_line);
    let has = |flag: &str| flags.windows(flag.len()).any(|w| w == flag.as_bytes());
    let added_break = has(ADDED_BREAK);
    // The break install added, which an editor may have turned into CRLF.
    let start = match (added_break, &text[..begin_at]) {
        (true, [.., b'\r', b'\n']) => begin_at - 2,
        (true, [.., b'\n']) => begin_at - 1,
        _ => begin_at,
    };
    let (end_at, end_line) = lines[end];
    // Line by line, so an editor's change of line endings keeps it intact.
    let body = lines[begin + 1..end]
        .iter()
        .map(|(_, line)| strip_eol(line));
    let intact = body.eq(BODY.lines().map(str::as_bytes));
    Some(Block {
        start,
        end: end_at + end_line.len(),
        new_file: has(NEW_FILE),
        intact,
    })
}

fn strip_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// The block install appends, with its flags.
fn block(new_file: bool, added_break: bool) -> String {
    let mut out = String::new();
    if added_break {
        out.push('\n');
    }
    out.push_str(BEGIN);
    if new_file {
        out.push_str(NEW_FILE);
    }
    if added_break {
        out.push_str(ADDED_BREAK);
    }
    out.push_str(BEGIN_TAIL);
    out.push('\n');
    out.push_str(BODY);
    out.push_str(END);
    out.push('\n');
    out
}

/// The block's Lua, between the marker lines.
pub fn body() -> &'static str {
    BODY
}

/// Whether [`merge`] can run: an error names a damaged block (its body
/// edited, or a begin line without an end line), which the user decides
/// about. Ok(true) when an intact block is there.
pub fn check(profile: &dyn Profile) -> Result<bool, String> {
    let old = profile.read(AUTOEXEC)?;
    let text = old.as_deref().unwrap_or_default();
    match find(text) {
        Some(found) if found.intact => Ok(true),
        Some(_) => Err(format!(
            "{AUTOEXEC}: the nanoprobe block was edited; remove the lines from `{BEGIN}` to `{END}` and run install again"
        )),
        None if has_begin(text) => Err(format!(
            "{AUTOEXEC}: a `{BEGIN}` line has no `{END}` line after it; remove it and run install again"
        )),
        None => Ok(false),
    }
}

/// Add the block, unless an intact one is there. A damaged block is refused,
/// as [`check`] says.
pub fn merge(profile: &dyn Profile) -> Result<Merged, String> {
    if check(profile)? {
        return Ok(Merged::Present);
    }
    let old = profile.read(AUTOEXEC)?;
    let text = old.as_deref().unwrap_or_default();
    let added_break = !text.is_empty() && !text.ends_with(b"\n");
    let mut new = text.to_vec();
    // After a lone CR, add a whole CRLF, so that uninstall, which takes a CRLF
    // before the block as the added break, leaves that CR.
    if added_break && text.ends_with(b"\r") {
        new.push(b'\r');
    }
    new.extend_from_slice(block(old.is_none(), added_break).as_bytes());
    profile.write(AUTOEXEC, &new)?;
    Ok(Merged::Added)
}

fn has_begin(text: &[u8]) -> bool {
    text.split(|&b| b == b'\n')
        .any(|line| strip_eol(line).starts_with(BEGIN.as_bytes()))
}

/// Remove the block. Returns whether one was there. The file goes when
/// install created it and nothing else is left in it. A damaged block is
/// left, and named in the error.
pub fn remove(profile: &dyn Profile) -> Result<bool, String> {
    let Some(text) = profile.read(AUTOEXEC)? else {
        return Ok(false);
    };
    let Some(found) = find(&text) else {
        if has_begin(&text) {
            return Err(format!(
                "{AUTOEXEC}: a `{BEGIN}` line has no `{END}` line after it; remove the nanoprobe lines by hand"
            ));
        }
        return Ok(false);
    };
    if !found.intact {
        return Err(format!(
            "{AUTOEXEC}: the nanoprobe block was edited; remove the lines from `{BEGIN}` to `{END}` by hand"
        ));
    }
    let mut rest = text[..found.start].to_vec();
    rest.extend_from_slice(&text[found.end..]);
    if found.new_file && rest.is_empty() {
        profile.remove_file(AUTOEXEC)?;
    } else {
        profile.write(AUTOEXEC, &rest)?;
    }
    Ok(true)
}

/// What `autoexec.cfg` does to the two keys, for `doctor`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Gates {
    /// Whether the file exists.
    pub file: bool,
    /// The block is there and intact.
    pub block: bool,
    /// A begin line without an end line, or an edited block.
    pub damaged: bool,
    /// Assignments to either key after the block (or anywhere, without a
    /// block), as `(line number, key, names)`. `names` is None when the value
    /// is not a plain list of quoted names.
    pub later: Vec<(usize, String, Option<Vec<String>>)>,
}

impl Gates {
    /// The later assignments that take away a name the platform needs: lists
    /// that lack it.
    pub fn overrides(&self) -> Vec<&(usize, String, Option<Vec<String>>)> {
        self.later
            .iter()
            .filter(|(_, key, names)| {
                let needed = NEEDED.iter().find(|(k, _)| k == key).map(|(_, n)| *n);
                match (needed, names) {
                    (Some(name), Some(names)) => !names.iter().any(|n| n == name),
                    _ => false,
                }
            })
            .collect()
    }

    /// The later assignments whose value cannot be read: they may keep the
    /// name the platform needs or not.
    pub fn unreadable(&self) -> Vec<&(usize, String, Option<Vec<String>>)> {
        self.later
            .iter()
            .filter(|(_, _, names)| names.is_none())
            .collect()
    }
}

/// Read what the file does to the two keys. Only lines that assign one of
/// them are looked at, and only their key and value are kept.
pub fn gates(profile: &dyn Profile) -> Result<Gates, String> {
    let Some(text) = profile.read(AUTOEXEC)? else {
        return Ok(Gates::default());
    };
    let found = find(&text);
    let mut gates = Gates {
        file: true,
        block: found.as_ref().is_some_and(|b| b.intact),
        damaged: found.as_ref().is_some_and(|b| !b.intact) || (found.is_none() && has_begin(&text)),
        later: Vec::new(),
    };
    let after = found.map_or(0, |b| b.end);
    let mut lines = Vec::new();
    let mut offset = 0;
    for (number, line) in text.split_inclusive(|&b| b == b'\n').enumerate() {
        let at = offset;
        offset += line.len();
        if at >= after {
            lines.push((
                number + 1,
                String::from_utf8_lossy(strip_eol(line)).into_owned(),
            ));
        }
    }
    let mut i = 0;
    while i < lines.len() {
        let (number, ref line) = lines[i];
        i += 1;
        if let Some((key, _)) = assignment(line) {
            // A list that goes on over later lines: join them up to its `}`.
            // Each line's comment goes first; a name in these lists holds no `--`.
            let mut joined = uncomment(line).to_owned();
            while joined.contains('{') && !joined.contains('}') && i < lines.len() {
                joined.push(' ');
                joined.push_str(uncomment(&lines[i].1));
                i += 1;
            }
            let names = assignment(&joined).and_then(|(_, names)| names);
            gates.later.push((number, key, names));
        } else if let Some(key) = other_form(line) {
            gates.later.push((number, key, None));
        }
    }
    Ok(gates)
}

fn uncomment(line: &str) -> &str {
    line.split("--").next().unwrap_or("")
}

/// A line that may replace either key in a form [`assignment`] does not
/// read: `net = ...`, or `net['allow_...']`. Returns what it names.
fn other_form(line: &str) -> Option<String> {
    let line = line.trim();
    let rest = line.strip_prefix("net")?.trim_start();
    if rest.starts_with('=') && !rest.starts_with("==") {
        return Some("net".into());
    }
    if rest.starts_with('[') {
        return NEEDED
            .iter()
            .find(|(key, _)| rest.contains(&key["net.".len()..]))
            .map(|(key, _)| (*key).to_owned());
    }
    None
}

/// `net.allow_unsafe_api = { 'a', "b" }` as the key and its names. The names
/// are None when the value is something other than such a list.
fn assignment(line: &str) -> Option<(String, Option<Vec<String>>)> {
    let line = line.trim();
    let (key, value) = NEEDED.iter().find_map(|(key, _)| {
        let rest = line.strip_prefix(key)?.trim_start();
        Some((*key, rest.strip_prefix('=')?.trim()))
    })?;
    // A `==` comparison is not an assignment.
    if value.starts_with('=') {
        return None;
    }
    let value = value.split("--").next().unwrap_or("").trim();
    let value = value.strip_suffix(';').unwrap_or(value).trim();
    Some((key.to_owned(), names(value)))
}

fn names(value: &str) -> Option<Vec<String>> {
    // A value that is no table at all holds no name.
    let literal = matches!(value, "nil" | "true" | "false")
        || value.parse::<f64>().is_ok()
        || (value.len() >= 2
            && (value.starts_with('\'') && value.ends_with('\'')
                || value.starts_with('"') && value.ends_with('"')));
    if literal {
        return Some(Vec::new());
    }
    let inner = value.strip_prefix('{')?.strip_suffix('}')?;
    let mut out = Vec::new();
    for item in inner.split([',', ';']) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let quoted = item
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .or_else(|| item.strip_prefix('"').and_then(|s| s.strip_suffix('"')))?;
        if quoted.contains(['\'', '"', '\\']) {
            return None;
        }
        out.push(quoted.to_owned());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_block_with_its_flags() {
        let text = block(true, false);
        let found = find(text.as_bytes()).unwrap();
        assert!(found.new_file && found.intact);
        assert_eq!((found.start, found.end), (0, text.len()));
    }

    #[test]
    fn an_added_line_break_is_part_of_the_block() {
        let mut text = b"options = {}".to_vec();
        text.extend_from_slice(block(false, true).as_bytes());
        let found = find(&text).unwrap();
        assert_eq!(found.start, b"options = {}".len());
    }

    #[test]
    fn line_endings_do_not_damage_the_block() {
        let text = format!("a = 1{}", block(false, true)).replace('\n', "\r\n");
        let found = find(text.as_bytes()).unwrap();
        assert!(found.intact);
        assert_eq!(found.start, "a = 1".len());
        assert_eq!(found.end, text.len());
    }

    #[test]
    fn reads_assignments_to_the_two_keys() {
        assert_eq!(
            assignment("net.allow_dostring_in = { 'server', \"gui\" } -- x"),
            Some((
                "net.allow_dostring_in".into(),
                Some(vec!["server".into(), "gui".into()])
            ))
        );
        assert_eq!(
            assignment("  net.allow_unsafe_api={};"),
            Some(("net.allow_unsafe_api".into(), Some(vec![])))
        );
        assert_eq!(
            assignment("net.allow_unsafe_api = make()"),
            Some(("net.allow_unsafe_api".into(), None))
        );
        assert_eq!(assignment("net.allow_unsafe_api == x"), None);
        assert_eq!(
            assignment("net.allow_dostring_in = nil"),
            Some(("net.allow_dostring_in".into(), Some(vec![])))
        );
        assert_eq!(
            assignment("net.allow_dostring_in = 'server'"),
            Some(("net.allow_dostring_in".into(), Some(vec![])))
        );
        assert_eq!(other_form("net = {}"), Some("net".into()));
        assert_eq!(
            other_form("net['allow_dostring_in'] = {}"),
            Some("net.allow_dostring_in".into())
        );
        assert_eq!(other_form("net.log('x')"), None);
        assert_eq!(assignment("options.graphics = {}"), None);
    }
}
