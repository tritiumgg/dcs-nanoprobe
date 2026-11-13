//! Checks a stub's journals: no record lost, changed or out of order; every
//! loss accounted for by a `GAP`; every command answered at most once, as
//! `APPLIED`, with the digest of what was sent, and answered whenever the
//! agent took it; and no session ended by a failed check.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::{gap_class, kind, lifecycle, result};
use nanoprobe_transport::records::{CommandResult, Frame, Gap, Hello, Lifecycle};

use crate::content;

/// What the journals held.
#[derive(Debug, Default)]
pub struct Report {
    /// Agent boots seen.
    pub boots: usize,
    /// Distinct records, padding included.
    pub records: u64,
    /// Padding records: how often a boot wrapped its ring.
    pub padding: u64,
    /// Records journalled twice, with the same bytes.
    pub repeats: u64,
    /// Attaches.
    pub attaches: u64,
    /// Events and log lines checked.
    pub events: u64,
    /// Events and log lines a `GAP` accounts for.
    pub gapped: u64,
    /// Position samples checked.
    pub samples: u64,
    /// Commands sent.
    pub sent: u64,
    /// Results read.
    pub results: u64,
    /// Results `APPLIED`.
    pub applied: u64,
    /// Commands the agent took, by a later `HELLO`'s `in_head`.
    pub taken: u64,
    /// Commands taken with no result before a resync HELLO: their outcome
    /// is unknown, which the spec allows after a lost control record.
    pub unknown: u64,
    /// Commands written after a padding record: inbound ring wraps.
    pub inbound_wraps: u64,
    /// Records read again after a restart and checked against the journal.
    pub rereads: u64,
    /// Journal lines cut short by a kill: the last line of a file with no
    /// newline. Not an error; the next stub journals the record again.
    pub cut: u64,
    /// Every broken rule found.
    pub errors: Vec<String>,
}

/// Why a session may end when a peer is killed or restarted. Any other
/// reason the stub reports is a failed check, and an error.
pub const EXPECTED_ENDS: &[&str] = &[
    "the agent closed the connection",
    "the connection: ",
    "connect ",
    "send HELLO: ",
    "read the answer to HELLO: ",
    "no answer to HELLO within the handshake timeout",
    "the agent restarted",
    "the agent detached",
    "the agent is rewriting the header",
    "header: the header is not ready",
    "header: the header changed while it was read",
    "the file's agent boot ",
    "a poll came ",
];

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

fn num(s: Option<&str>) -> Option<u64> {
    u64::from_str_radix(s?, 16).ok()
}

struct Sent {
    boot: u64,
    end: u64,
    digest: u64,
    /// The journal line it was sent on, across all journals.
    line: usize,
}

