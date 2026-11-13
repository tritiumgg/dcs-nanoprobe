//! The simulation adapter (plan 4.4): the one boundary between the
//! reconciler and DCS.
//!
//! The adapter speaks the agent's protocol. It writes commands to the inbound
//! ring and reads the records the agent wrote, in ring order. `RealSim` (step
//! S17) does that through the mapped file; `FakeSim` (crate
//! `nanoprobe-sim-fake`) models DCS and the agent; a recording replays what a
//! real agent wrote ([`crate::replay`]).

pub mod wire;

/// One outbound record as the link read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawRecord {
    /// The agent's boot that wrote it.
    pub agent_boot: u64,
    /// The record's number within that boot (spec/records.md, "Stamps").
    pub number: u64,
    /// The record kind.
    pub kind: u16,
    /// The payload.
    pub payload: Vec<u8>,
}

/// The adapter refused a command: no agent is attached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detached;

/// The adapter.
pub trait SimAdapter {
    /// The inbound position just after this command if it is the next one
    /// written. The reconciler journals it before it writes the command, and
    /// later compares it with a `HELLO`'s `in_head` (spec/commands.md).
    fn position_after(&self, opcode: u16, payload: &[u8]) -> u64;

    /// Write one command, opcode and whole payload, to the inbound ring.
    /// Returns the inbound position just after it.
    fn send(&mut self, opcode: u16, payload: &[u8]) -> Result<u64, Detached>;

    /// The records read since the last call, in ring order.
    fn read(&mut self) -> Vec<RawRecord>;
}
