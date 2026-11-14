//! The applied revisions the agent keeps for de-duplication
//! (spec/commands.md, "Sequence, epoch and revisions").

use std::collections::HashMap;

/// What a command's revision is compared with.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RevKey {
    /// `applied_spec_rev` of an entity.
    Spec(u64),
    /// `applied_order_rev` of an entity.
    Order(u64),
    /// The applied revision of the extension with this name.
    Extension(Vec<u8>),
}

/// Applied revisions, each 0 until a command applies one.
#[derive(Debug, Default)]
pub struct Revisions {
    spec: HashMap<u64, u64>,
    order: HashMap<u64, u64>,
    extension: HashMap<Vec<u8>, u64>,
}

impl Revisions {
    /// The applied revision for `key`.
    pub fn get(&self, key: &RevKey) -> u64 {
        match key {
            RevKey::Spec(e) => self.spec.get(e),
            RevKey::Order(e) => self.order.get(e),
            RevKey::Extension(n) => self.extension.get(n),
        }
        .copied()
        .unwrap_or(0)
    }

    /// Store `rev` as applied for `key`.
    pub fn set(&mut self, key: RevKey, rev: u64) {
        match key {
            RevKey::Spec(e) => self.spec.insert(e, rev),
            RevKey::Order(e) => self.order.insert(e, rev),
            RevKey::Extension(n) => self.extension.insert(n, rev),
        };
    }

    /// Forget an extension's revision, when it is removed.
    pub fn forget_extension(&mut self, name: &[u8]) {
        self.extension.remove(name);
    }

    /// A new mission generation: the entities' revisions go.
    pub fn new_generation(&mut self) {
        self.spec.clear();
        self.order.clear();
    }

    /// A campaign switch: every revision goes.
    pub fn new_campaign(&mut self) {
        self.new_generation();
        self.extension.clear();
    }

    /// How many revisions are kept, for tests and reports.
    pub fn len(&self) -> usize {
        self.spec.len() + self.order.len() + self.extension.len()
    }

    /// Whether none is kept.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
