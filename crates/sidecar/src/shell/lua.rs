//! A reader of the Lua the Mission Editor writes into a `.miz` member: one
//! assignment `name = <table>` of literal values (ED-CODE: `mission` and
//! `warehouses` as the editor saves them). It keeps the byte range of every
//! value, so a writer replaces a few values and leaves every other byte as
//! it was.
//!
//! It reads table constructors with `[key] = v`, `name = v` and positional
//! fields, double- and single-quoted strings, long brackets, numbers,
//! `true`, `false`, `nil` and `--` comments. Anything else is an error, never
//! a guess.

use std::ops::Range;

use nanoprobe_transport::codec::Lv;

/// A table key.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    /// A string key: `["x"]` or `x`.
    Str(Vec<u8>),
    /// A number key: `[3]`, or the position of a positional field.
    Num(f64),
}

impl Key {
    /// A string key from text.
    pub fn s(k: &str) -> Key {
        Key::Str(k.as_bytes().to_vec())
    }
}

/// A value and where it is in the source.
#[derive(Clone, Debug)]
pub struct Node {
    /// The value's bytes, `{` to `}` for a table.
    pub span: Range<usize>,
    /// The fields of a table, in source order; empty for a scalar.
    pub fields: Vec<(Key, Node)>,
    /// Whether it is a table.
    pub table: bool,
    /// A bare name that `parse_returned` takes as a preset's task (see
    /// there). It has no value: it never reads as a string, a number or a
    /// table.
    pub opaque: bool,
}

impl Node {
    /// The field under `key`, when this is a table that has it.
    pub fn get(&self, key: &Key) -> Option<&Node> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// The node at `path` below this one.
    pub fn at(&self, path: &[Key]) -> Option<&Node> {
        path.iter().try_fold(self, |n, k| n.get(k))
    }
}

/// A parsed member: `name = <value>`.
#[derive(Debug)]
pub struct Doc {
    /// The assigned name.
    pub name: String,
    /// The value.
    pub root: Node,
}

/// Parse `src`.
pub fn parse(src: &[u8]) -> Result<Doc, String> {
    let mut p = P {
        s: src,
        i: 0,
        tasks: None,
    };
    p.ws()?;
    let name = p.ident().ok_or_else(|| p.err("an assignment"))?;
    p.ws()?;
    p.expect(b'=')?;
    p.ws()?;
    let root = p.value(0)?;
    p.ws()?;
    if p.i != src.len() {
        return Err(p.err("the end after the value"));
    }
    Ok(Doc {
        name: String::from_utf8_lossy(name).into_owned(),
        root,
    })
}

/// The words Lua 5.1 reserves; none of them names a local.
const RESERVED: [&[u8]; 21] = [
    b"and",
    b"break",
    b"do",
    b"else",
    b"elseif",
    b"end",
    b"false",
    b"for",
    b"function",
    b"if",
    b"in",
    b"local",
    b"nil",
    b"not",
    b"or",
    b"repeat",
    b"return",
    b"then",
    b"true",
    b"until",
    b"while",
];

/// Parse `src` as the Mission Editor's payload preset files are written
/// (ED-CODE, docs/results/2026-11-24-payload-presets.md): exactly
/// `local <name> = <value>` then `return <name>`, the same name, with only
/// white space and comments around them. The value is read as `parse`
/// reads one, with one addition: a value directly inside a preset's
/// `tasks` table, `payloads[*].tasks[*]` from the root, may be a single
/// name followed by `,` or `}`, such as `[1] = CAS`. The editor runs the
/// file in its global environment (ED-CODE), where what the name holds is
/// UNVERIFIED; here the node is opaque
/// ([`Node::opaque`]). A bare name anywhere else is refused. The error
/// names the line of the first text that is not this form.
pub fn parse_returned(src: &[u8]) -> Result<Doc, String> {
    let mut p = P {
        s: src,
        i: 0,
        tasks: Some(Vec::new()),
    };
    p.ws()?;
    if p.ident() != Some(b"local".as_slice()) {
        p.i = 0;
        p.ws()?;
        return Err(p.err("`local <name> = {`"));
    }
    p.ws()?;
    let at = p.i;
    let name = p.ident().filter(|n| !RESERVED.contains(n)).ok_or_else(|| {
        p.i = at;
        p.err("a name after `local`")
    })?;
    p.ws()?;
    p.expect(b'=')?;
    p.ws()?;
    let root = p.value(0)?;
    p.ws()?;
    let at = p.i;
    let want = format!("`return {}`", String::from_utf8_lossy(name));
    if p.ident() != Some(b"return".as_slice()) {
        p.i = at;
        return Err(p.err(&want));
    }
    p.ws()?;
    if p.ident() != Some(name) {
        p.i = at;
        return Err(p.err(&want));
    }
    p.ws()?;
    if p.i != src.len() {
        return Err(p.err("the end after the `return`"));
    }
    Ok(Doc {
        name: String::from_utf8_lossy(name).into_owned(),
        root,
    })
}

