//! Players: keys, the player registry and seat occupancy (spec/world.md,
//! "Players"; spec/records.md, "Layout: PLAYER" and "Layout: MENU").
//!
//! A [`Roster`] is server-scoped. It reads every record the agent writes, as
//! the API's driver does, and keeps who is connected, in which slot, in which
//! unit, and how much fuel each player used in a campaign. It outputs
//! [`Note`]s, which the API turns into player events.
//!
//! A ucid enters the roster in a `PLAYER` record or a `PLAYERS` result and
//! stays inside it. Everything the roster hands out is keyed by the player
//! key, the first 16 bytes of HMAC-SHA-256 of the ucid under the server's
//! secret ([`Keyring`]). Only [`Roster::ucid_of`] gives a ucid back, for the
//! one caller that writes a `MODERATE` command.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::result as rc;
use nanoprobe_transport::records::Menu;
use sha2::{Digest, Sha256};

use crate::ids::EntityId;
use crate::sim::RawRecord;
use crate::sim::wire::{Input, lv, opcode};
use crate::store::scope;

mod attribution;
pub use attribution::{Attribution, Departed, LaunchRule, Row};
use attribution::{Memory, field};

/// `player_event` codes (spec/records.md, "Codes: player_event").
mod pe {
    pub const CONNECT: u32 = 1;
    pub const DISCONNECT: u32 = 2;
    pub const SLOT: u32 = 3;
    pub const CHAT: u32 = 4;
}

/// `S_EVENT_BIRTH`.
const BIRTH: u32 = 15;
/// `event_field` codes the roster reads.
const FIELD_INITIATOR: u64 = 2;
const FIELD_INITIATOR_MISSION_ID: u64 = 21;

/// The most finished occupancies kept.
pub const HISTORY_KEPT: usize = 20_000;

/// The server's secret and the map from key to ucid: what the server-scoped
/// store keeps (spec/world.md, "Players"). Neither is ever logged or shown.
#[derive(Clone)]
pub struct Keyring {
    secret: [u8; 32],
    ucids: BTreeMap<String, String>,
    /// The file the keyring is kept in, when persistence is on.
    file: Option<PathBuf>,
    /// A pair or the secret is not in the file yet.
    dirty: bool,
}

/// The keyring's file name inside the store directory (`[store] dir`).
pub const KEYRING_FILE: &str = "players.json";

impl fmt::Debug for Keyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keyring")
            .field("players", &self.ucids.len())
            .finish_non_exhaustive()
    }
}

impl Keyring {
    /// A keyring with a new random secret.
    pub fn generate() -> Keyring {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).expect("the operating system gives random bytes");
        Keyring::from_secret(secret)
    }

    /// A keyring over a secret kept earlier, or one that several servers
    /// share.
    pub fn from_secret(secret: [u8; 32]) -> Keyring {
        Keyring {
            secret,
            ucids: BTreeMap::new(),
            file: None,
            dirty: false,
        }
    }

    /// The keyring kept in `dir`, which is the store directory: the file
    /// [`KEYRING_FILE`] there, read when it exists, made with a new secret
    /// when it does not. The keyring is written again by [`Keyring::flush`]
    /// whenever a pair was learned. A file that cannot be read is an error:
    /// a new secret would give every player a new key.
    pub fn open(dir: &Path) -> io::Result<Keyring> {
        let file = dir.join(KEYRING_FILE);
        let mut ring = match std::fs::read(&file) {
            Ok(bytes) => Keyring::parse(&bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let mut r = Keyring::generate();
                r.dirty = true;
                r
            }
            Err(e) => return Err(e),
        };
        ring.file = Some(file);
        ring.flush()?;
        Ok(ring)
    }

    fn parse(bytes: &[u8]) -> io::Result<Keyring> {
        // The message names no content: the file holds the secret.
        let bad = || io::Error::new(io::ErrorKind::InvalidData, "players.json is malformed");
        let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| bad())?;
        let hex = v.get("secret").and_then(|s| s.as_str()).ok_or_else(bad)?;
        if hex.len() != 64 || !hex.is_ascii() {
            return Err(bad());
        }
        let mut secret = [0u8; 32];
        for (i, b) in secret.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
        }
        let mut ring = Keyring::from_secret(secret);
        if let Some(players) = v.get("players") {
            for (key, ucid) in players.as_object().ok_or_else(bad)? {
                ring.restore(key, ucid.as_str().ok_or_else(bad)?);
            }
        }
        Ok(ring)
    }

    /// Write the keyring when something changed since the last write: to a
    /// temporary file with mode 0600, then renamed over [`KEYRING_FILE`].
    /// Nothing happens when persistence is off. On an error the keyring
    /// stays unwritten and the next call tries again.
    pub fn flush(&mut self) -> io::Result<()> {
        let Some(file) = self.file.clone() else {
            return Ok(());
        };
        if !self.dirty {
            return Ok(());
        }
        let secret: String = self.secret.iter().map(|b| format!("{b:02x}")).collect();
        let doc = serde_json::json!({ "secret": secret, "players": &self.ucids });
        let bytes = serde_json::to_vec_pretty(&doc).map_err(io::Error::other)?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = file.with_extension("json.tmp");
        write_private(&tmp, &bytes)?;
        std::fs::rename(&tmp, &file)?;
        self.dirty = false;
        Ok(())
    }

    /// The secret, for the server-scoped store.
    pub fn secret(&self) -> [u8; 32] {
        self.secret
    }

    /// The map from key to ucid, for the server-scoped store.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.ucids.iter().map(|(k, u)| (k.as_str(), u.as_str()))
    }

    /// Restore a pair read from the store.
    pub fn restore(&mut self, key: &str, ucid: &str) {
        self.ucids.insert(key.to_owned(), ucid.to_owned());
    }

    /// The key of a ucid: 32 lower-case hex digits.
    pub fn key_of(&self, ucid: &str) -> String {
        let mac = hmac_sha256(&self.secret, ucid.as_bytes());
        mac[..16].iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The key of a ucid, remembering which ucid it came from.
    pub fn learn(&mut self, ucid: &str) -> String {
        let key = self.key_of(ucid);
        if !self.ucids.contains_key(&key) {
            self.ucids.insert(key.clone(), ucid.to_owned());
            self.dirty = true;
        }
        key
    }

    /// The ucid a key came from, if this server saw it.
    pub fn ucid_of(&self, key: &str) -> Option<&str> {
        self.ucids.get(key).map(String::as_str)
    }
}

/// Write a file only its owner can read.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // A temporary file left by an earlier run keeps its old mode.
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(bytes)?;
    f.sync_all()
}

