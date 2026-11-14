//! `nanoprobe inspect`: a snapshot of the transport file as text.
//!
//! The status block and both rings are copied into private memory once. Every
//! decode reads that copy, because the agent may write the file at any time.
//! The metrics are read from the live file under their sequence lock.

use std::fmt::Write as _;

use nanoprobe_transport::golden;
use nanoprobe_transport::layout::{CAPACITY, status as s};
use nanoprobe_transport::mem::{Heap, Memory};
use nanoprobe_transport::ring::{Consumer, Ring, RingError};
use nanoprobe_transport::status::{self, Snapshot};

use crate::decode;
use crate::util::{self, flag_names, hist_summary, named};

/// What to print.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Records to list per ring.
    pub records: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            records: crate::args::DEFAULT_RECORDS,
        }
    }
}

/// Bytes copied from the live rings at a time.
const CHUNK: usize = 1 << 16;

/// Render the snapshot of `live`, a file of `file_len` bytes.
pub fn render(live: &impl Memory, file_len: u64, opts: &Options) -> String {
    let snap = util::snapshot(live);
    let geometry = geometry(&snap, live.len() as u64);
    let copy = match geometry {
        Some((out_cap, in_cap)) => {
            let total = s::SIZE + (out_cap + in_cap) as usize;
            let heap = Heap::new(total);
            heap.write(0, snap.bytes());
            copy_range(live, &heap, s::SIZE, total);
            heap
        }
        None => Heap::from_bytes(snap.bytes()),
    };
    // The tails after the copy: bytes more than a capacity behind them may
    // be newer than the snapshot's positions.
    let tails_after = match geometry {
        Some(_) => (
            live.load(s::OUT_TAIL, std::sync::atomic::Ordering::Acquire),
            live.load(s::IN_TAIL, std::sync::atomic::Ordering::Acquire),
        ),
        None => (0, 0),
    };

    let mut out = String::new();
    let _ = writeln!(out, "file: {file_len} bytes");
    match status::check(&copy, file_len) {
        Ok(g) => {
            let _ = writeln!(
                out,
                "header check: ok (interface {}, out {} bytes, in {} bytes)",
                g.interface, g.out_capacity, g.in_capacity
            );
        }
        Err(e) => {
            let _ = writeln!(out, "header check: FAILED: {e}");
        }
    }
    out.push('\n');
    status_fields(&mut out, &snap);
    out.push('\n');
    metrics(&mut out, live);
    match geometry {
        Some((out_cap, in_cap)) => {
            let outbound = Ring::outbound(out_cap);
            let inbound = Ring::inbound(out_cap, in_cap);
            out.push('\n');
            list_ring(
                &mut out,
                &copy,
                &RingView {
                    label: "outbound ring (agent to sidecar)",
                    ring: outbound,
                    head: snap.u64(s::OUT_HEAD),
                    reused_below: tails_after.0.saturating_sub(outbound.capacity),
                    section: "record_kind",
                    inbound: false,
                },
                opts.records,
            );
            out.push('\n');
            list_ring(
                &mut out,
                &copy,
                &RingView {
                    label: "inbound ring (sidecar to agent)",
                    ring: inbound,
                    head: snap.u64(s::IN_HEAD),
                    reused_below: tails_after.1.saturating_sub(inbound.capacity),
                    section: "opcode",
                    inbound: true,
                },
                opts.records,
            );
        }
        None => {
            out.push_str("\nrings: not listed, the ring geometry in the header is not sane\n");
        }
    }
    out
}

/// The two capacities when the header's ring geometry is sane for a memory of
/// `mem_len` bytes.
fn geometry(snap: &Snapshot, mem_len: u64) -> Option<(u64, u64)> {
    let out_cap = snap.u64(s::OUT_CAPACITY);
    let in_cap = snap.u64(s::IN_CAPACITY);
    let sane = |c: u64| c.is_power_of_two() && CAPACITY.contains(&c);
    let size = s::SIZE as u64;
    (sane(out_cap)
        && sane(in_cap)
        && snap.u64(s::OUT_OFFSET) == size
        && snap.u64(s::IN_OFFSET) == size + out_cap
        && size + out_cap + in_cap <= mem_len)
        .then_some((out_cap, in_cap))
}

fn copy_range(live: &impl Memory, heap: &Heap, start: usize, end: usize) {
    let mut buf = vec![0u8; CHUNK];
    let mut at = start;
    while at < end {
        let n = CHUNK.min(end - at);
        live.read(at, &mut buf[..n]);
        heap.write(at, &buf[..n]);
        at += n;
    }
}

/// Fields that print in hex: boots, hashes, the build and the breadcrumb.
fn hex_field(name: &str) -> bool {
    name.contains("boot")
        || matches!(
            name,
            "magic" | "layout_hash" | "build" | "attach_seen" | "crumb" | "crumb_arg"
        )
}