/// Deepest table nesting read.
const DEPTH_MAX: usize = 200;

struct P<'a> {
    s: &'a [u8],
    i: usize,
    /// For `parse_returned`, the place of the value being read: one mark
    /// per table key from the root. `None` for `parse`, which takes no
    /// bare name.
    tasks: Option<Vec<Mark>>,
}

/// What a key on the way to a value is, for the one place a bare name is
/// taken.
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    Payloads,
    Tasks,
    Other,
}

fn mark(k: &Key) -> Mark {
    match k {
        Key::Str(s) if s == b"payloads" => Mark::Payloads,
        Key::Str(s) if s == b"tasks" => Mark::Tasks,
        _ => Mark::Other,
    }
}

impl<'a> P<'a> {
    fn err(&self, want: &str) -> String {
        let line = 1 + self.s[..self.i.min(self.s.len())]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        format!("line {line}: expected {want}")
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn expect(&mut self, b: u8) -> Result<(), String> {
        if self.peek() == Some(b) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(&format!("`{}`", b as char)))
        }
    }

    /// Skip white space and comments.
    fn ws(&mut self) -> Result<(), String> {
        loop {
            while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"--") {
                self.i += 2;
                if let Some(level) = self.long_open() {
                    self.long_body(level)?;
                } else {
                    while self.peek().is_some_and(|b| b != b'\n') {
                        self.i += 1;
                    }
                }
                continue;
            }
            return Ok(());
        }
    }

    /// At `[` followed by `=`* and `[`: consume it and return the level.
    fn long_open(&mut self) -> Option<usize> {
        let rest = &self.s[self.i..];
        if rest.first() != Some(&b'[') {
            return None;
        }
        let level = rest[1..].iter().take_while(|&&b| b == b'=').count();
        (rest.get(1 + level) == Some(&b'[')).then(|| {
            self.i += level + 2;
            level
        })
    }

    /// The body of a long bracket up to its close, which is consumed.
    fn long_body(&mut self, level: usize) -> Result<Vec<u8>, String> {
        let mut close = vec![b']'];
        close.extend(std::iter::repeat_n(b'=', level));
        close.push(b']');
        let start = self.i;
        let at = self.s[start..]
            .windows(close.len())
            .position(|w| w == close.as_slice())
            .ok_or_else(|| self.err("the close of a long bracket"))?;
        self.i = start + at + close.len();
        let mut body = &self.s[start..start + at];
        // A first newline is skipped (Lua 5.1 manual, 2.1).
        if body.starts_with(b"\r\n") {
            body = &body[2..];
        } else if body.starts_with(b"\n") {
            body = &body[1..];
        }
        Ok(body.to_vec())
    }

    /// Whether the value being read sits directly in a preset's `tasks`:
    /// `payloads`, a preset, `tasks`, an entry.
    fn in_tasks(&self) -> bool {
        matches!(
            self.tasks.as_deref(),
            Some([Mark::Payloads, _, Mark::Tasks, _])
        )
    }

    /// At a name: whether it is a task name `parse_returned` takes, a name
    /// in a preset's `tasks` followed by `,` or `}`. Consumes nothing.
    fn task_name_here(&mut self) -> bool {
        if !self.in_tasks() {
            return false;
        }
        let start = self.i;
        let ok = match self.ident() {
            Some(b"true" | b"false" | b"nil") | None => false,
            Some(id) if RESERVED.contains(&id) => false,
            Some(_) => self.ws().is_ok() && matches!(self.peek(), Some(b',' | b'}')),
        };
        self.i = start;
        ok
    }

    fn ident(&mut self) -> Option<&'a [u8]> {
        let start = self.i;
        if !self
            .peek()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        {
            return None;
        }
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            self.i += 1;
        }
        Some(&self.s[start..self.i])
    }

    fn value(&mut self, depth: usize) -> Result<Node, String> {
        let start = self.i;
        match self.peek() {
            Some(b'{') => self.table(depth),
            Some(b'"' | b'\'') => {
                self.string()?;
                Ok(scalar(start, self.i))
            }
            Some(b'[') => {
                let level = self.long_open().ok_or_else(|| self.err("a value"))?;
                self.long_body(level)?;
                Ok(scalar(start, self.i))
            }
            Some(b) if b == b'-' || b == b'.' || b.is_ascii_digit() => {
                self.number()?;
                Ok(scalar(start, self.i))
            }
            _ if self.task_name_here() => {
                self.ident();
                Ok(Node {
                    opaque: true,
                    ..scalar(start, self.i)
                })
            }
            _ => match self.ident() {
                Some(b"true" | b"false" | b"nil") => Ok(scalar(start, self.i)),
                _ => {
                    self.i = start;
                    Err(self.err("a value"))
                }
            },
        }
    }

    fn table(&mut self, depth: usize) -> Result<Node, String> {
        if depth >= DEPTH_MAX {
            return Err(self.err("a table less deeply nested"));
        }
        let start = self.i;
        self.expect(b'{')?;
        let mut fields = Vec::new();
        let mut next = 1.0;
        loop {
            self.ws()?;
            if self.peek() == Some(b'}') {
                self.i += 1;
                break;
            }
            let key = if self.peek() == Some(b'[') && self.long_level_at().is_none() {
                self.i += 1;
                self.ws()?;
                let k = self.key()?;
                self.ws()?;
                self.expect(b']')?;
                self.ws()?;
                self.expect(b'=')?;
                self.ws()?;
                k
            } else {
                let save = self.i;
                // A task name as a positional field: the place is the
                // field's, which the value check below needs.
                if let Some(st) = self.tasks.as_mut() {
                    st.push(Mark::Other);
                }
                let positional_task = self.task_name_here();
                if let Some(st) = self.tasks.as_mut() {
                    st.pop();
                }
                match self.ident() {
                    Some(_) if positional_task => {
                        self.i = save;
                        let k = Key::Num(next);
                        next += 1.0;
                        k
                    }
                    Some(id) if !matches!(id, b"true" | b"false" | b"nil") => {
                        let id = id.to_vec();
                        self.ws()?;
                        if self.peek() == Some(b'=') {
                            self.i += 1;
                            self.ws()?;
                            Key::Str(id)
                        } else {
                            return Err(self.err("`=` after a name"));
                        }
                    }
                    _ => {
                        self.i = save;
                        let k = Key::Num(next);
                        next += 1.0;
                        k
                    }
                }
            };
            if let Some(st) = self.tasks.as_mut() {
                st.push(mark(&key));
            }
            let v = self.value(depth + 1);
            if let Some(st) = self.tasks.as_mut() {
                st.pop();
            }
            fields.push((key, v?));
            self.ws()?;
            match self.peek() {
                Some(b',' | b';') => self.i += 1,
                Some(b'}') => {}
                _ => return Err(self.err("`,` or `}`")),
            }
        }
        // A pushed-to list keeps room for more: four fields' worth for one
        // field. Many small tables would hold four times what they use.
        fields.shrink_to_fit();
        Ok(Node {
            span: start..self.i,
            fields,
            table: true,
            opaque: false,
        })
    }

    /// The level of a long bracket opening here, without consuming it.
    fn long_level_at(&self) -> Option<usize> {
        let rest = &self.s[self.i..];
        let level = rest.get(1..)?.iter().take_while(|&&b| b == b'=').count();
        (rest.first() == Some(&b'[') && rest.get(1 + level) == Some(&b'[')).then_some(level)
    }

    fn key(&mut self) -> Result<Key, String> {
        match self.peek() {
            Some(b'"' | b'\'') => Ok(Key::Str(self.string()?)),
            _ => {
                let start = self.i;
                self.number()?;
                let text = std::str::from_utf8(&self.s[start..self.i]).expect("ASCII");
                Ok(Key::Num(
                    number_of(text).ok_or_else(|| self.err("a number"))?,
                ))
            }
        }
    }

    fn number(&mut self) -> Result<(), String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'.')
            || (self.i > start
                && matches!(self.s[self.i - 1], b'e' | b'E')
                && !self.s[start..self.i].starts_with(b"0x")
                && matches!(self.peek(), Some(b'+' | b'-')))
        {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).map_err(|_| self.err("a number"))?;
        if number_of(text).is_none() {
            self.i = start;
            return Err(self.err("a number"));
        }
        Ok(())
    }

    /// A quoted string; returns its bytes.
    fn string(&mut self) -> Result<Vec<u8>, String> {
        let q = self.peek().expect("at a quote");
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(b) = self.peek() else {
                return Err(self.err("the end of a string"));
            };
            self.i += 1;
            match b {
                b'\n' => return Err(self.err("the end of a string before the line ends")),
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return Err(self.err("an escape"));
                    };
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'a' => out.push(7),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'v' => out.push(11),
                        b'\n' => out.push(b'\n'),
                        b'0'..=b'9' => {
                            let mut n = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'9') => {
                                        n = n * 10 + u32::from(d - b'0');
                                        self.i += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(u8::try_from(n).map_err(|_| self.err("an escape below 256"))?);
                        }
                        other => out.push(other),
                    }
                }
                _ if b == q => return Ok(out),
                _ => out.push(b),
            }
        }
    }
}