/// HMAC-SHA-256 (RFC 2104).
fn hmac_sha256(key: &[u8; 32], msg: &[u8]) -> [u8; 32] {
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for (i, k) in key.iter().enumerate() {
        ipad[i] ^= k;
        opad[i] ^= k;
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

/// A name a player used and when it was first seen (seconds since the Unix
/// epoch).
#[derive(Clone, Debug, PartialEq)]
pub struct NameSeen {
    /// The name.
    pub name: String,
    /// When it was first seen.
    pub first_seen: f64,
}

/// One player, by key.
#[derive(Clone, Debug, Default)]
pub struct PlayerRec {
    /// The player key.
    pub key: String,
    /// Every name seen, oldest first.
    pub names: Vec<NameSeen>,
    /// Whether the player is connected.
    pub connected: bool,
    /// The DCS connection ID while connected.
    pub conn: u32,
    /// The DCS coalition number, 0 for none.
    pub side: u32,
    /// The slot ID, empty for spectators.
    pub slot: String,
    /// Fuel used by campaign, in fractions of a full tank, from the vitals
    /// samples of the units the player occupied.
    pub fuel_used: BTreeMap<u64, f64>,
}

impl PlayerRec {
    /// The name last seen.
    pub fn name(&self) -> &str {
        self.names.last().map_or("", |n| n.name.as_str())
    }
}

/// One seat in one unit and how long a player held it.
#[derive(Clone, Debug, PartialEq)]
pub struct Occupancy {
    /// The player key.
    pub key: String,
    /// The DCS object ID. Internal: the API never shows it.
    pub object: u64,
    /// The unit's `EntityId`, 0 when the binding holds none.
    pub unit: u64,
    /// The unit's group `EntityId`, 0 when the binding holds none.
    pub group: u64,
    /// The seat, 0 for the first.
    pub seat: u32,
    /// The player's DCS coalition number.
    pub side: u32,
    /// The campaign the join was stamped with, 0 for none.
    pub campaign: u64,
    /// When the player took the seat (seconds since the Unix epoch).
    pub from: f64,
    /// When the player left it, absent while held.
    pub to: Option<f64>,
}

/// What the roster saw, for the API's player events.
#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    /// A player connected.
    Connect {
        /// The player key.
        key: String,
        /// The name.
        name: String,
        /// The DCS coalition number.
        side: u32,
    },
    /// A player disconnected.
    Disconnect {
        /// The player key.
        key: String,
        /// The name.
        name: String,
        /// The DCS coalition number.
        side: u32,
    },
    /// A player took a seat in a unit.
    Enter {
        /// The player key.
        key: String,
        /// The name.
        name: String,
        /// The DCS coalition number.
        side: u32,
        /// The unit's `EntityId`, 0 for none.
        unit: u64,
        /// The seat.
        seat: u32,
    },
    /// A player left a seat.
    Leave {
        /// The player key.
        key: String,
        /// The name.
        name: String,
        /// The DCS coalition number.
        side: u32,
        /// The unit's `EntityId`, 0 for none.
        unit: u64,
        /// The seat.
        seat: u32,
    },
    /// A player sent a chat line.
    Chat {
        /// The player key.
        key: String,
        /// The name.
        name: String,
        /// The DCS coalition number.
        side: u32,
        /// The message.
        text: String,
    },
    /// A player's name differs from the last one seen.
    Rename {
        /// The player key.
        key: String,
        /// The new name.
        name: String,
        /// The DCS coalition number.
        side: u32,
        /// The name before.
        from: String,
    },
}

/// What the roster reads from outside it while it folds a record.
pub struct Ctx<'a> {
    /// Wall-clock seconds since the Unix epoch.
    pub now: f64,
    /// The unit and group `EntityId`s a DCS object ID is bound to.
    pub bound: &'a dyn Fn(u64) -> Option<(u64, u64)>,
    /// Gives an unmanaged unit an `EntityId` for it and its group, and
    /// returns them. `None` when the caller adopts nothing.
    pub adopt: Option<&'a Adopt<'a>>,
}

/// Adopts an unmanaged unit: its unit and group `EntityId`s.
pub type Adopt<'a> = dyn Fn(&Unmanaged) -> Option<(u64, u64)> + 'a;

/// An unmanaged unit a player occupies, as its `OBJECT` record gave it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unmanaged {
    /// The DCS object ID.
    pub object: u64,
    /// The DCS group ID.
    pub group: u64,
    /// The `EntityId` the agent's binding holds for the unit, 0 for none.
    pub entity: u64,
    /// The `EntityId` the agent's binding holds for the group, 0 for none.
    pub group_entity: u64,
}

/// A held slot: attaches to every birth with its mission ID.
#[derive(Clone, Debug)]
struct Hold {
    base: String,
    seat: u32,
    matched: bool,
}

/// Counters of the occupancy join.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Slots that ended without matching any birth.
    pub unmatched: u64,
    /// Occupancies of a unit the binding holds no `EntityId` for.
    pub unbound: u64,
    /// `PLAYERS` answers applied.
    pub resyncs: u64,
}

/// Who is on the server and where.
#[derive(Debug)]
pub struct Roster {
    keys: Keyring,
    players: BTreeMap<String, PlayerRec>,
    conns: BTreeMap<u32, String>,
    holds: BTreeMap<String, Hold>,
    /// The object of the latest birth, by mission ID.
    births: BTreeMap<String, u64>,
    open: Vec<Occupancy>,
    /// The last fuel sample of each occupied object.
    fuel_last: BTreeMap<u64, f32>,
    history: VecDeque<Occupancy>,
    boot: u64,
    gen_: u32,
    campaign: u64,
    resync: bool,
    counts: Counts,
    /// The table, the rows that left, and the weapons and BDAs seen.
    mem: Memory,
    /// Model time of the last `FRAME`.
    model_time: f64,
    /// The records after the last `FRAME` belong to the current generation.
    current: bool,
}

impl Roster {
    /// A roster over a keyring.
    pub fn new(keys: Keyring) -> Roster {
        Roster {
            keys,
            players: BTreeMap::new(),
            conns: BTreeMap::new(),
            holds: BTreeMap::new(),
            births: BTreeMap::new(),
            open: Vec::new(),
            fuel_last: BTreeMap::new(),
            history: VecDeque::new(),
            boot: 0,
            gen_: 0,
            campaign: 0,
            resync: false,
            counts: Counts::default(),
            mem: Memory::default(),
            model_time: 0.0,
            current: true,
        }
    }

    /// The keyring, for the server-scoped store.
    pub fn keyring(&self) -> &Keyring {
        &self.keys
    }

    /// The key of a ucid, remembering the ucid. For an admin's raw ucid and
    /// for a ban list entry.
    pub fn learn(&mut self, ucid: &str) -> String {
        self.keys.learn(ucid)
    }

    /// Write the keyring if it changed. A failed write is tried again at
    /// the next call.
    pub fn flush_keys(&mut self) -> io::Result<()> {
        self.keys.flush()
    }

    /// The ucid of a key: for the one caller that writes a `MODERATE`.
    pub fn ucid_of(&self, key: &str) -> Option<&str> {
        self.keys.ucid_of(key)
    }

    /// The DCS connection ID of a connected player.
    pub fn conn_of(&self, key: &str) -> Option<u32> {
        self.players
            .get(key)
            .filter(|p| p.connected)
            .map(|p| p.conn)
    }

    /// The key of the player on a DCS connection ID.
    pub fn key_on(&self, conn: u32) -> Option<&str> {
        self.conns.get(&conn).map(String::as_str)
    }

    /// Whether the server has seen this key.
    pub fn knows(&self, key: &str) -> bool {
        self.players.contains_key(key) || self.keys.ucids.contains_key(key)
    }

    /// One player.
    pub fn player(&self, key: &str) -> Option<&PlayerRec> {
        self.players.get(key)
    }

    /// Every player seen, by key.
    pub fn players(&self) -> impl Iterator<Item = &PlayerRec> {
        self.players.values()
    }

    /// The counters of the join.
    pub fn counts(&self) -> Counts {
        self.counts
    }

    /// Slots held now that no birth matched yet.
    pub fn held_unmatched(&self) -> usize {
        self.holds.values().filter(|h| !h.matched).count()
    }

    /// Whether a `PLAYERS` query is owed, and clear the flag.
    pub fn take_resync(&mut self) -> bool {
        std::mem::take(&mut self.resync)
    }