fn status_fields(out: &mut String, snap: &Snapshot) {
    let layout = golden::layout("status");
    let _ = writeln!(out, "status block (offset, field, value):");
    for &(name, off, width, count, _) in s::FIELDS {
        // The metrics fields print in their own section, from the sequence lock.
        if s::METRICS.contains(&off) {
            continue;
        }
        let ty = layout
            .as_ref()
            .and_then(|l| l.field(name))
            .map_or("u64", |f| f.ty);
        let value = if count > 1 {
            String::from("(array)")
        } else if ty == "f64" {
            snap.f64(off).to_string()
        } else {
            let v = if width == 8 {
                snap.u64(off)
            } else {
                u64::from(snap.u32(off))
            };
            match name {
                "lifecycle" => named("lifecycle", v),
                "flags" => flag_names("agent_flags", v),
                n if hex_field(n) => format!("{v:#x}"),
                _ => v.to_string(),
            }
        };
        let _ = writeln!(out, "  {off:#06x} {name:<20} {value}");
    }
}

fn metrics(out: &mut String, live: &impl Memory) {
    let _ = writeln!(out, "metrics (read under the sequence lock):");
    let Some(m) = util::metrics(live) else {
        let _ = writeln!(out, "  unavailable: the agent kept writing them\n");
        return;
    };
    let _ = writeln!(out, "  slice_us             {}", m.slice_us);
    let _ = writeln!(out, "  frame_interval_us    {}", m.frame_interval_us);
    for (name, v) in [
        "period_position",
        "period_vitals",
        "period_ammo",
        "period_warehouse",
        "period_enumeration",
    ]
    .iter()
    .zip(m.periods)
    {
        let _ = writeln!(out, "  {name:<20} {v}");
    }
    let _ = writeln!(out, "  rows_per_clock       {}", m.rows_per_clock);
    for (name, h) in [
        "hist_pump",
        "hist_interval",
        "hist_command",
        "hist_event",
        "hist_syscall",
    ]
    .iter()
    .zip(&m.hist)
    {
        let total: u128 = h.iter().map(|&c| u128::from(c)).sum();
        let _ = writeln!(out, "  {name:<20} {total} samples  {}", hist_summary(h));
    }
}

struct RingView {
    label: &'static str,
    ring: Ring,
    head: u64,
    /// Positions below this may have been written over while the ring was
    /// copied: the producer's tail after the copy, less the capacity.
    reused_below: u64,
    section: &'static str,
    inbound: bool,
}

fn list_ring(out: &mut String, copy: &Heap, view: &RingView, limit: usize) {
    let tail = copy.load(view.ring.tail, std::sync::atomic::Ordering::Relaxed);
    let _ = writeln!(
        out,
        "{}: head {} tail {} capacity {}",
        view.label, view.head, tail, view.ring.capacity
    );
    if !view.head.is_multiple_of(8) {
        let _ = writeln!(
            out,
            "  ring check failed: head {:#x} is not a multiple of 8",
            view.head
        );
        return;
    }
    if view.head < view.reused_below {
        let _ = writeln!(
            out,
            "  not listed: the producer wrote over unread records while they were copied; run inspect again"
        );
        return;
    }
    let mut consumer = Consumer::new(view.ring, view.head);
    if let Err(e) = consumer.refresh(copy) {
        let _ = writeln!(out, "  ring check failed: {e}");
        return;
    }
    if consumer.pending() == 0 {
        let _ = writeln!(out, "  no unread records");
        return;
    }
    let mut payload = Vec::new();
    let mut listed = 0usize;
    let mut skipped = 0usize;
    loop {
        match consumer.next(copy, &mut payload) {
            Ok(None) => break,
            Ok(Some(read)) => {
                if listed >= limit {
                    skipped += 1;
                    continue;
                }
                listed += 1;
                let name = golden::code_name(view.section, u64::from(read.kind));
                let text = if view.inbound {
                    decode::command(name, &payload)
                } else {
                    decode::record(name, &payload)
                };
                let shown = name.map_or_else(|| format!("kind {}", read.kind), str::to_string);
                let _ = writeln!(
                    out,
                    "  {:>10}  {:<18} len={:<6} {}",
                    read.position,
                    shown,
                    payload.len(),
                    text
                );
            }
            Err(e) => {
                ring_error(out, &e);
                break;
            }
        }
    }
    if skipped > 0 {
        let _ = writeln!(out, "  ... {skipped} more unread records not listed");
    }
}

fn ring_error(out: &mut String, e: &RingError) {
    let _ = writeln!(out, "  ring check failed: {e}; the listing ends here");
}