fn scalar(start: usize, end: usize) -> Node {
    Node {
        span: start..end,
        fields: Vec::new(),
        table: false,
        opaque: false,
    }
}

/// The value of a Lua numeral, with an optional leading minus.
fn number_of(text: &str) -> Option<f64> {
    let (neg, t) = match text.strip_prefix('-') {
        Some(t) => (true, t),
        None => (false, text),
    };
    let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()? as f64
    } else {
        if t.is_empty()
            || !t
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_digit() || b == b'.')
        {
            return None;
        }
        t.parse::<f64>().ok()?
    };
    Some(if neg { -v } else { v })
}

/// Read a scalar node's value.
pub fn scalar_value(src: &[u8], node: &Node) -> Option<Lv> {
    if node.table || node.opaque {
        return None;
    }
    let text = &src[node.span.clone()];
    match text {
        b"true" => Some(Lv::Bool(true)),
        b"false" => Some(Lv::Bool(false)),
        b"nil" => Some(Lv::Nil),
        _ if text
            .first()
            .is_some_and(|b| matches!(b, b'"' | b'\'' | b'[')) =>
        {
            let mut p = P {
                s: src,
                i: node.span.start,
                tasks: None,
            };
            if matches!(text[0], b'"' | b'\'') {
                p.string().ok().map(Lv::String)
            } else {
                let level = p.long_open()?;
                p.long_body(level).ok().map(Lv::String)
            }
        }
        _ => number_of(std::str::from_utf8(text).ok()?).map(Lv::Number),
    }
}

