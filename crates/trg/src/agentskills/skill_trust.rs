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

use std::collections::BTreeSet;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::trg_config_dir;

const TRUST_FILE: &str = "trusted-skills.json";

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
        let skill = canonical(skill_dir);
        let working = canonical(working_dir);
        match skill.starts_with(&working) {
            true => Self::Own,
            false => Self::Foreign,
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
pub enum TrustRefusal {
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
) -> Result<SkillTrust, TrustRefusal> {
    if operator.is_granted() || !SkillOrigin::of(skill_dir, working_dir).needs_trust() || store.trusts(skill_dir) {
        return Ok(SkillTrust::Granted);
    }

    let dir = skill_dir.display().to_string();
    match answer(skill_dir) {
        Some(true) => {
            store.record(skill_dir);
            Ok(SkillTrust::Granted)
        }
        Some(false) => Err(TrustRefusal::Declined { dir }),
        None => Err(TrustRefusal::NothingToAsk { dir }),
    }
}

/// The gate as an actual run meets it: the operator's flag, the stored answers, and a
/// terminal to ask at when neither settles it.
pub fn admit(skill_dir: &Path, operator: SkillTrust) -> Result<SkillTrust, TrustRefusal> {
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

    eprint!(
        "Skill directory '{}' is outside this working tree. Running it lets its author decide \
         what an agent does as you. Trust it from now on? [y/N] ",
        skill_dir.display()
    );
    let _ = io::stderr().flush();

    let mut reply = String::new();
    if io::stdin().read_line(&mut reply).is_err() {
        return None;
    }
    Some(matches!(reply.trim(), "y" | "Y" | "yes" | "Yes"))
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refuse_to_answer(_: &Path) -> Option<bool> {
        panic!("nothing should have been asked")
    }

    #[test]
    fn a_skill_in_the_operators_own_tree_is_not_asked_about() {
        let tree = tempfile::tempdir().expect("temp dir");
        let skill = tree.path().join("skills/my-skill");
        std::fs::create_dir_all(&skill).expect("create skill dir");
        let mut store = TrustStore::default();

        let trust = admit_with(&skill, tree.path(), SkillTrust::Withheld, &mut store, refuse_to_answer);

        assert_eq!(trust, Ok(SkillTrust::Granted));
        assert!(!store.trusts(&skill), "an own tree is not a standing answer about it");
    }

    #[test]
    fn a_foreign_skill_the_operator_vouched_for_on_the_command_line_is_not_asked_about() {
        let foreign = tempfile::tempdir().expect("temp dir");
        let here = tempfile::tempdir().expect("temp dir");
        let mut store = TrustStore::default();

        let trust = admit_with(
            foreign.path(),
            here.path(),
            SkillTrust::Granted,
            &mut store,
            refuse_to_answer,
        );

        assert_eq!(trust, Ok(SkillTrust::Granted));
    }

    #[test]
    fn a_foreign_skill_is_asked_about_once_and_remembered() {
        let foreign = tempfile::tempdir().expect("temp dir");
        let here = tempfile::tempdir().expect("temp dir");
        let mut store = TrustStore::default();

        let first = admit_with(foreign.path(), here.path(), SkillTrust::Withheld, &mut store, |_| {
            Some(true)
        });
        let second = admit_with(
            foreign.path(),
            here.path(),
            SkillTrust::Withheld,
            &mut store,
            refuse_to_answer,
        );

        assert_eq!(first, Ok(SkillTrust::Granted));
        assert_eq!(second, Ok(SkillTrust::Granted));
    }

    #[test]
    fn a_foreign_skill_the_operator_said_no_to_does_not_run() {
        let foreign = tempfile::tempdir().expect("temp dir");
        let here = tempfile::tempdir().expect("temp dir");
        let mut store = TrustStore::default();

        let trust = admit_with(foreign.path(), here.path(), SkillTrust::Withheld, &mut store, |_| {
            Some(false)
        });

        assert!(matches!(trust, Err(TrustRefusal::Declined { .. })));
        assert!(!store.trusts(foreign.path()), "a no is not remembered as a yes");
    }

    #[test]
    fn a_foreign_skill_nobody_can_be_asked_about_is_refused_rather_than_run() {
        let foreign = tempfile::tempdir().expect("temp dir");
        let here = tempfile::tempdir().expect("temp dir");
        let mut store = TrustStore::default();

        let trust = admit_with(foreign.path(), here.path(), SkillTrust::Withheld, &mut store, |_| None);

        let Err(refusal) = trust else {
            panic!("a question nobody answered is not a yes");
        };
        assert!(
            refusal.to_string().contains("--trust-skill"),
            "the refusal has to say how to proceed: {refusal}"
        );
    }

    #[test]
    fn a_remembered_answer_survives_the_file_it_is_written_to() {
        let foreign = tempfile::tempdir().expect("temp dir");
        let home = tempfile::tempdir().expect("temp dir");
        let path = home.path().join("trg/trusted-skills.json");
        let mut store = TrustStore::default();
        store.record(foreign.path());
        store.save_to(&path).expect("save");

        assert!(TrustStore::load_from(&path).trusts(foreign.path()));
    }

    #[test]
    fn a_pass_nobody_had_to_answer_leaves_the_store_alone() {
        let foreign = tempfile::tempdir().expect("temp dir");
        let working = tempfile::tempdir().expect("temp dir");
        let mut store = TrustStore::default();

        admit_with(foreign.path(), working.path(), SkillTrust::Granted, &mut store, |_| {
            panic!("nobody should be asked")
        })
        .expect("the flag settles it");

        assert!(!store.has_an_answer_nobody_wrote_down());
    }

    #[test]
    fn a_store_nobody_has_written_yet_trusts_nothing() {
        let home = tempfile::tempdir().expect("temp dir");

        let store = TrustStore::load_from(&home.path().join("trg/trusted-skills.json"));

        assert!(!store.trusts(home.path()));
    }

    #[test]
    fn a_store_that_cannot_be_read_asks_again_rather_than_trusting_everything() {
        let home = tempfile::tempdir().expect("temp dir");
        let path = home.path().join("trusted-skills.json");
        std::fs::write(&path, "{ not json").expect("write");

        let store = TrustStore::load_from(&path);

        assert!(!store.trusts(home.path()));
    }
}