/// Check every journal in `dir`.
pub fn verify(dir: &Path) -> std::io::Result<Report> {
    let mut names: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("journal-"))
        })
        .collect();
    names.sort();
    let mut report = Report::default();
    // boot -> number -> (kind, payload)
    let mut records: HashMap<u64, BTreeMap<u64, (u16, Vec<u8>)>> = HashMap::new();
    let mut sent: HashMap<(u64, u64), Sent> = HashMap::new();
    // (line, boot, in_head, resync) of each HELLO the stub took; a resync is
    // the second HELLO of an attach, after the agent lost a control record.
    let mut hellos: Vec<(usize, u64, u64, bool)> = Vec::new();
    // (boot, number, kind, digest) of each record read again.
    let mut rereads: Vec<(u64, u64, u16, u64)> = Vec::new();
    let mut boot_order = Vec::new();
    // The last HELLO number taken, per boot.
    let mut hello_numbers: HashMap<u64, u64> = HashMap::new();
    let mut at = 0usize;
    for path in names {
        let text = std::fs::read_to_string(&path)?;
        let file = path.display().to_string();
        let mut lines: Vec<&str> = text.lines().collect();
        if !text.is_empty() && !text.ends_with('\n') {
            // A kill cut the stub's last write short.
            lines.pop();
            report.cut += 1;
        }
        // The open attach of this stub: its line, whether it took a HELLO,
        // and whether a report ended it.
        let mut attach: Option<(usize, bool, bool)> = None;
        for (i, line) in lines.into_iter().enumerate() {
            at += 1;
            let mut f = line.split(' ');
            let tag = f.next();
            let bad = |r: &mut Report| r.errors.push(format!("{file}:{}: a malformed line", i + 1));
            match tag {
                Some("R") => {
                    let (Some(boot), Some(number), Some(k), Some(Some(payload))) = (
                        num(f.next()),
                        num(f.next()),
                        f.next().and_then(|k| k.parse().ok()),
                        f.next().map(unhex),
                    ) else {
                        bad(&mut report);
                        continue;
                    };
                    if !records.contains_key(&boot) {
                        boot_order.push(boot);
                    }
                    let m = records.entry(boot).or_default();
                    match m.get(&number) {
                        Some((pk, pp)) if *pk == k && *pp == payload => report.repeats += 1,
                        Some(_) => report.errors.push(format!(
                            "record {boot:x}/{number} journalled twice with other bytes"
                        )),
                        None => {
                            m.insert(number, (k, payload));
                        }
                    }
                }
                Some("S") => {
                    let (Some(boot), Some(epoch), Some(seq), Some(end), Some(digest)) = (
                        num(f.next()),
                        num(f.next()),
                        num(f.next()),
                        num(f.next()),
                        num(f.next()),
                    ) else {
                        bad(&mut report);
                        continue;
                    };
                    if f.next() == Some("1") {
                        report.inbound_wraps += 1;
                    }
                    let s = Sent {
                        boot,
                        end,
                        digest,
                        line: at,
                    };
                    if sent.insert((epoch, seq), s).is_some() {
                        report
                            .errors
                            .push(format!("command {epoch}/{seq} sent twice"));
                    }
                    report.sent += 1;
                }
                Some("H") => {
                    let (Some(boot), Some(_epoch), Some(_last), Some(in_head)) =
                        (num(f.next()), num(f.next()), num(f.next()), num(f.next()))
                    else {
                        bad(&mut report);
                        continue;
                    };
                    let resync = attach.is_some_and(|a| a.1);
                    hellos.push((at, boot, in_head, resync));
                    if let Some(a) = attach.as_mut() {
                        a.1 = true;
                    }
                    // A resync writes another HELLO later in the ring; a HELLO
                    // taken twice repeats or goes back.
                    if let Some(number) = num(f.next()) {
                        let last = hello_numbers.entry(boot).or_insert(0);
                        if number <= *last {
                            report.errors.push(format!(
                                "{file}:{}: HELLO {number} taken again, after {last}",
                                i + 1
                            ));
                        }
                        *last = number;
                    }
                }
                Some("D") => {
                    let (Some(boot), Some(number), Some(k), Some(digest)) = (
                        num(f.next()),
                        num(f.next()),
                        f.next().and_then(|k| k.parse().ok()),
                        num(f.next()),
                    ) else {
                        bad(&mut report);
                        continue;
                    };
                    rereads.push((boot, number, k, digest));
                }
                Some("A") => {
                    report.attaches += 1;
                    if let Some((line, false, false)) = attach {
                        report
                            .errors
                            .push(format!("{file}:{line}: an attach that read no HELLO"));
                    }
                    attach = Some((i + 1, false, false));
                }
                Some("X") => {
                    if let Some(a) = attach.as_mut() {
                        a.2 = true;
                    }
                    let why = line.get(2..).unwrap_or("");
                    if !EXPECTED_ENDS.iter().any(|e| why.starts_with(e)) {
                        report
                            .errors
                            .push(format!("{file}:{}: a failed check: {why}", i + 1));
                    }
                }
                _ => bad(&mut report),
            }
        }
    }
    report.boots = records.len();
    for (boot, number, k, digest) in rereads {
        report.rereads += 1;
        match records.get(&boot).and_then(|m| m.get(&number)) {
            Some((pk, p)) if *pk == k && content::digest(p) as u64 == digest => {}
            Some(_) => report.errors.push(format!(
                "record {boot:x}/{number} read again with other bytes"
            )),
            None => report.errors.push(format!(
                "record {boot:x}/{number} read again but never journalled"
            )),
        }
    }
    let mut answered: HashMap<(u64, u64), u16> = HashMap::new();
    for boot in boot_order {
        let m = &records[&boot];
        check_boot(boot, m, &sent, &mut answered, &mut report);
    }
    // A command at or before the in_head of the next HELLO of the same boot
    // was taken, so it has a result (spec/commands.md, "Unanswered
    // commands"). Only the next one: an attach after it resets in_tail to
    // in_head, and later commands reuse the positions.
    for ((epoch, seq), s) in &sent {
        let next = hellos
            .iter()
            .find(|&&(line, boot, _, _)| line > s.line && boot == s.boot);
        let taken = next.is_some_and(|&(_, _, in_head, _)| in_head >= s.end);
        if taken {
            report.taken += 1;
            // After a lost control record the result may be the one lost:
            // the outcome is unknown (spec/commands.md, "Unanswered commands").
            let resync = next.is_some_and(|h| h.3);
            if resync && !answered.contains_key(&(*epoch, *seq)) {
                report.unknown += 1;
            } else if !answered.contains_key(&(*epoch, *seq)) {
                report.errors.push(format!(
                    "command {epoch}/{seq} was taken (a later HELLO's in_head passed it) and never answered"
                ));
            }
        }
    }
    Ok(report)
}