/// One change to a source: replace `span` with `text`. An empty span
/// inserts.
#[derive(Clone, Debug)]
pub struct Edit {
    /// What to replace.
    pub span: Range<usize>,
    /// The new bytes.
    pub text: Vec<u8>,
}

/// Apply edits that do not overlap.
pub fn apply(src: &[u8], mut edits: Vec<Edit>) -> Result<Vec<u8>, String> {
    edits.sort_by_key(|e| (e.span.start, e.span.end));
    let mut out = Vec::with_capacity(src.len());
    let mut at = 0;
    for e in &edits {
        if e.span.start < at {
            return Err("two edits overlap".into());
        }
        out.extend_from_slice(&src[at..e.span.start]);
        out.extend_from_slice(&e.text);
        at = e.span.end;
    }
    out.extend_from_slice(&src[at..]);
    Ok(out)
}

/// A value as Lua source in the Mission Editor's layout: `["k"] = v,` one
/// per line, tabs indented to `indent`. A number that is not finite and a
/// value Lua cannot hold are errors.
pub fn write(v: &Lv, indent: usize) -> Result<String, String> {
    let mut out = String::new();
    write_into(&mut out, v, indent)?;
    Ok(out)
}

fn write_into(out: &mut String, v: &Lv, indent: usize) -> Result<(), String> {
    match v {
        Lv::Nil => out.push_str("nil"),
        Lv::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Lv::Number(n) => out.push_str(&number(*n)?),
        Lv::String(s) => quote(out, s),
        Lv::Table(pairs) => {
            if pairs.is_empty() {
                out.push_str("{}");
                return Ok(());
            }
            let tabs = "\t".repeat(indent + 1);
            out.push('\n');
            out.push_str(&"\t".repeat(indent));
            out.push_str("{\n");
            for (k, v) in pairs {
                out.push_str(&tabs);
                out.push('[');
                match k {
                    Lv::Number(n) => out.push_str(&number(*n)?),
                    Lv::String(s) => quote(out, s),
                    other => return Err(format!("a table key Lua cannot hold: {other:?}")),
                }
                out.push_str("] = ");
                write_into(out, v, indent + 1)?;
                out.push_str(",\n");
            }
            out.push_str(&"\t".repeat(indent));
            out.push('}');
        }
        Lv::Other(t) => return Err(format!("a value Lua cannot hold: {t}")),
    }
    Ok(())
}

