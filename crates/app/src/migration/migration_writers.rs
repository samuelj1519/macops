use crate::{
    capability::{capability_store, rules::capability_tx::Namespace},
    error::{Error, Result},
    migration::migration_fs as fs,
    policy::governance::FileProof,
};
use std::path::{Path, PathBuf};

const MASK: &[u8] = b"#!/bin/sh\nexit 75\n";
const WRITERS: [(&str, &str, bool); 3] = [
    (".local/bin/mo", "dot_local/bin/executable_mo", true),
    (".local/bin/macops", "dot_local/bin/executable_macops", true),
    (
        ".agents/bin/skillctl",
        "dot_agents/bin/executable_skillctl",
        false,
    ),
];

#[derive(Debug)]
enum State {
    Absent(Namespace),
    Masked(FileProof),
}

#[derive(Debug)]
struct Observation {
    path: PathBuf,
    mode: u32,
    state: State,
}

impl Observation {
    fn capture(path: PathBuf, mode: u32) -> std::result::Result<Self, Blocked> {
        let state = Self::read(&path, mode).map_err(|_| Blocked(path.clone()))?;
        Ok(Self { path, mode, state })
    }

    fn read(path: &Path, mode: u32) -> Result<State> {
        if let Some((bytes, proof)) = fs::bounded_snapshot(path, MASK.len() as u64)? {
            if bytes == MASK && proof.mode == mode {
                return Ok(State::Masked(proof));
            }
            return Err(blocked(path));
        }
        Ok(State::Absent(capability_store::namespace(path)?))
    }

    fn absent(&self) -> bool {
        matches!(self.state, State::Absent(_))
    }

    fn verify(&self) -> Result<()> {
        let current = Self::read(&self.path, self.mode).map_err(|_| blocked(&self.path))?;
        match (&self.state, current) {
            (State::Absent(anchor), State::Absent(_)) => {
                capability_store::open(anchor).map_err(|_| blocked(&self.path))?;
                Ok(())
            }
            (State::Masked(before), State::Masked(after)) if *before == after => Ok(()),
            _ => Err(blocked(&self.path)),
        }
    }
}

#[derive(Debug)]
pub struct Fence(Vec<Observation>);

#[derive(Debug)]
pub struct Blocked(pub PathBuf);

impl Blocked {
    pub fn error(&self) -> Error {
        blocked(&self.0)
    }
}

impl Fence {
    pub fn capture(home: &Path) -> std::result::Result<Self, Blocked> {
        let source_root = home.join(".local/share/chezmoi/home");
        let mut observations = Vec::with_capacity(WRITERS.len() * 2);
        for (live, source, require_source) in WRITERS {
            let live = Observation::capture(home.join(live), 0o700)?;
            let source = Observation::capture(source_root.join(source), 0o600)?;
            if live.absent() && !source.absent() {
                return Err(Blocked(live.path));
            }
            if !live.absent() && source.absent() && require_source {
                return Err(Blocked(source.path));
            }
            observations.extend([live, source]);
        }
        Ok(Self(observations))
    }

    pub fn verify(&self) -> Result<()> {
        self.0.iter().try_for_each(Observation::verify)
    }
}

fn blocked(path: &Path) -> Error {
    Error::attention(
        "migration_old_writer_active",
        format!(
            "旧写入口或恢复来源的停写证据缺失或已改变（{}）",
            path.display()
        ),
    )
}