    /// Ask for a `PLAYERS` query again, as when one could not be sent.
    pub fn want_resync(&mut self) {
        self.resync = true;
    }

    /// Whether a `PLAYERS` query is owed.
    pub fn resync_wanted(&self) -> bool {
        self.resync
    }

    /// The units that players occupy and the binding holds, for `SUBSCRIBE`.
    pub fn occupied_units(&self) -> BTreeSet<EntityId> {
        self.open
            .iter()
            .filter(|o| o.unit != 0)
            .map(|o| EntityId(o.unit))
            .collect()
    }

    /// Every occupancy, finished ones first.
    pub fn occupancies(&self) -> impl Iterator<Item = &Occupancy> {
        self.history.iter().chain(self.open.iter())
    }

    /// The keys of the players in a DCS object now, by seat.
    pub fn players_of(&self, object: u64) -> Vec<String> {
        let mut seats: Vec<(u32, &str)> = self
            .open
            .iter()
            .filter(|o| o.object == object)
            .map(|o| (o.seat, o.key.as_str()))
            .collect();
        seats.sort();
        seats.into_iter().map(|(_, k)| k.to_owned()).collect()
    }

    /// The key a menu record's player is, when DCS names one the roster
    /// knows.
    pub fn selector(&self, m: &Menu) -> Option<String> {
        (m.player != 0)
            .then(|| self.conns.get(&m.player).cloned())
            .flatten()
    }