fn number(n: f64) -> Result<String, String> {
    if !n.is_finite() {
        return Err(format!("a number that is not finite: {n}"));
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        Ok(format!("{}", n as i64))
    } else {
        // Shortest text that reads back as the same double.
        Ok(format!("{n:?}"))
    }
}

/// A double-quoted Lua string holding exactly `s`.
pub fn quote(out: &mut String, s: &[u8]) {
    out.push('"');
    for &b in s {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            0x20..=0x7e => out.push(b as char),
            // Every other byte, UTF-8 included, as a three-digit escape, so
            // the text stays ASCII and a following digit cannot join it.
            _ => out.push_str(&format!("\\{b:03}")),
        }
    }
    out.push('"');
}

/// Convert a parsed node to a value.
pub fn value(src: &[u8], node: &Node) -> Option<Lv> {
    if !node.table {
        return scalar_value(src, node);
    }
    let pairs = node
        .fields
        .iter()
        .map(|(k, v)| {
            let k = match k {
                Key::Str(s) => Lv::String(s.clone()),
                Key::Num(n) => Lv::Number(*n),
            };
            Some((k, value(src, v)?))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Lv::Table(pairs))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "mission = \n{\n\t[\"date\"] = \n\t{\n\t\t[\"Day\"] = 1,\n\t\t[\"Year\"] = 2016,\n\t}, -- end of [\"date\"]\n\t[\"start_time\"] = 28800,\n\t[\"s\"] = \"a\\\"b\\n\\200\",\n\t[3] = -1.5e-3,\n\tname = true,\n\t{ 1, 'x'; nil },\n\t[\"long\"] = [==[\nhi]]x]==],\n} -- end of mission\n";

    #[test]
    fn reads_the_editor_layout_with_spans() {
        let d = parse(SRC.as_bytes()).unwrap();
        assert_eq!(d.name, "mission");
        let b = SRC.as_bytes();
        let st = d.root.get(&Key::s("start_time")).unwrap();
        assert_eq!(&SRC[st.span.clone()], "28800");
        let day = d.root.at(&[Key::s("date"), Key::s("Day")]).unwrap();
        assert_eq!(scalar_value(b, day), Some(Lv::Number(1.0)));
        let s = d.root.get(&Key::s("s")).unwrap();
        assert_eq!(scalar_value(b, s), Some(Lv::String(b"a\"b\n\xc8".to_vec())));
        assert_eq!(
            scalar_value(b, d.root.get(&Key::Num(3.0)).unwrap()),
            Some(Lv::Number(-1.5e-3))
        );
        assert_eq!(
            scalar_value(b, d.root.get(&Key::s("name")).unwrap()),
            Some(Lv::Bool(true))
        );
        let pos = d.root.get(&Key::Num(1.0)).unwrap();
        assert_eq!(pos.fields.len(), 3);
        assert_eq!(
            scalar_value(b, d.root.get(&Key::s("long")).unwrap()),
            Some(Lv::String(b"hi]]x".to_vec()))
        );
    }

    #[test]
    fn refuses_what_it_does_not_read() {
        for bad in [
            "x = { a }",
            "x = { [1] = f() }",
            "x = {",
            "x = { 1 } y",
            "x = \"open",
            "x = { [\"a\"] 1 }",
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad}");
        }
        let deep = format!("x = {}{}", "{".repeat(300), "}".repeat(300));
        assert!(parse(deep.as_bytes()).is_err());
    }

    #[test]
    fn parse_keeps_refusing_local_and_return() {
        assert!(parse(b"local x = {}\nreturn x\n").is_err());
        assert!(parse(b"x = {}\nreturn x\n").is_err());
    }

    #[test]
    fn reads_a_returned_local() {
        let src = "-- a comment\nlocal unitPayloads =\n{\n\t[\"name\"] = \"F-15C\",\n\t[\"payloads\"] = { [1] = { [\"num\"] = 2 } },\n}\nreturn unitPayloads\n";
        let d = parse_returned(src.as_bytes()).unwrap();
        assert_eq!(d.name, "unitPayloads");
        let b = src.as_bytes();
        assert_eq!(
            scalar_value(b, d.root.get(&Key::s("name")).unwrap()),
            Some(Lv::String(b"F-15C".to_vec()))
        );
        assert!(parse_returned(b"local x={1} --[[ c ]] return x -- end").is_ok());
    }

    /// A preset file with `body` as the first preset's `tasks` table.
    fn with_tasks(body: &str) -> String {
        format!(
            "local p = {{\n\t[\"payloads\"] = {{\n\t\t[1] = {{\n\t\t\t[\"name\"] = \"a\",\n\t\t\t[\"tasks\"] = {body},\n\t\t}},\n\t}},\n}}\nreturn p\n"
        )
    }

    #[test]
    fn takes_a_bare_name_only_as_a_presets_task() {
        for ok in [
            "{ [1] = CAS, [2] = GroundAttack }",
            "{ [1] = CAS }",
            "{ CAS, Intercept }",
            "{ [1] = CAS -- c\n, }",
            "{ [1] = 31, [2] = CAP }",
        ] {
            let src = with_tasks(ok);
            let d = parse_returned(src.as_bytes()).unwrap_or_else(|e| panic!("{ok}: {e}"));
            let tasks = d
                .root
                .at(&[Key::s("payloads"), Key::Num(1.0), Key::s("tasks")])
                .unwrap();
            let first = &tasks.fields[0].1;
            let b = src.as_bytes();
            // Never a string, a number or a table, and its table has no
            // value either.
            if &b[first.span.clone()] != b"31" {
                assert!(first.opaque && !first.table, "{ok}");
                assert_eq!(scalar_value(b, first), None);
                assert_eq!(value(b, tasks), None);
            }
        }
        for bad in [
            "{ [1] = CAS.x }",
            "{ [1] = CAS[1] }",
            "{ [1] = CAS() }",
            "{ [1] = CAS CAP }",
            "{ [1] = CAS; }",
            "{ [1] = { CAS } }",
            "{ [1] = end }",
            // A name as a key in brackets; `CAS = 1` is a string key.
            "{ [CAS] = 1 }",
        ] {
            assert!(parse_returned(with_tasks(bad).as_bytes()).is_err(), "{bad}");
        }
        // A bare name in any other place.
        for bad in [
            "local p = { [\"payloads\"] = { [1] = { [\"name\"] = CAS } } }\nreturn p",
            "local p = { [\"payloads\"] = { [1] = { [\"pylons\"] = { [1] = { [\"CLSID\"] = X, [\"num\"] = 1 } } } } }\nreturn p",
            "local p = { [\"payloads\"] = { [1] = { [\"pylons\"] = { [1] = { [\"num\"] = N } } } } }\nreturn p",
            "local p = { [\"payloads\"] = { [1] = { [\"pylons\"] = { [1] = { [\"settings\"] = { [\"a\"] = B } } } } } }\nreturn p",
            "local p = { [\"unitType\"] = F16, [\"payloads\"] = {} }\nreturn p",
            "local p = { [\"name\"] = F16 }\nreturn p",
            "local p = { CAS }\nreturn p",
            "local p = { [\"tasks\"] = { [1] = CAS } }\nreturn p",
            "local p = { [\"payloads\"] = { [\"tasks\"] = { [1] = CAS } } }\nreturn p",
            "local p = { [\"payloads\"] = { [1] = { [\"tasks\"] = { [1] = { [1] = CAS } } } } }\nreturn p",
            "local x = CAS\nreturn x",
        ] {
            assert!(parse_returned(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn parse_refuses_a_bare_name_even_as_a_task() {
        let src = with_tasks("{ [1] = CAS }")
            .replace("local p", "p")
            .replace("\nreturn p\n", "\n");
        assert!(parse(src.as_bytes()).is_err());
        assert!(
            parse(b"x = { [\"payloads\"] = { [1] = { [\"tasks\"] = { [1] = 31 } } } }").is_ok()
        );
    }

    #[test]
    fn refuses_a_returned_local_in_any_other_form() {
        for (bad, line) in [
            ("x = {}\nreturn x", 1),
            ("local x = {}", 1),
            ("local x = {}\nreturn y", 2),
            ("local x = {}\nreturn x;", 2),
            ("local x = {}\nreturn x\nprint(x)", 3),
            ("local a = \"{EFT}\"\nlocal x = { a }\nreturn x", 2),
            ("local x = { [\"n\"] = \"a\" .. \"b\" }\nreturn x", 1),
            ("local x = {}\nx.y = 1\nreturn x", 2),
            ("local function f() end\nreturn f", 1),
            ("local return = {}\nreturn return", 1),
            ("local x, y = {}, {}\nreturn x", 1),
            ("localx = {}\nreturn localx", 1),
            ("", 1),
        ] {
            let e = parse_returned(bad.as_bytes()).expect_err(bad);
            assert!(e.starts_with(&format!("line {line}:")), "{bad}: {e}");
        }
    }

    #[test]
    fn edits_change_only_their_spans() {
        let d = parse(SRC.as_bytes()).unwrap();
        let st = d.root.get(&Key::s("start_time")).unwrap().span.clone();
        let out = apply(
            SRC.as_bytes(),
            vec![Edit {
                span: st.clone(),
                text: b"3600".to_vec(),
            }],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text, SRC.replacen("28800", "3600", 1));
        assert!(
            apply(
                SRC.as_bytes(),
                vec![
                    Edit {
                        span: st.clone(),
                        text: vec![]
                    },
                    Edit {
                        span: st.start..st.end + 1,
                        text: vec![]
                    },
                ]
            )
            .is_err()
        );
    }

    #[test]
    fn written_values_read_back_equal() {
        let v = Lv::Table(vec![
            (Lv::String(b"a".to_vec()), Lv::Number(0.1)),
            (Lv::Number(1.0), Lv::String("é\"\\\n9".as_bytes().to_vec())),
            (
                Lv::String(b"t".to_vec()),
                Lv::Table(vec![(Lv::Number(1.0), Lv::Bool(false))]),
            ),
            (Lv::String(b"e".to_vec()), Lv::Table(vec![])),
            (Lv::String(b"big".to_vec()), Lv::Number(-123456789.0)),
        ]);
        let src = format!("x = {}", write(&v, 0).unwrap());
        let d = parse(src.as_bytes()).unwrap();
        assert_eq!(value(src.as_bytes(), &d.root), Some(v));
        assert!(write(&Lv::Number(f64::NAN), 0).is_err());
        assert!(write(&Lv::Other("function".into()), 0).is_err());
    }
}
