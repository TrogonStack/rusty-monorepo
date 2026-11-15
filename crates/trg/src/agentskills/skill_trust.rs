//! Whether the operator agreed to run what a skill directory tells an agent to do.
//!
//! A pass runs the directory's prompts through a harness holding the operator's own
//! credentials, so the directory decides what an agent does on their behalf. Inside their
//! own working tree that is the whole point and asking would be noise: they are running
//! `trg` there because they already trust what is there. A directory from anywhere else
//! was authored by somebody the operator may never have met, and nothing should run it on
//! the strength of a path on the command line.
//!
//! Trust is remembered per directory rather than per version of it, because a skill under
//! authoring changes on every run and a gate that fired on every edit would be answered
//! without being read.

mod constants;
use constants::TRUST_FILE;
use std::collections::BTreeSet;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::trg_config_dir;
use crate::telemetry::semconv::trg::EVAL_TRUST_ANSWER;

/// The tree the operator vouched for by running `trg` inside it.
///
/// The filesystem root is not one. A pass started there would be saying it has already
/// read every skill on the machine, which is not a statement anyone makes by changing
/// directory, and a container whose `WORKDIR` is `/` would make the gate unreachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingTree(PathBuf);

impl WorkingTree {
    pub fn parse(dir: &Path) -> Result<Self, String> {
        let dir = canonical(dir);
        if dir.parent().is_none() {
            return Err(format!(
                "'{}' is the whole filesystem rather than a tree anybody chose, so it vouches for nothing",
                dir.display()
            ));
        }
        Ok(Self(dir))
    }

    pub fn holds(&self, skill_dir: &Path) -> bool {
        canonical(skill_dir).starts_with(&self.0)
    }
}

/// Where a skill directory sits relative to the tree the operator invoked `trg` from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillOrigin {
    /// Inside the operator's own working tree, which is the tree they chose by running
    /// here.
    Own,
    /// Anywhere else, so its author is somebody other than whoever is running this.
    Foreign,
}

impl SkillOrigin {
    pub fn of(skill_dir: &Path, working_dir: &Path) -> Self {
        match WorkingTree::parse(working_dir) {
            Ok(tree) if tree.holds(skill_dir) => Self::Own,
            _ => Self::Foreign,
        }
    }

    pub fn needs_trust(self) -> bool {
        matches!(self, Self::Foreign)
    }
}

/// Whether this pass may run what the directory says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillTrust {
    #[default]
    Withheld,
    Granted,
}

impl SkillTrust {
    pub fn granted(granted: bool) -> Self {
        match granted {
            true => Self::Granted,
            false => Self::Withheld,
        }
    }

    pub fn is_granted(self) -> bool {
        matches!(self, Self::Granted)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TrustError {
    #[error(
        "skill directory '{dir}' is outside this working tree, and running it lets its author \
         decide what an agent does as you; nothing here can ask, so pass --trust-skill to say \
         you have read it"
    )]
    NothingToAsk { dir: String },
    #[error("skill directory '{dir}' was not trusted")]
    Declined { dir: String },
}

/// The directories the operator has already said yes to.
///
/// Missing, unreadable and malformed all read as "nothing is trusted yet", because the
/// only thing this file can do by failing to load is ask a question again.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustStore {
    trusted: BTreeSet<PathBuf>,
    #[serde(skip)]
    unsaved: bool,
}

impl TrustStore {
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn load() -> Self {
        Self::load_from(&store_path())
    }

    pub fn trusts(&self, skill_dir: &Path) -> bool {
        self.trusted.contains(&canonical(skill_dir))
    }

    pub fn record(&mut self, skill_dir: &Path) {
        self.unsaved |= self.trusted.insert(canonical(skill_dir));
    }

    pub fn has_an_answer_nobody_wrote_down(&self) -> bool {
        self.unsaved
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)
    }
}

pub fn store_path() -> PathBuf {
    trg_config_dir().join(TRUST_FILE)
}

/// The gate itself, with everything it touches passed in so a test can answer it.
///
/// `answer` is consulted only when nothing else has settled the question, so an operator
/// who passed the flag, or who is running their own tree, is never asked.
pub fn admit_with(
    skill_dir: &Path,
    working_dir: &Path,
    operator: SkillTrust,
    store: &mut TrustStore,
    answer: impl FnOnce(&Path) -> Option<bool>,
) -> Result<SkillTrust, TrustError> {
    if operator.is_granted() || !SkillOrigin::of(skill_dir, working_dir).needs_trust() || store.trusts(skill_dir) {
        return Ok(SkillTrust::Granted);
    }

    let dir = skill_dir.display().to_string();
    match answer(skill_dir) {
        Some(true) => {
            store.record(skill_dir);
            Ok(SkillTrust::Granted)
        }
        Some(false) => Err(TrustError::Declined { dir }),
        None => Err(TrustError::NothingToAsk { dir }),
    }
}

/// The gate as an actual run meets it: the operator's flag, the stored answers, and a
/// terminal to ask at when neither settles it.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
pub fn admit(skill_dir: &Path, operator: SkillTrust) -> Result<SkillTrust, TrustError> {
    let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut store = TrustStore::load();
    let trust = admit_with(skill_dir, &working_dir, operator, &mut store, ask_at_terminal)?;

    if store.has_an_answer_nobody_wrote_down() {
        if let Err(e) = store.save_to(&store_path()) {
            eprintln!(
                "warning: could not remember that '{}' is trusted: {e}",
                skill_dir.display()
            );
        }
    }
    Ok(trust)
}

/// `None` when there is nobody to ask, which is a refusal rather than a no: a pass that
/// cannot put the question has not had it answered.
fn ask_at_terminal(skill_dir: &Path) -> Option<bool> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return None;
    }

    let span = tracing::info_span!("await trust answer", { EVAL_TRUST_ANSWER } = tracing::field::Empty);
    let answer = span.in_scope(|| {
        eprint!(
            "Skill directory '{}' is outside this working tree. Running it lets its author decide \
             what an agent does as you. Trust it from now on? [y/N] ",
            skill_dir.display()
        );
        let _ = io::stderr().flush();

        let mut reply = String::new();
        io::stdin()
            .read_line(&mut reply)
            .ok()
            .map(|_| matches!(reply.trim(), "y" | "Y" | "yes" | "Yes"))
    });
    let recorded = match answer {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unanswered",
    };
    span.record(EVAL_TRUST_ANSWER, recorded);
    answer
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[path = "skill_trust_tests.rs"]
mod tests;