    /// The players who could have selected a menu item at this moment,
    /// by occupancy and connection (spec/records.md, "Layout: MENU"): a
    /// group item's candidates are the group's occupants, a coalition
    /// item's the coalition's connected players, an `ALL` item's every
    /// connected player.
    pub fn candidates(&self, m: &Menu) -> Vec<String> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        match m.scope {
            scope::GROUP => {
                for o in &self.open {
                    if o.group != 0 && o.group == m.target {
                        out.insert(o.key.clone());
                    }
                }
            }
            scope::COALITION => {
                for p in self.players.values() {
                    if p.connected && u64::from(p.side) == m.target {
                        out.insert(p.key.clone());
                    }
                }
            }
            _ => {
                for p in self.players.values().filter(|p| p.connected) {
                    out.insert(p.key.clone());
                }
            }
        }
        out.into_iter().collect()
    }

    /// Fold one record in. Returns the player events it makes, in order.
    pub fn fold(&mut self, r: &RawRecord, ctx: &Ctx<'_>) -> Vec<Note> {
        let mut out = Vec::new();
        let Ok(input) = Input::decode(r.kind, &r.payload) else {
            return out;
        };
        match input {
            Input::Hello(h) => {
                if self.boot != 0 && h.agent_boot != self.boot {
                    // A new DCS process holds no player of the old one.
                    self.drop_everyone(ctx, &mut out);
                    self.gen_ = 0;
                    self.births.clear();
                    self.mem.clear();
                }
                self.boot = h.agent_boot;
                self.resync = true;
            }
            Input::Frame(f) => {
                self.campaign = f.campaign;
                if f.replay == 0 && f.mission_gen != self.gen_ {
                    self.gen_ = f.mission_gen;
                    self.new_generation(ctx, &mut out);
                }
                if f.replay == 0 {
                    self.model_time = f.model_time;
                }
                // A replayed frame stamps records of an earlier generation.
                self.current = f.replay == 0 || f.mission_gen == self.gen_;
            }
            Input::Object(o) if self.current => self.object(&o, ctx),
            Input::Player(p) => self.player_record(&p, ctx, &mut out),
            Input::Event(e) if e.id == BIRTH && self.current => {
                let object = e.number(FIELD_INITIATOR).map(|n| n as u64);
                let mission = e.text(FIELD_INITIATOR_MISSION_ID);
                if let (Some(object), Some(mission)) = (object, mission)
                    && !mission.is_empty()
                {
                    self.birth(object, &mission, ctx, &mut out);
                }
            }
            Input::Event(e) if self.current => self.event(&e),
            Input::ObjectGone(g) if self.current => self.object_gone(&g, ctx, &mut out),
            Input::Vitals(rows) => {
                for v in rows {
                    self.fuel(v.object, v.fuel);
                }
            }
            Input::Result(res)
                if u64::from(res.opcode) == u64::from(opcode::PLAYERS)
                    && u64::from(res.code) == rc::APPLIED =>
            {
                self.apply_players(&res.value, ctx, &mut out);
            }
            _ => {}
        }
        out
    }

    /// A new mission generation: every object is new and DCS puts players
    /// back to spectators. The `PLAYERS` answer says who holds what.
    fn new_generation(&mut self, ctx: &Ctx<'_>, out: &mut Vec<Note>) {
        let keys: Vec<String> = self.open.iter().map(|o| o.key.clone()).collect();
        for k in keys {
            self.end_occupancy(&k, ctx, out);
        }
        for (_, h) in std::mem::take(&mut self.holds) {
            if !h.matched {
                self.counts.unmatched += 1;
            }
        }
        self.births.clear();
        self.mem.clear();
        self.resync = true;
    }

    fn drop_everyone(&mut self, ctx: &Ctx<'_>, out: &mut Vec<Note>) {
        let conns: Vec<u32> = self.conns.keys().copied().collect();
        for c in conns {
            self.disconnect(c, ctx, out);
        }
    }

    fn name_of(&self, key: &str) -> String {
        self.players
            .get(key)
            .map_or_else(String::new, |p| p.name().to_owned())
    }

    fn side_of(&self, key: &str) -> u32 {
        self.players.get(key).map_or(0, |p| p.side)
    }

    /// Record a name seen. A name that differs from the last one is a
    /// rename.
    fn see_name(&mut self, key: &str, name: &str, now: f64) -> Option<String> {
        if name.is_empty() {
            return None;
        }
        let p = self.players.get_mut(key)?;
        if p.names.last().is_some_and(|n| n.name == name) {
            return None;
        }
        let from = p.names.last().map(|n| n.name.clone());
        p.names.push(NameSeen {
            name: name.to_owned(),
            first_seen: now,
        });
        from
    }

    fn player_record(
        &mut self,
        p: &nanoprobe_transport::records::Player,
        ctx: &Ctx<'_>,
        out: &mut Vec<Note>,
    ) {
        match p.event {
            pe::CONNECT => {
                if p.client.is_empty() {
                    return;
                }
                let key = self.keys.learn(&p.client);
                self.connect(&key, p.player, p.side, &p.name, ctx, out);
                self.set_slot(&key, &p.slot, p.seat, ctx, out);
            }
            pe::DISCONNECT => {
                let conn = if p.client.is_empty() {
                    p.player
                } else {
                    let key = self.keys.key_of(&p.client);
                    self.players
                        .get(&key)
                        .filter(|r| r.connected)
                        .map_or(p.player, |r| r.conn)
                };
                self.disconnect(conn, ctx, out);
            }
            pe::SLOT => {
                let Some(key) = self.key_for(p) else {
                    return;
                };
                if !p.client.is_empty() && !self.players.get(&key).is_some_and(|r| r.connected) {
                    // A slot change from a client the roster missed joining:
                    // the join is implied.
                    self.connect(&key, p.player, p.side, &p.name, ctx, out);
                }
                if let Some(rec) = self.players.get_mut(&key) {
                    rec.side = p.side;
                }
                self.rename(&key, &p.name, ctx, out);
                self.set_slot(&key, &p.slot, p.seat, ctx, out);
            }
            pe::CHAT => {
                let Some(key) = self.key_for(p) else {
                    return;
                };
                // The record carries the name and side of a player the roster
                // does not know yet.
                let known = self.players.get(&key).is_some_and(|r| r.connected);
                out.push(Note::Chat {
                    name: if known {
                        self.name_of(&key)
                    } else {
                        p.name.clone()
                    },
                    side: if known { self.side_of(&key) } else { p.side },
                    key,
                    text: p.text.clone(),
                });
            }
            _ => {}
        }
    }

    /// The key of the player a record names: by the client when the record
    /// carries one, else by the connection.
    fn key_for(&mut self, p: &nanoprobe_transport::records::Player) -> Option<String> {
        if !p.client.is_empty() {
            return Some(self.keys.learn(&p.client));
        }
        self.conns.get(&p.player).cloned()
    }

    fn rename(&mut self, key: &str, name: &str, ctx: &Ctx<'_>, out: &mut Vec<Note>) {
        if let Some(from) = self.see_name(key, name, ctx.now) {
            out.push(Note::Rename {
                key: key.to_owned(),
                name: name.to_owned(),
                side: self.side_of(key),
                from,
            });
        }
    }

    fn connect(
        &mut self,
        key: &str,
        conn: u32,
        side: u32,
        name: &str,
        ctx: &Ctx<'_>,
        out: &mut Vec<Note>,
    ) {
        let already = self
            .players
            .get(key)
            .is_some_and(|p| p.connected && p.conn == conn);
        if let Some(old) = self
            .players
            .get(key)
            .filter(|p| p.connected)
            .map(|p| p.conn)
            && old != conn
        {
            // The same client on a new connection: the old one is gone.
            self.disconnect(old, ctx, out);
        }
        if let Some(other) = self.conns.get(&conn).cloned()
            && other != key
        {
            // The connection ID now belongs to someone else.
            self.disconnect(conn, ctx, out);
        }
        let rec = self
            .players
            .entry(key.to_owned())
            .or_insert_with(|| PlayerRec {
                key: key.to_owned(),
                ..PlayerRec::default()
            });
        rec.connected = true;
        rec.conn = conn;
        rec.side = side;
        self.conns.insert(conn, key.to_owned());
        if already {
            self.rename(key, name, ctx, out);
            return;
        }
        let from = self.see_name(key, name, ctx.now);
        out.push(Note::Connect {
            key: key.to_owned(),
            name: self.name_of(key),
            side,
        });
        if let Some(from) = from {
            out.push(Note::Rename {
                key: key.to_owned(),
                name: name.to_owned(),
                side,
                from,
            });
        }
    }

    fn disconnect(&mut self, conn: u32, ctx: &Ctx<'_>, out: &mut Vec<Note>) {
        let Some(key) = self.conns.remove(&conn) else {
            return;
        };
        self.end_occupancy(&key, ctx, out);
        self.end_hold(&key);
        let name = self.name_of(&key);
        let side = self.side_of(&key);
        if let Some(p) = self.players.get_mut(&key) {
            p.connected = false;
            p.conn = 0;
            p.slot.clear();
        }
        out.push(Note::Disconnect { key, name, side });
    }

    fn end_hold(&mut self, key: &str) {
        if let Some(h) = self.holds.remove(key)
            && !h.matched
        {
            self.counts.unmatched += 1;
        }
    }

    /// The player's slot is now `slot`: the old occupancy and hold end, the
    /// new hold joins a birth already seen with its mission ID.
    fn set_slot(&mut self, key: &str, slot: &str, seat: u32, ctx: &Ctx<'_>, out: &mut Vec<Note>) {
        let same = self.players.get(key).is_some_and(|p| p.slot == slot)
            && self.holds.contains_key(key) == !slot.is_empty();
        if same {
            return;
        }
        self.end_occupancy(key, ctx, out);
        self.end_hold(key);
        if let Some(p) = self.players.get_mut(key) {
            p.slot = slot.to_owned();
        }
        if slot.is_empty() {
            return;
        }
        let base = base_slot(slot).to_owned();
        let seat = if seat == 0 { seat_of(slot) } else { seat };
        let object = self.births.get(&base).copied();
        self.holds.insert(
            key.to_owned(),
            Hold {
                base,
                seat,
                matched: false,
            },
        );
        if let Some(object) = object {
            self.occupy(key, object, ctx, out);
        }
    }

    fn occupy(&mut self, key: &str, object: u64, ctx: &Ctx<'_>, out: &mut Vec<Note>) {
        let Some(h) = self.holds.get_mut(key) else {
            return;
        };
        h.matched = true;
        let seat = h.seat;
        if self
            .open
            .iter()
            .any(|o| o.key == key && o.object == object && o.seat == seat)
        {
            return;
        }
        // A respawn into the held slot replaces the occupancy before it.
        self.end_occupancy(key, ctx, out);
        let (unit, group) = self.unit_of(object, ctx);
        if unit == 0 {
            self.counts.unbound += 1;
        }
        let side = self.side_of(key);
        self.open.push(Occupancy {
            key: key.to_owned(),
            object,
            unit,
            group,
            seat,
            side,
            campaign: self.campaign,
            from: ctx.now,
            to: None,
        });
        out.push(Note::Enter {
            key: key.to_owned(),
            name: self.name_of(key),
            side,
            unit,
            seat,
        });
    }

    fn end_occupancy(&mut self, key: &str, ctx: &Ctx<'_>, out: &mut Vec<Note>) {
        let mut i = 0;
        while i < self.open.len() {
            if self.open[i].key == key {
                let mut o = self.open.remove(i);
                o.to = Some(ctx.now);
                if !self.open.iter().any(|x| x.object == o.object) {
                    self.fuel_last.remove(&o.object);
                }
                out.push(Note::Leave {
                    key: o.key.clone(),
                    name: self.name_of(&o.key),
                    side: o.side,
                    unit: o.unit,
                    seat: o.seat,
                });
                self.history.push_back(o);
                while self.history.len() > HISTORY_KEPT {
                    self.history.pop_front();
                }
            } else {
                i += 1;
            }
        }
    }

    fn birth(&mut self, object: u64, mission: &str, ctx: &Ctx<'_>, out: &mut Vec<Note>) {
        self.births.insert(mission.to_owned(), object);
        let holders: Vec<String> = self
            .holds
            .iter()
            .filter(|(_, h)| h.base == mission)
            .map(|(k, _)| k.clone())
            .collect();
        for k in holders {
            self.occupy(&k, object, ctx, out);
        }
    }

    /// The unit and group `EntityId`s of an object: its binding, else the
    /// adoption of an unmanaged unit.
    fn unit_of(&self, object: u64, ctx: &Ctx<'_>) -> (u64, u64) {
        if let Some(b) = (ctx.bound)(object)
            && b.0 != 0
        {
            return b;
        }
        let (Some(adopt), Some(row)) = (ctx.adopt, self.mem.rows.get(&object)) else {
            return (0, 0);
        };
        if row.platform || row.kind != 1 {
            return (0, 0);
        }
        adopt(&Unmanaged {
            object,
            group: row.group,
            entity: row.entity,
            group_entity: row.group_entity,
        })
        .unwrap_or((0, 0))
    }

    /// An `OBJECT` record: the table row. An occupied unit that had no row
    /// when its player joined gets its `EntityId`s now.
    fn object(&mut self, o: &nanoprobe_transport::records::Object, ctx: &Ctx<'_>) {
        self.mem.rows.insert(o.object, Row::of(o));
        let waiting: Vec<usize> = self
            .open
            .iter()
            .enumerate()
            .filter(|(_, x)| x.object == o.object && x.unit == 0)
            .map(|(i, _)| i)
            .collect();
        if waiting.is_empty() {
            return;
        }
        let (unit, group) = self.unit_of(o.object, ctx);
        if unit != 0 {
            for i in waiting {
                self.open[i].unit = unit;
                self.open[i].group = group;
            }
        }
    }

    /// The players in an object now, else the ones in it when it left.
    fn keys_of(&self, object: u64) -> Vec<String> {
        let now = self.players_of(object);
        if now.is_empty() {
            self.mem
                .departed(object)
                .map(|d| d.keys.clone())
                .unwrap_or_default()
        } else {
            now
        }
    }

    /// An event that changes what attribution needs: a shot remembers its
    /// weapon's shooter, a BDA the time of the hit that killed.
    fn event(&mut self, e: &nanoprobe_transport::records::Event) {
        match e.id {
            attribution::SHOT => {
                let (shooter, weapon) = (
                    attribution::object_of(e, field::INITIATOR),
                    attribution::object_of(e, field::WEAPON),
                );
                if let (Some(shooter), Some(weapon)) = (shooter, weapon) {
                    let keys = self.keys_of(shooter);
                    self.mem.shot(weapon, keys);
                }
            }
            attribution::BDA => {
                if let (Some(target), Some(time)) = (
                    attribution::object_of(e, field::TARGET),
                    e.number(field::TIME).filter(|t| t.is_finite()),
                ) {
                    self.mem.bda(target, time);
                }
            }
            _ => {}
        }
    }

    /// What the roster knows of an event's objects: players (also of rows
    /// that left), type names, the date of a kill and the players who
    /// launched the weapon (spec/world.md, "Kill attribution" and "Launch
    /// attribution").
    pub fn attribute(&self, e: &nanoprobe_transport::records::Event) -> Attribution {
        let initiator = attribution::object_of(e, field::INITIATOR);
        let target = attribution::object_of(e, field::TARGET);
        let keys = |o: Option<u64>| o.map(|o| self.keys_of(o)).unwrap_or_default();
        let type_of = |o: Option<u64>| {
            let o = o?;
            let t = match self.mem.rows.get(&o) {
                Some(r) => &r.type_name,
                None => &self.mem.departed(o)?.type_name,
            };
            (!t.is_empty()).then(|| t.clone())
        };
        let entity_of =
            |o: Option<u64>| self.mem.departed(o?).map(|d| d.entity).filter(|e| *e != 0);
        let mut a = Attribution {
            initiator_players: keys(initiator),
            target_players: keys(target),
            initiator_type: type_of(initiator),
            target_type: type_of(target),
            initiator_entity: entity_of(initiator),
            target_entity: entity_of(target),
            ..Attribution::default()
        };
        let victim = match e.id {
            attribution::KILL => target,
            attribution::DEAD => initiator,
            _ => None,
        };
        a.dated_time = victim.and_then(|v| self.mem.dated(v));
        if matches!(e.id, attribution::HIT | attribution::KILL) {
            let weapon = attribution::object_of(e, field::WEAPON).and_then(|w| self.mem.weapon(w));
            match (weapon, initiator) {
                (Some(keys), _) => {
                    a.launch_players = keys.clone();
                    a.launch_rule = Some(LaunchRule::Weapon);
                }
                (None, Some(_)) => {
                    a.launch_players = a.initiator_players.clone();
                    a.launch_rule = Some(LaunchRule::Initiator);
                }
                (None, None) => {}
            }
        }
        a
    }

    fn object_gone(
        &mut self,
        g: &nanoprobe_transport::records::ObjectGone,
        ctx: &Ctx<'_>,
        out: &mut Vec<Note>,
    ) {
        let object = g.object;
        let keys = self.players_of(object);
        let entity = if g.entity != 0 {
            g.entity
        } else {
            (ctx.bound)(object).map_or(0, |b| b.0)
        };
        self.mem.leave(object, entity, keys, self.model_time);
        let keys: Vec<String> = self
            .open
            .iter()
            .filter(|o| o.object == object)
            .map(|o| o.key.clone())
            .collect();
        for k in keys {
            self.end_occupancy(&k, ctx, out);
        }
        self.births.retain(|_, o| *o != object);
    }

    /// A vitals row: the fuel an occupied unit lost since the last row is
    /// the pilot's (the occupant of the lowest seat). A rise is a refuel and
    /// counts for nothing.
    fn fuel(&mut self, object: u64, fuel: f32) {
        let Some(pilot) = self
            .open
            .iter()
            .filter(|o| o.object == object)
            .min_by_key(|o| o.seat)
            .map(|o| o.key.clone())
        else {
            return;
        };
        if !fuel.is_finite() {
            return;
        }
        if let Some(prev) = self.fuel_last.insert(object, fuel)
            && fuel < prev
            && let Some(p) = self.players.get_mut(&pilot)
        {
            *p.fuel_used.entry(self.campaign).or_default() += f64::from(prev - fuel);
        }
    }

    /// A `PLAYERS` answer: the truth of who is connected and where.
    fn apply_players(&mut self, value: &Lv, ctx: &Ctx<'_>, out: &mut Vec<Note>) {
        self.counts.resyncs += 1;
        let mut present: BTreeSet<u32> = BTreeSet::new();
        for row in lv::items(value) {
            let (Some(id), Some(ucid)) = (lv::num_at(row, "id"), lv::str_at(row, "ucid")) else {
                continue;
            };
            let conn = id as u32;
            if conn == 0 || ucid.is_empty() {
                continue;
            }
            present.insert(conn);
            let key = self.keys.learn(&ucid);
            let side = lv::num_at(row, "side").map_or(0, |n| n as u32);
            let name = lv::str_at(row, "name").unwrap_or_default();
            // DCS gives a slot as text or as a number; a number is the slot
            // of an unsuffixed seat.
            let slot = match lv::get(row, "slot") {
                Some(Lv::Number(n)) => format!("{n:.0}"),
                _ => lv::str_at(row, "slot").unwrap_or_default(),
            };
            let known = self
                .players
                .get(&key)
                .is_some_and(|p| p.connected && p.conn == conn);
            if known {
                if let Some(rec) = self.players.get_mut(&key) {
                    rec.side = side;
                }
            } else {
                self.connect(&key, conn, side, &name, ctx, out);
            }
            self.rename(&key, &name, ctx, out);
            let seat = seat_of(&slot);
            self.set_slot(&key, &slot, seat, ctx, out);
        }
        let gone: Vec<u32> = self
            .conns
            .keys()
            .filter(|c| !present.contains(c))
            .copied()
            .collect();
        for c in gone {
            self.disconnect(c, ctx, out);
        }
    }

    /// The fuel a player used in a campaign, in fractions of a full tank.
    pub fn fuel_used(&self, key: &str, campaign: u64) -> f64 {
        self.players
            .get(key)
            .and_then(|p| p.fuel_used.get(&campaign))
            .copied()
            .unwrap_or(0.0)
    }
}