fn check_boot(
    boot: u64,
    m: &BTreeMap<u64, (u16, Vec<u8>)>,
    sent: &HashMap<(u64, u64), Sent>,
    answered: &mut HashMap<(u64, u64), u16>,
    report: &mut Report,
) {
    let mut err = |e: String| report.errors.push(format!("boot {boot:x}: {e}"));
    let mut expect = 1;
    let mut frame: Option<Frame> = None;
    let mut current_frame = 0;
    let mut in_head = 0;
    // Per class: the last sequence number, holes so far, GAP counts so far.
    let mut last = [0u64; 2];
    let mut holes = [0u64; 2];
    let mut gapped = [0u64; 2];
    let (mut events, mut samples, mut results, mut applied, mut padding) = (0, 0, 0, 0, 0);
    let fixed = Lifecycle {
        state: lifecycle::RUNNING as u32,
        mission_gen: 1,
        theater: "Fake".into(),
        mission: "fake.miz".into(),
    };
    for (&number, (k, payload)) in m {
        if number != expect {
            err(format!("records {expect} to {} are missing", number - 1));
        }
        expect = number + 1;
        let k = u64::from(*k);
        let class_of = |k| if k == kind::LOG { 1 } else { 0 };
        match k {
            kind::PADDING => {
                if payload.is_empty() {
                    padding += 1;
                } else {
                    err(format!("padding {number} with a payload"));
                }
            }
            // The sidecar numbers a FRAME by the number it names and checks
            // that against the records before it; here the stamps are checked.
            kind::FRAME => match Frame::decode(payload) {
                Ok(f)
                    if f.real_time != f.model_time
                        || f.mission_gen != 1
                        || f.lifecycle != lifecycle::RUNNING as u32
                        || f.campaign != 0
                        || f.replay > 1 =>
                {
                    err(format!(
                        "FRAME {number} has stamps the fake agent never writes: {f:?}"
                    ))
                }
                Ok(f) => {
                    // Current frames only move forward. A replayed FRAME
                    // stamps queued records of any earlier frame.
                    if f.replay == 0 {
                        if f.frame < current_frame {
                            err(format!("FRAME {number} goes back to frame {}", f.frame));
                        }
                        current_frame = f.frame;
                    }
                    frame = Some(f);
                }
                Err(e) => err(format!("FRAME {number}: {e}")),
            },
            _ if frame.is_none() => err(format!("record {number} before any FRAME")),
            kind::AUTHOR_EVENT | kind::LOG => {
                let c = class_of(k);
                let seq = if k == kind::LOG {
                    content::check_log(boot, payload)
                } else {
                    content::check_event(boot, payload)
                };
                match seq {
                    Ok(seq) if seq > last[c] => {
                        holes[c] += seq - last[c] - 1;
                        last[c] = seq;
                        events += 1;
                        if holes[c] != gapped[c] {
                            err(format!(
                                "class {c}: {} lost before seq {seq}, GAPs say {}",
                                holes[c], gapped[c]
                            ));
                            gapped[c] = holes[c];
                        }
                    }
                    Ok(seq) => err(format!("record {number}: seq {seq} after {}", last[c])),
                    Err(e) => err(format!("record {number}: {e}")),
                }
            }
            kind::GAP
                if frame.is_some_and(|f| {
                    Gap::decode(payload).is_ok_and(|g| g.to_real != f.real_time)
                }) =>
            {
                err(format!(
                    "GAP {number} is not stamped with its last lost record"
                ))
            }
            kind::GAP => match Gap::decode(payload) {
                Ok(g)
                    if g.from_real > g.to_real
                        || g.from_gen != 1
                        || g.to_gen != 1
                        || g.count == 0 =>
                {
                    err(format!("GAP {number} has an impossible span: {g:?}"))
                }
                Ok(g) if g.class == gap_class::EVENTS as u32 => gapped[0] += u64::from(g.count),
                Ok(g) if g.class == gap_class::LOGS as u32 => gapped[1] += u64::from(g.count),
                Ok(g) => err(format!("GAP {number} of class {}", g.class)),
                Err(e) => err(format!("GAP {number}: {e}")),
            },
            kind::POSITIONS => {
                let f = frame.expect("a frame");
                if f.replay != 0 {
                    err(format!("samples {number} after a replayed FRAME"));
                }
                match content::check_positions(boot, f.frame, payload) {
                    Ok(()) => samples += 1,
                    Err(e) => err(format!("record {number}: {e}")),
                }
            }
            kind::RESULT => match CommandResult::decode(payload) {
                Ok(r) => {
                    results += 1;
                    let key = (r.epoch, r.seq);
                    match sent.get(&key) {
                        None => err(format!("a result for {key:?}, never sent")),
                        Some(s) if Lv::Number(s.digest as f64) != r.value => {
                            err(format!("the result for {key:?} names other bytes"))
                        }
                        Some(s) if s.boot != boot => {
                            err(format!("the result for {key:?} is from another boot"))
                        }
                        Some(_) => {}
                    }
                    if u64::from(r.opcode) != content::OPCODE
                        || r.campaign != 0
                        || r.entity != 0
                        || r.rev != 0
                        || r.object != 0
                    {
                        err(format!(
                            "the result for {key:?} has fields its command did not: {r:?}"
                        ));
                    }
                    if u64::from(r.code) == result::APPLIED {
                        applied += 1;
                    } else {
                        // Each epoch's commands are new to the agent.
                        err(format!("{key:?} answered {}, not APPLIED", r.code));
                    }
                    if let Some(prev) = answered.insert(key, r.code) {
                        err(format!("{key:?} answered twice: {prev} then {}", r.code));
                    }
                }
                Err(e) => err(format!("RESULT {number}: {e}")),
            },
            kind::HELLO => match Hello::decode(payload) {
                Ok(h) if h.agent_boot != boot => err(format!("HELLO {number} of another boot")),
                Ok(h) if h.in_head < in_head => err(format!(
                    "HELLO {number}: in_head moved back to {:#x}",
                    h.in_head
                )),
                Ok(h) => in_head = h.in_head,
                Err(e) => err(format!("HELLO {number}: {e}")),
            },
            kind::LIFECYCLE => match Lifecycle::decode(payload) {
                Ok(l) if l == fixed => {}
                Ok(l) => err(format!("LIFECYCLE {number} is {l:?}")),
                Err(e) => err(format!("LIFECYCLE {number}: {e}")),
            },
            other => err(format!("record {number} of kind {other}")),
        }
    }
    report.records += m.len() as u64;
    report.padding += padding;
    report.events += events;
    report.gapped += gapped[0] + gapped[1];
    report.samples += samples;
    report.results += results;
    report.applied += applied;
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoprobe_transport::records::Gap;

    const BOOT: u64 = 0xb007;

    fn hexs(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn frame(number: u64, frame: u64) -> String {
        let f = Frame {
            frame,
            number: number + 1,
            mission_gen: 1,
            lifecycle: lifecycle::RUNNING as u32,
            ..Frame::default()
        };
        format!(
            "R {BOOT:x} {number:x} {} {}",
            kind::FRAME,
            hexs(&f.encode())
        )
    }

    fn event(number: u64, seq: u64) -> String {
        format!(
            "R {BOOT:x} {number:x} {} {}",
            kind::AUTHOR_EVENT,
            hexs(&content::event(BOOT, seq))
        )
    }

    fn run(lines: &[String]) -> Report {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("nanoprobe-verify-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("journal-000001.txt"), lines.join("\n") + "\n").expect("write");
        let r = verify(&dir).expect("verify");
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    #[test]
    fn a_clean_journal_passes() {
        let r = run(&[frame(1, 1), event(2, 1), event(3, 2), event(3, 2)]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!((r.records, r.repeats, r.events), (3, 1, 2));
    }

    #[test]
    fn a_missing_record_is_found() {
        let r = run(&[frame(1, 1), event(3, 1)]);
        assert!(
            r.errors.iter().any(|e| e.contains("missing")),
            "{:?}",
            r.errors
        );
    }

    #[test]
    fn a_lost_event_without_a_gap_is_found_and_a_gap_explains_one() {
        let r = run(&[frame(1, 1), event(2, 1), event(3, 3)]);
        assert!(
            r.errors.iter().any(|e| e.contains("GAPs say")),
            "{:?}",
            r.errors
        );
        let g = Gap {
            class: gap_class::EVENTS as u32,
            count: 1,
            from_gen: 1,
            to_gen: 1,
            ..Gap::default()
        };
        let gap = format!("R {BOOT:x} 3 {} {}", kind::GAP, hexs(&g.encode()));
        let r = run(&[frame(1, 1), event(2, 1), gap, event(4, 3)]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
    }

    #[test]
    fn changed_bytes_are_found() {
        let mut bad = event(2, 1);
        let last = bad.pop().expect("a digit");
        bad.push(if last == '0' { '1' } else { '0' });
        let r = run(&[frame(1, 1), bad]);
        assert!(!r.errors.is_empty());
        let r = run(&[frame(1, 1), event(2, 1), event(2, 2)]);
        assert!(
            r.errors.iter().any(|e| e.contains("other bytes")),
            "{:?}",
            r.errors
        );
    }

    #[test]
    fn a_result_for_a_command_never_sent_is_found() {
        let res = CommandResult {
            seq: 1,
            epoch: 1,
            campaign: 0,
            entity: 0,
            rev: 0,
            object: 0,
            cost_us: 0,
            opcode: 49,
            code: 0,
            message: String::new(),
            value: Lv::Number(5.0),
        };
        let line = format!("R {BOOT:x} 2 {} {}", kind::RESULT, hexs(&res.encode()));
        let r = run(&[frame(1, 1), line.clone()]);
        assert!(
            r.errors.iter().any(|e| e.contains("never sent")),
            "{:?}",
            r.errors
        );
        let r = run(&["S b007 1 1 40 5".into(), frame(1, 1), line.clone()]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let r = run(&["S b007 1 1 40 6".into(), frame(1, 1), line]);
        assert!(
            r.errors.iter().any(|e| e.contains("other bytes")),
            "{:?}",
            r.errors
        );
    }

    fn result(code: u64) -> String {
        let res = CommandResult {
            seq: 1,
            epoch: 1,
            campaign: 0,
            entity: 0,
            rev: 0,
            object: 0,
            cost_us: 0,
            opcode: 49,
            code: code as u16,
            message: String::new(),
            value: Lv::Number(5.0),
        };
        format!("R {BOOT:x} 2 {} {}", kind::RESULT, hexs(&res.encode()))
    }

    #[test]
    fn a_result_that_is_not_applied_is_found() {
        let r = run(&[
            "S b007 1 1 40 5".into(),
            frame(1, 1),
            result(result::DUPLICATE),
        ]);
        assert!(
            r.errors.iter().any(|e| e.contains("not APPLIED")),
            "{:?}",
            r.errors
        );
        let r = run(&[
            "S b007 1 1 40 5".into(),
            frame(1, 1),
            result(result::APPLIED),
        ]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
    }

    #[test]
    fn a_taken_command_with_no_result_is_found() {
        // Sent ending at 0x40; a later HELLO's in_head 0x40 says it was taken.
        let r = run(&[
            "S b007 1 1 40 5".into(),
            "H b007 2 0 40".into(),
            frame(1, 1),
        ]);
        assert!(
            r.errors.iter().any(|e| e.contains("never answered")),
            "{:?}",
            r.errors
        );
        let r = run(&[
            "S b007 1 1 40 5".into(),
            "H b007 2 0 38".into(),
            frame(1, 1),
        ]);
        assert!(r.errors.is_empty(), "not taken: {:?}", r.errors);
    }

    #[test]
    fn a_failed_check_is_an_error_and_a_kill_is_not() {
        let r = run(&[frame(1, 1), "X the agent closed the connection".into()]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let r = run(&[
            frame(1, 1),
            "X outbound ring: record at 0x8 (len 9, kind 1): record passes the tail".into(),
        ]);
        assert!(
            r.errors.iter().any(|e| e.contains("a failed check")),
            "{:?}",
            r.errors
        );
    }

    #[test]
    fn padding_counts_as_a_record() {
        let pad = format!("R {BOOT:x} 2 0 ");
        let r = run(&[frame(1, 1), pad, event(3, 1)]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.padding, 1);
    }

    #[test]
    fn a_wrong_stamp_is_found() {
        let f = Frame {
            frame: 1,
            number: 2,
            mission_gen: 2,
            lifecycle: lifecycle::RUNNING as u32,
            ..Frame::default()
        };
        let line = format!("R {BOOT:x} 1 {} {}", kind::FRAME, hexs(&f.encode()));
        let r = run(&[line]);
        assert!(
            r.errors.iter().any(|e| e.contains("never writes")),
            "{:?}",
            r.errors
        );
    }

    #[test]
    fn an_empty_directory_has_nothing_to_report() {
        let r = run(&[]);
        assert_eq!((r.records, r.boots), (0, 0));
    }

    fn run_files(files: &[&[String]]) -> Report {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("nanoprobe-verify-files-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        for (i, lines) in files.iter().enumerate() {
            std::fs::write(
                dir.join(format!("journal-{:06}.txt", i + 1)),
                lines.join("\n"),
            )
            .expect("write");
        }
        let r = verify(&dir).expect("verify");
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    fn result_for(epoch: u64, seq: u64, number: u64, digest: f64) -> String {
        let res = CommandResult {
            seq,
            epoch,
            campaign: 0,
            entity: 0,
            rev: 0,
            object: 0,
            cost_us: 0,
            opcode: 49,
            code: 0,
            message: String::new(),
            value: Lv::Number(digest),
        };
        format!(
            "R {BOOT:x} {number:x} {} {}",
            kind::RESULT,
            hexs(&res.encode())
        )
    }

    #[test]
    fn only_the_next_hello_decides_whether_a_command_was_taken() {
        // 1/1 ends at 0x40; the next HELLO's in_head 0x38 says it was not
        // taken. A new sidecar then sends 2/1 at the same positions, and the
        // HELLO after it says that one was taken.
        let mut lines = vec![
            "S b007 1 1 40 5".to_string(),
            "A b007 c 0".into(),
            "H b007 2 0 38".into(),
            "S b007 2 1 40 6".into(),
            "A b007 d 0".into(),
            "H b007 3 0 40".into(),
            frame(1, 1),
        ];
        let r = run(&lines);
        assert!(
            r.errors
                .iter()
                .any(|e| e.contains("2/1") && e.contains("never answered")),
            "{:?}",
            r.errors
        );
        assert!(
            !r.errors.iter().any(|e| e.contains("1/1")),
            "{:?}",
            r.errors
        );
        lines.push(result_for(2, 1, 2, 6.0));
        let r = run(&lines);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.taken, 1);
    }

    #[test]
    fn a_line_cut_by_a_kill_is_dropped_and_journalled_again() {
        let full = event(2, 1);
        let cut = full[..full.len() - 7].to_string();
        // The first stub's file ends mid-line with no newline.
        let one = [frame(1, 1), cut];
        let two = [full + "\n"];
        let r = run_files(&[&one, &two]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.cut, 1);
    }

    #[test]
    fn a_record_read_again_with_other_bytes_is_found() {
        let e = content::event(BOOT, 1);
        let d = content::digest(&e) as u64;
        let good = format!("D {BOOT:x} 2 {} {d:x}", kind::AUTHOR_EVENT);
        let r = run(&[frame(1, 1), event(2, 1), good]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.rereads, 1);
        let bad = format!("D {BOOT:x} 2 {} {:x}", kind::AUTHOR_EVENT, d ^ 1);
        let r = run(&[frame(1, 1), event(2, 1), bad]);
        assert!(
            r.errors
                .iter()
                .any(|e| e.contains("read again with other bytes")),
            "{:?}",
            r.errors
        );
    }

    fn hello(number: u64, in_head: u64) -> String {
        let h = Hello {
            agent_boot: BOOT,
            in_head,
            ..Hello::default()
        };
        format!(
            "R {BOOT:x} {number:x} {} {}",
            kind::HELLO,
            hexs(&h.encode())
        )
    }

    #[test]
    fn the_checks_of_each_kind_find_what_they_check() {
        let has = |r: &Report, t: &str| r.errors.iter().any(|e| e.contains(t));
        let r = run(&[frame(1, 1), hello(2, 0x40), hello(3, 0x38)]);
        assert!(has(&r, "in_head moved back"), "{:?}", r.errors);
        let g = Gap {
            class: gap_class::EVENTS as u32,
            count: 0,
            from_gen: 1,
            to_gen: 1,
            ..Gap::default()
        };
        let line = format!("R {BOOT:x} 2 {} {}", kind::GAP, hexs(&g.encode()));
        assert!(has(&run(&[frame(1, 1), line]), "impossible span"));
        let l = Lifecycle {
            state: 1,
            mission_gen: 1,
            theater: "Fake".into(),
            mission: "fake.miz".into(),
        };
        let line = format!("R {BOOT:x} 2 {} {}", kind::LIFECYCLE, hexs(&l.encode()));
        assert!(has(&run(&[frame(1, 1), line]), "LIFECYCLE 2 is"));
        let mut res = CommandResult::decode(
            &unhex(result(result::APPLIED).rsplit(' ').next().expect("hex")).expect("hex"),
        )
        .expect("a result");
        res.entity = 4;
        let line = format!("R {BOOT:x} 2 {} {}", kind::RESULT, hexs(&res.encode()));
        assert!(has(
            &run(&["S b007 1 1 40 5".into(), frame(1, 1), line]),
            "fields its command did not"
        ));
        let other = format!("S {:x} 1 1 40 5", BOOT + 1);
        assert!(has(
            &run(&[other, frame(1, 1), result(result::APPLIED)]),
            "another boot"
        ));
        let twice = [
            "S b007 1 1 40 5".into(),
            frame(1, 1),
            result(result::APPLIED),
            result_for(1, 1, 3, 5.0),
        ];
        assert!(has(&run(&twice), "answered twice"));
        let gap_stamp = Gap {
            class: gap_class::EVENTS as u32,
            count: 1,
            to_real: 9.0,
            from_gen: 1,
            to_gen: 1,
            ..Gap::default()
        };
        let line = format!("R {BOOT:x} 2 {} {}", kind::GAP, hexs(&gap_stamp.encode()));
        assert!(has(
            &run(&[frame(1, 1), line]),
            "not stamped with its last lost record"
        ));
        let resync = [
            "A b007 c 0".into(),
            "H b007 1 0 0 2".into(),
            "H b007 2 0 0 9".into(),
        ];
        assert!(
            run(&resync).errors.is_empty(),
            "a resync HELLO is later in the ring"
        );
        let again = [
            "A b007 c 0".into(),
            "H b007 1 0 0 9".into(),
            "A b007 d 0".into(),
            "H b007 2 0 0 9".into(),
        ];
        assert!(has(&run(&again), "taken again"));
        let silent = [
            "A b007 c 0".into(),
            "A b007 d 0".into(),
            "H b007 2 0 0 9".into(),
        ];
        assert!(has(&run(&silent), "read no HELLO"));
        let ended = [
            "A b007 c 0".into(),
            "X the agent closed the connection".into(),
            "A b007 d 0".into(),
            "H b007 2 0 0 9".into(),
        ];
        assert!(run(&ended).errors.is_empty(), "an attach a kill ended");
    }
}
