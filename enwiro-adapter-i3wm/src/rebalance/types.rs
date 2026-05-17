// TODO: drop `#![allow(dead_code)]` once `rebalance::derive` / `rebalance::compile`
// land in step (5) of the migration — they pull in `EnvName`, `Slot`, `Env`,
// `Handle::slotted`, and `Handle::parked`. Until then these types are
// referenced only by the validation gate in `crate::tests`.
#![allow(dead_code)]

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EnvName(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Slot(pub i32);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Handle(pub String);

impl Handle {
    pub fn slotted(slot: Slot, env: &EnvName) -> Self {
        Self(format!("{}: {}", slot.0, env.0))
    }

    pub fn parked(token: u32, idx: usize) -> Self {
        Self(format!("enwiro-rebalance-{token}-{idx}"))
    }
}

#[derive(Clone, Debug)]
pub struct Env {
    pub name: EnvName,
    pub slot: Slot,
    pub score: f64,
}