/// The slot ID without its `_N` seat suffix.
pub fn base_slot(slot: &str) -> &str {
    match slot.rsplit_once('_') {
        Some((b, n)) if !b.is_empty() && !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()) => {
            b
        }
        _ => slot,
    }
}

/// The seat a slot ID names: N - 1 for a `_N` suffix, else 0.
pub fn seat_of(slot: &str) -> u32 {
    match slot.rsplit_once('_') {
        Some((b, n)) if !b.is_empty() && !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()) => {
            n.parse::<u32>().unwrap_or(1).saturating_sub(1)
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use attribution::{BDA, DEAD, HIT, KILL, SHOT};
    use nanoprobe_transport::records::{Event, Frame, Hello, Player};

    fn ring() -> Keyring {
        Keyring::from_secret(std::array::from_fn(|i| i as u8))
    }

    #[test]
    fn a_key_is_hmac_sha_256_of_the_ucid_cut_to_16_bytes() {
        // Computed with Python's hmac and hashlib.
        assert_eq!(ring().key_of("ucid-a"), "a561c2129b5770c002b63bb5edfed6c8");
        assert_eq!(ring().key_of("ucid-b"), "a523f63baa0efaf56795635c4493862e");
        let other = Keyring::from_secret([7; 32]);
        assert_eq!(other.key_of("ucid-a"), "100620224910f614faf381c03d076fcb");
    }

    #[test]
    fn a_new_secret_is_random_and_the_debug_text_hides_it() {
        let (a, b) = (Keyring::generate(), Keyring::generate());
        assert_ne!(a.secret(), b.secret());
        assert_ne!(a.key_of("u"), b.key_of("u"));
        let mut r = ring();
        r.learn("ucid-a");
        let ring_text = format!("{r:?}");
        let text = format!("{ring_text} {:?}", Roster::new(r));
        assert!(
            !text.contains("ucid-a") && !text.contains("a561c212"),
            "{text}"
        );
    }

    #[test]
    fn slot_ids_split_into_a_base_and_a_seat() {
        assert_eq!((base_slot("123"), seat_of("123")), ("123", 0));
        assert_eq!((base_slot("123_2"), seat_of("123_2")), ("123", 1));
        assert_eq!((base_slot("_2"), seat_of("_2")), ("_2", 0));
        assert_eq!((base_slot("a_b"), seat_of("a_b")), ("a_b", 0));
    }

    fn rec(i: Input) -> RawRecord {
        let (kind, payload) = i.encode();
        RawRecord {
            agent_boot: 1,
            number: 0,
            kind,
            payload,
            resync: false,
        }
    }

    fn player(event: u32, id: u32, ucid: &str, name: &str, slot: &str) -> RawRecord {
        rec(Input::Player(Player {
            event,
            player: id,
            side: 2,
            seat: 0,
            client: ucid.into(),
            name: name.into(),
            slot: slot.into(),
            text: String::new(),
        }))
    }

    fn birth(object: u64, mission: &str) -> RawRecord {
        rec(Input::Event(Event {
            id: BIRTH,
            fields: vec![
                (FIELD_INITIATOR as u16, Lv::Number(object as f64)),
                (FIELD_INITIATOR_MISSION_ID as u16, lv::s(mission)),
            ],
        }))
    }

    fn run(r: &mut Roster, records: &[RawRecord]) -> Vec<Note> {
        let bound = |o: u64| Some((o + 100, o + 200));
        let ctx = Ctx {
            now: 10.0,
            bound: &bound,
            adopt: None,
        };
        records.iter().flat_map(|x| r.fold(x, &ctx)).collect()
    }

    #[test]
    fn a_birth_before_the_slot_and_a_slot_before_the_birth_both_join() {
        for slot_first in [false, true] {
            let mut r = Roster::new(ring());
            run(&mut r, &[player(pe::CONNECT, 2, "ucid-a", "Ann", "")]);
            let slot = player(pe::SLOT, 2, "ucid-a", "Ann", "77");
            let notes = if slot_first {
                let mut n = run(&mut r, &[slot]);
                n.extend(run(&mut r, &[birth(5, "77")]));
                n
            } else {
                let mut n = run(&mut r, &[birth(5, "77")]);
                n.extend(run(&mut r, &[slot]));
                n
            };
            assert!(
                matches!(
                    &notes[..],
                    [Note::Enter {
                        unit: 105,
                        seat: 0,
                        ..
                    }]
                ),
                "{notes:?}"
            );
            assert_eq!(r.players_of(5), vec![r.keys.key_of("ucid-a")]);
            assert_eq!(r.occupied_units().len(), 1);
            assert_eq!(r.counts().unmatched, 0);
        }
    }

    #[test]
    fn a_held_slot_follows_a_respawn_and_ends_with_a_slot_change() {
        let mut r = Roster::new(ring());
        run(&mut r, &[player(pe::CONNECT, 2, "ucid-a", "Ann", "77")]);
        run(&mut r, &[birth(5, "77")]);
        // The respawn: a new object with the same mission ID.
        let n = run(&mut r, &[birth(9, "77")]);
        assert!(
            matches!(&n[..], [Note::Leave { .. }, Note::Enter { .. }]),
            "{n:?}"
        );
        assert!(r.players_of(5).is_empty());
        assert_eq!(r.players_of(9).len(), 1);
        // The old object leaving changes nothing.
        let n = run(
            &mut r,
            &[rec(Input::ObjectGone(
                nanoprobe_transport::records::ObjectGone {
                    object: 5,
                    ..Default::default()
                },
            ))],
        );
        assert!(n.is_empty());
        // A slot change ends the seat.
        let n = run(&mut r, &[player(pe::SLOT, 2, "ucid-a", "Ann", "")]);
        assert!(matches!(&n[..], [Note::Leave { .. }]));
        assert!(r.players_of(9).is_empty());
        assert_eq!(r.counts().unmatched, 0);
    }

    #[test]
    fn a_slot_with_no_birth_is_counted_when_it_ends_and_never_guessed() {
        let mut r = Roster::new(ring());
        run(&mut r, &[player(pe::CONNECT, 2, "ucid-a", "Ann", "77")]);
        assert_eq!(r.held_unmatched(), 1);
        // A birth that carries no mission ID cannot match.
        let none = rec(Input::Event(Event {
            id: BIRTH,
            fields: vec![(FIELD_INITIATOR as u16, Lv::Number(5.0))],
        }));
        assert!(run(&mut r, &[none]).is_empty());
        assert!(r.occupied_units().is_empty());
        run(&mut r, &[player(pe::DISCONNECT, 2, "ucid-a", "Ann", "")]);
        assert_eq!(r.counts().unmatched, 1);
        assert_eq!(r.held_unmatched(), 0);
    }

    #[test]
    fn crew_seats_join_to_one_unit_and_the_pilot_gets_the_fuel() {
        let mut r = Roster::new(ring());
        run(&mut r, &[player(pe::CONNECT, 2, "ucid-a", "Ann", "10")]);
        run(&mut r, &[player(pe::CONNECT, 3, "ucid-b", "Bo", "10_2")]);
        run(&mut r, &[birth(5, "10")]);
        let keys = r.players_of(5);
        assert_eq!(keys, vec![r.keys.key_of("ucid-a"), r.keys.key_of("ucid-b")]);
        let rows = |fuel: f32| {
            rec(Input::Vitals(vec![crate::sim::wire::Vitals {
                object: 5,
                life: 1.0,
                fuel,
            }]))
        };
        run(&mut r, &[rows(0.9), rows(0.8), rows(0.95), rows(0.85)]);
        let pilot = r.fuel_used(&keys[0], 0);
        assert!((pilot - 0.2).abs() < 1e-6, "{pilot}");
        assert_eq!(r.fuel_used(&keys[1], 0), 0.0);
    }

    #[test]
    fn a_name_that_differs_is_a_rename_and_names_keep_their_first_time() {
        let mut r = Roster::new(ring());
        let n = run(&mut r, &[player(pe::CONNECT, 2, "ucid-a", "Ann", "")]);
        assert!(matches!(&n[..], [Note::Connect { .. }]));
        let n = run(&mut r, &[player(pe::SLOT, 2, "ucid-a", "Anna", "")]);
        assert!(
            matches!(&n[..], [Note::Rename { from, name, .. }] if from == "Ann" && name == "Anna")
        );
        let n = run(&mut r, &[player(pe::SLOT, 2, "ucid-a", "Anna", "")]);
        assert!(n.is_empty());
        let key = r.keys.key_of("ucid-a");
        let names = &r.player(&key).unwrap().names;
        assert_eq!(names.len(), 2);
        assert_eq!(names[0].first_seen, 10.0);
    }

    #[test]
    fn a_new_agent_boot_disconnects_everyone_and_asks_for_players() {
        let mut r = Roster::new(ring());
        run(&mut r, &[player(pe::CONNECT, 2, "ucid-a", "Ann", "")]);
        let hello = |boot| {
            rec(Input::Hello(Hello {
                agent_boot: boot,
                ..Hello::default()
            }))
        };
        assert!(run(&mut r, &[hello(1)]).is_empty());
        assert!(r.take_resync());
        let n = run(&mut r, &[hello(2)]);
        assert!(matches!(&n[..], [Note::Disconnect { .. }]));
        assert!(r.take_resync() && !r.take_resync());
        // A mission generation change asks again.
        run(
            &mut r,
            &[rec(Input::Frame(Frame {
                mission_gen: 3,
                ..Frame::default()
            }))],
        );
        assert!(r.take_resync());
    }

    fn object(object: u64, group: u64, name: &str, type_name: &str) -> RawRecord {
        rec(Input::Object(nanoprobe_transport::records::Object {
            object,
            group,
            kind: 1,
            coalition: 2,
            name: name.into(),
            type_name: type_name.into(),
            ..Default::default()
        }))
    }

    fn gone(object: u64, entity: u64) -> RawRecord {
        rec(Input::ObjectGone(
            nanoprobe_transport::records::ObjectGone {
                object,
                reason: 1,
                entity,
                group_entity: 0,
            },
        ))
    }

    fn frame(model_time: f64) -> RawRecord {
        rec(Input::Frame(Frame {
            model_time,
            ..Frame::default()
        }))
    }

    fn event(id: u32, fields: &[(u64, f64)]) -> Event {
        Event {
            id,
            fields: fields
                .iter()
                .map(|(c, v)| (*c as u16, Lv::Number(*v)))
                .collect(),
        }
    }

    fn event_rec(e: Event) -> RawRecord {
        rec(Input::Event(e))
    }

    /// A player in object 7 and a victim, object 8, both in the table.
    fn duel() -> (Roster, String) {
        let mut r = Roster::new(ring());
        run(
            &mut r,
            &[
                player(pe::CONNECT, 2, "ucid-a", "Ann", ""),
                object(7, 70, "Ann-1", "F-16C_50"),
                object(8, 80, "Bear", "Su-27"),
                player(pe::SLOT, 2, "ucid-a", "Ann", "40"),
                birth(7, "40"),
            ],
        );
        let key = r.keyring().key_of("ucid-a");
        assert_eq!(r.players_of(7), vec![key.clone()]);
        (r, key)
    }

    #[test]
    fn a_late_kill_resolves_from_the_rows_that_left_and_is_dated_from_the_bda() {
        let (mut r, key) = duel();
        run(&mut r, &[frame(90.0)]);
        // The BDA names the victim at 95 s; both rows leave at 100 s; the
        // KILL comes at 160 s.
        run(
            &mut r,
            &[event_rec(event(BDA, &[(1, 95.0), (2, 7.0), (3, 8.0)]))],
        );
        run(&mut r, &[frame(100.0), gone(8, 0), gone(7, 55)]);
        run(&mut r, &[frame(160.0)]);
        assert!(r.players_of(7).is_empty());
        let a = r.attribute(&event(KILL, &[(1, 160.0), (2, 7.0), (3, 8.0), (4, 99.0)]));
        assert_eq!(a.initiator_players, vec![key.clone()]);
        assert!(a.target_players.is_empty());
        assert_eq!(a.initiator_type.as_deref(), Some("F-16C_50"));
        assert_eq!(a.target_type.as_deref(), Some("Su-27"));
        assert_eq!(a.initiator_entity, Some(55));
        assert_eq!(a.dated_time, Some(95.0));
        // The SCORE names only the killer: no victim, no date, still the
        // killer's occupants.
        let sc = r.attribute(&event(29, &[(1, 160.0), (2, 7.0)]));
        assert_eq!(sc.initiator_players, vec![key]);
        assert_eq!(sc.dated_time, None);
    }

    #[test]
    fn a_late_kill_without_a_bda_is_dated_from_the_removal() {
        let (mut r, _) = duel();
        run(&mut r, &[frame(100.0), gone(8, 0)]);
        run(&mut r, &[frame(150.0)]);
        // The late DEAD of the victim, and a second OBJECT_GONE for it.
        run(&mut r, &[gone(8, 0)]);
        let dead = r.attribute(&event(DEAD, &[(1, 150.0), (2, 8.0)]));
        assert_eq!(dead.dated_time, Some(100.0));
        assert_eq!(dead.initiator_type.as_deref(), Some("Su-27"));
        // A kill of a row still in the table and no BDA has no date.
        let live = r.attribute(&event(KILL, &[(2, 7.0), (3, 7.0)]));
        assert_eq!(live.dated_time, None);
    }

    #[test]
    fn launch_players_follow_the_weapon_else_the_initiator() {
        let (mut r, key) = duel();
        run(&mut r, &[event_rec(event(SHOT, &[(2, 7.0), (4, 99.0)]))]);
        // The shooter leaves before the hit.
        run(&mut r, &[frame(10.0), gone(7, 0)]);
        let hit = r.attribute(&event(HIT, &[(2, 7.0), (3, 8.0), (4, 99.0)]));
        assert_eq!(hit.launch_rule, Some(LaunchRule::Weapon));
        assert_eq!(hit.launch_players, vec![key.clone()]);
        assert_eq!(hit.initiator_players, vec![key.clone()]);
        // A gun's hit names no weapon.
        let gun = r.attribute(&event(HIT, &[(2, 7.0), (3, 8.0)]));
        assert_eq!(gun.launch_rule, Some(LaunchRule::Initiator));
        assert_eq!(gun.launch_players, vec![key.clone()]);
        // A weapon never seen shot falls back too.
        let unseen = r.attribute(&event(KILL, &[(2, 7.0), (3, 8.0), (4, 5.0)]));
        assert_eq!(unseen.launch_rule, Some(LaunchRule::Initiator));
        assert_eq!(unseen.launch_players, vec![key]);
        // Other events get no launch attribution.
        let other = r.attribute(&event(DEAD, &[(2, 7.0)]));
        assert_eq!(other.launch_rule, None);
        assert!(other.launch_players.is_empty());
    }

    #[test]
    fn a_new_generation_forgets_the_memory() {
        let (mut r, _) = duel();
        run(&mut r, &[frame(1.0), gone(7, 0)]);
        assert!(r.attribute(&event(DEAD, &[(2, 7.0)])).dated_time.is_some());
        run(
            &mut r,
            &[rec(Input::Frame(Frame {
                mission_gen: 1,
                ..Frame::default()
            }))],
        );
        let a = r.attribute(&event(DEAD, &[(2, 7.0)]));
        assert_eq!((a.dated_time, a.initiator_type), (None, None));
    }

    #[test]
    fn an_unmanaged_unit_is_adopted_once_and_a_platform_name_is_not() {
        let calls = std::cell::RefCell::new(Vec::new());
        let adopt = |u: &Unmanaged| {
            calls.borrow_mut().push(*u);
            Some((900 + u.object, 950))
        };
        let bound = |_: u64| None;
        let ctx = Ctx {
            now: 1.0,
            bound: &bound,
            adopt: Some(&adopt),
        };
        let mut r = Roster::new(ring());
        let managed = crate::ids::name(
            crate::ids::NameKind::Unit,
            crate::ids::CampaignId(5),
            EntityId(3),
        );
        let notes: Vec<Note> = [
            player(pe::CONNECT, 2, "ucid-a", "Ann", ""),
            player(pe::CONNECT, 3, "ucid-b", "Bo", ""),
            object(7, 70, "Ann-1", "F-16C_50"),
            object(8, 80, &managed, "F-16C_50"),
            player(pe::SLOT, 2, "ucid-a", "Ann", "40"),
            birth(7, "40"),
            player(pe::SLOT, 3, "ucid-b", "Bo", "41"),
            birth(8, "41"),
        ]
        .iter()
        .flat_map(|x| r.fold(x, &ctx))
        .collect();
        let enters: Vec<u64> = notes
            .iter()
            .filter_map(|n| match n {
                Note::Enter { unit, .. } => Some(*unit),
                _ => None,
            })
            .collect();
        assert_eq!(enters, vec![907, 0]);
        assert_eq!(calls.borrow().len(), 1);
        assert_eq!(calls.borrow()[0].group, 70);
        assert_eq!(r.occupied_units(), BTreeSet::from([EntityId(907)]));
        // A group scope names the player's group.
        let m = Menu {
            scope: scope::GROUP,
            target: 950,
            ..Menu::default()
        };
        assert_eq!(r.candidates(&m), vec![r.keyring().key_of("ucid-a")]);
    }

    #[test]
    fn an_object_record_after_the_join_completes_the_occupancy() {
        let adopt = |u: &Unmanaged| Some((900 + u.object, 950));
        let bound = |_: u64| None;
        let ctx = Ctx {
            now: 1.0,
            bound: &bound,
            adopt: Some(&adopt),
        };
        let mut r = Roster::new(ring());
        for x in [
            player(pe::CONNECT, 2, "ucid-a", "Ann", ""),
            player(pe::SLOT, 2, "ucid-a", "Ann", "40"),
            birth(7, "40"),
        ] {
            r.fold(&x, &ctx);
        }
        assert!(r.occupied_units().is_empty());
        r.fold(&object(7, 70, "Ann-1", "F-16C_50"), &ctx);
        assert_eq!(r.occupied_units(), BTreeSet::from([EntityId(907)]));
    }

    fn scratch(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!("nanoprobe-{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn the_keyring_file_round_trips_with_mode_0600_and_keeps_keys_stable() {
        let dir = scratch("keyring");
        let mut a = Keyring::open(&dir).expect("opens");
        let key = a.learn("ucid-a");
        a.flush().expect("writes");
        let file = dir.join(KEYRING_FILE);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).expect("file").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(!dir.join("players.json.tmp").exists());
        let b = Keyring::open(&dir).expect("opens again");
        assert_eq!(b.key_of("ucid-a"), key);
        assert_eq!(b.ucid_of(&key), Some("ucid-a"));
        assert_eq!(b.secret(), a.secret());
        // A second flush with nothing new leaves the file as it is.
        let before = std::fs::read(&file).expect("read");
        let mut c = Keyring::open(&dir).expect("opens");
        c.flush().expect("flush");
        assert_eq!(std::fs::read(&file).expect("read"), before);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_malformed_keyring_file_is_an_error_that_names_no_content() {
        let dir = scratch("keyring-bad");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join(KEYRING_FILE), br#"{"secret":"zz-ucid-leak"}"#).expect("write");
        let e = Keyring::open(&dir).expect_err("malformed");
        assert!(!e.to_string().contains("leak"), "{e}");
        // Nothing was replaced.
        assert!(
            std::fs::read_to_string(dir.join(KEYRING_FILE))
                .expect("read")
                .contains("leak")
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    fn players_result(rows: Vec<Lv>) -> RawRecord {
        rec(Input::Result(nanoprobe_transport::records::CommandResult {
            seq: 0,
            epoch: 0,
            campaign: 0,
            entity: 0,
            rev: 0,
            object: 0,
            cost_us: 0,
            opcode: opcode::PLAYERS,
            code: rc::APPLIED as u16,
            message: String::new(),
            value: lv::list(rows),
        }))
    }

    fn row(id: f64, ucid: &str, name: &str, slot: Lv) -> Lv {
        lv::obj(vec![
            ("id", lv::n(id)),
            ("ucid", lv::s(ucid)),
            ("name", lv::s(name)),
            ("side", lv::n(2.0)),
            ("slot", slot),
        ])
    }

    #[test]
    fn a_players_answer_reads_a_numeric_slot_and_reports_a_new_name() {
        let mut r = Roster::new(ring());
        run(&mut r, &[player(pe::CONNECT, 2, "ucid-a", "Ann", "77")]);
        run(&mut r, &[birth(5, "77")]);
        let n = run(
            &mut r,
            &[players_result(vec![row(
                2.0,
                "ucid-a",
                "Anna",
                lv::n(77.0),
            )])],
        );
        assert!(
            matches!(&n[..], [Note::Rename { from, .. }] if from == "Ann"),
            "{n:?}"
        );
        // The number reads as the same slot: the seat stays held.
        assert_eq!(r.players_of(5).len(), 1);
    }

    #[test]
    fn a_chat_or_slot_from_a_client_not_yet_known_is_kept() {
        let mut r = Roster::new(ring());
        let n = run(
            &mut r,
            &[{
                let mut c = Player {
                    event: pe::CHAT,
                    player: 3,
                    side: 1,
                    client: "ucid-z".into(),
                    name: "Zed".into(),
                    ..Player::default()
                };
                c.text = "hello".into();
                rec(Input::Player(c))
            }],
        );
        assert!(
            matches!(&n[..], [Note::Chat { name, side: 1, text, .. }] if name == "Zed" && text == "hello"),
            "{n:?}"
        );
        let n = run(&mut r, &[player(pe::SLOT, 4, "ucid-y", "Yan", "9")]);
        assert!(matches!(&n[0], Note::Connect { .. }), "{n:?}");
    }

    #[test]
    fn a_birth_replayed_from_an_earlier_generation_joins_nothing() {
        let mut r = Roster::new(ring());
        let fr = |gen_: u32, replay: u32| {
            rec(Input::Frame(Frame {
                mission_gen: gen_,
                replay,
                ..Frame::default()
            }))
        };
        run(&mut r, &[fr(2, 0)]);
        run(&mut r, &[player(pe::CONNECT, 2, "ucid-a", "Ann", "77")]);
        let n = run(&mut r, &[fr(1, 1), birth(5, "77")]);
        assert!(n.is_empty(), "{n:?}");
        assert!(r.players_of(5).is_empty());
        let n = run(&mut r, &[fr(2, 1), birth(6, "77")]);
        assert!(matches!(&n[..], [Note::Enter { .. }]), "{n:?}");
    }

    #[test]
    fn with_persistence_off_no_file_is_written() {
        let mut k = Keyring::generate();
        k.learn("ucid-a");
        assert!(k.file.is_none(), "a generated keyring has no file");
        k.flush().expect("nothing to write");
        assert!(
            !Path::new(KEYRING_FILE).exists(),
            "no keyring file in the working directory"
        );
    }
}
