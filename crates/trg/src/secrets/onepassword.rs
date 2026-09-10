//! 1Password backend, addressed through an already-authenticated `op` CLI.
//!
//! # Why we shell out to `op` instead of the Connect API or a service account
//!
//! Both of those need infrastructure or a token this backend would then have
//! to manage: a Connect server to run, or a service-account token to store
//! somewhere (which begs the question this backend exists to answer). `op`
//! sidesteps both — it's already signed in via the 1Password desktop app, the
//! same session a developer's shell already trusts, so shelling out reuses
//! that session with nothing new to authenticate or store. This mirrors why
//! [`super::keychain`] shells out to `/usr/bin/security` rather than pulling
//! in a keyring crate: the local, already-unlocked CLI is the whole point.
//!
//! # Addressing
//!
//! A config var addresses this backend with a 1Password secret reference,
//! [`OnePasswordReference`]: `op://<vault>/<item>/<field>`, or
//! `op://<vault>/<item>/<section>/<field>` for a field inside a section. That
//! is 1Password's own addressing, and it is what the app's `Copy Secret
//! Reference` button puts on the clipboard, so a declaration is pasted rather
//! than translated.
//!
//! `op item get <item> --vault <vault> --format json` already reveals
//! concealed field values in its JSON output with no `--reveal` flag needed
//! (that flag only affects the masked human-readable table format), so one
//! call yields every field of an item at once. Reads are grouped by item for
//! that reason: four references into one item cost one subprocess, the same
//! way four keys at one OpenBao path cost one round trip. Switching to
//! `op read <reference>` would spend a subprocess per value instead.
//!
//! There is no [`SecretPath`]-shaped view of an item. A `"<vault>/<item>"`
//! path plus a field name is a fourth vocabulary nobody writes, and it cannot
//! express a section without inventing a spelling for one, so every caller
//! that wants a value here goes through [`OnePasswordReference`] and
//! [`OnePasswordItemFields::get`] instead.
//!
//! # Field matching
//!
//! `op` itself is looser than a literal comparison of what a reference names
//! against what `op item get --format json` reports, and this backend matches
//! the same way so a reference that resolves through `op read` also resolves
//! here: a field is addressable by its `label` or by its `id` (an imported
//! field can carry only the latter), matching is case-insensitive, and a
//! reference naming no section still reaches a field that happens to live in
//! one, provided that is unambiguous. See [`OnePasswordItemFields::get`] for
//! the exact precedence and why an ambiguous case is an error rather than a
//! guess.
//!
//! # Why the account is required
//!
//! A vault name is only unique *within* an account, and `op` will happily sign
//! a developer into several at once. Left to itself it resolves a bare
//! `--vault Ops` against whichever account it considers the default, which
//! is machine-local state — not something the config says — so the same
//! `[secrets.backends.<name>]` on two machines can address two different
//! vaults, and a personal account shadowing a work vault name is a silent
//! misread rather than an error. [`OpAccount`] is therefore mandatory and
//! every invocation carries `--account`: which account a path resolves against
//! is part of the address, and addresses belong in the config.
//!
//! # Why `op whoami` is not the health probe
//!
//! `op whoami` answers only for a session `op signin` (or a service account)
//! established. Under the desktop-app integration this backend is built
//! around it reports `account is not signed in` while every real read
//! succeeds, which made `trg doctor` call a perfectly healthy backend broken.
//! [`OnePasswordBackend::current_account`] asks `op account get` instead: it
//! needs the same live session an `item get` needs, so it fails exactly when
//! reads would.
//!
//! # What this does not do
//!
//! `set`/`delete`/`list` are unsupported: the items this backend reads are
//! managed by hand, via the 1Password app or `op` CLI directly, not through
//! `trg secret put/delete/list`. This is a read path onto secrets that
//! already exist, not a place `trg` writes to.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;
use tokio::process::Command;

use super::{SecretMap, SecretPath, SecretsError};

/// How long an `op` invocation gets before it is treated as hung.
///
/// `op` normally answers in well under a second, but a stalled 1Password
/// desktop app or a Touch ID prompt nothing is watching for can otherwise
/// block `get`/`current_account` — and, through the latter, `trg doctor` —
/// forever.
const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a `ref = "op://..."` declaration could not be read as an address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpReferenceError {
    /// The scheme is kept rather than stripped for brevity, because it is
    /// what makes a reference paste verbatim out of 1Password, and because it
    /// cross-checks that the var and the backend it names agree about which
    /// product is being addressed.
    #[error(
        "1Password secret reference `{0}` must start with `op://`, the form 1Password's `Copy Secret Reference` yields"
    )]
    Scheme(String),

    #[error("1Password secret reference `{0}` must be `op://<vault>/<item>/<field>` or `op://<vault>/<item>/<section>/<field>`")]
    Shape(String),

    #[error("1Password secret reference `{0}` must not contain an empty segment")]
    EmptySegment(String),

    /// Silently ignoring the query would hand back the field's plain value
    /// while the config says it asked for something else, which is a wrong
    /// answer rather than a missing feature.
    #[error("1Password secret reference `{reference}` carries `?{query}`, which is not supported by this backend")]
    Query { reference: String, query: String },

    /// The vault and the item/title are handed to `op` as argv (see
    /// [`OnePasswordBackend::get_item`]), so a leading `-` there reads as a
    /// flag rather than as the value to look up: `op item get "--help"
    /// --vault NoSuchVault` prints help text and exits success instead of
    /// complaining about the vault, which this backend would then try to
    /// parse as the item's JSON. The section and field are never passed to
    /// `op`; they are matched against the JSON `op item get` returns, so a
    /// section or field legitimately named `-foo` is untouched by this check.
    #[error("1Password secret reference `{reference}` must not start with `-` in its {part}")]
    Dash { reference: String, part: OpReferencePart },
}

/// Which segment of an `op://` reference [`OpReferenceError::Dash`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpReferencePart {
    Vault,
    Item,
}

impl fmt::Display for OpReferencePart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Vault => "vault",
            Self::Item => "item",
        })
    }
}

/// One `/`-separated piece of an `op://` reference.
///
/// One type for all four pieces because the grammar validates them
/// identically; which piece a value is, is carried by the field holding it
/// and by [`OnePasswordReference`]'s accessors, not by a fourfold repetition
/// of the same newtype.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OpSegment(String);

impl OpSegment {
    fn parse(raw: &str, reference: &str) -> Result<Self, OpReferenceError> {
        if raw.is_empty() {
            return Err(OpReferenceError::EmptySegment(reference.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    /// Like [`Self::parse`], plus a check that `op` cannot mistake this
    /// segment for a flag. Only the vault and the item/title go through this:
    /// they are the two segments [`OnePasswordBackend::get_item`] hands to
    /// `op` as argv, unlike the section and field, which are matched against
    /// JSON `op` already returned and so cannot be confused for a flag no
    /// matter what they start with.
    fn parse_argv(raw: &str, reference: &str, part: OpReferencePart) -> Result<Self, OpReferenceError> {
        let segment = Self::parse(raw, reference)?;
        if raw.starts_with('-') {
            return Err(OpReferenceError::Dash {
                reference: reference.to_string(),
                part,
            });
        }
        Ok(segment)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OpSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for OpSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OpSegment({:?})", self.0)
    }
}

/// The item one `op item get` reads, and therefore the unit reads are
/// grouped by.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OnePasswordItem {
    vault: OpSegment,
    title: OpSegment,
}

impl OnePasswordItem {
    pub fn new(vault: OpSegment, title: OpSegment) -> Self {
        Self { vault, title }
    }

    pub fn vault(&self) -> &OpSegment {
        &self.vault
    }

    pub fn title(&self) -> &OpSegment {
        &self.title
    }
}

impl fmt::Display for OnePasswordItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.vault, self.title)
    }
}

impl fmt::Debug for OnePasswordItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OnePasswordItem({}/{})", self.vault, self.title)
    }
}

/// A 1Password secret reference: `op://<vault>/<item>[/<section>]/<field>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OnePasswordReference {
    item: OnePasswordItem,
    section: Option<OpSegment>,
    field: OpSegment,
}

impl OnePasswordReference {
    pub const SCHEME: &'static str = "op://";

    pub fn parse(raw: &str) -> Result<Self, OpReferenceError> {
        let rest = raw
            .strip_prefix(Self::SCHEME)
            .ok_or_else(|| OpReferenceError::Scheme(raw.to_string()))?;

        // Before splitting, because `?attribute=otp` and `?ssh-format=openssh`
        // change what a reference resolves to, and this backend reads the
        // field's value and nothing else.
        if let Some((_, query)) = rest.split_once('?') {
            return Err(OpReferenceError::Query {
                reference: raw.to_string(),
                query: query.to_string(),
            });
        }

        let segments: Vec<&str> = rest.split('/').collect();
        let (vault, title, section, field) = match segments.as_slice() {
            [vault, title, field] => (*vault, *title, None, *field),
            [vault, title, section, field] => (*vault, *title, Some(*section), *field),
            _ => return Err(OpReferenceError::Shape(raw.to_string())),
        };

        let section = match section {
            None => None,
            Some(section) => Some(OpSegment::parse(section, raw)?),
        };

        Ok(Self {
            item: OnePasswordItem::new(
                OpSegment::parse_argv(vault, raw, OpReferencePart::Vault)?,
                OpSegment::parse_argv(title, raw, OpReferencePart::Item)?,
            ),
            section,
            field: OpSegment::parse(field, raw)?,
        })
    }

    pub fn item(&self) -> &OnePasswordItem {
        &self.item
    }

    pub fn vault(&self) -> &OpSegment {
        self.item.vault()
    }

    pub fn title(&self) -> &OpSegment {
        self.item.title()
    }

    pub fn section(&self) -> Option<&OpSegment> {
        self.section.as_ref()
    }

    pub fn field(&self) -> &OpSegment {
        &self.field
    }
}

impl fmt::Display for OnePasswordReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", Self::SCHEME, self.item)?;
        if let Some(section) = &self.section {
            write!(f, "/{section}")?;
        }
        write!(f, "/{}", self.field)
    }
}

/// One field of an item, as `op item get --format json` reports it.
///
/// The section is kept alongside the label rather than folded into it because
/// two sections of one item may hold a field of the same name, and 1Password
/// treats those as different fields.
#[derive(Clone, Debug)]
struct ItemField {
    /// `op` reports a section as an id plus, usually, a label. A reference
    /// written by hand or copied from the app names the label, but a section
    /// created by an import can arrive with only an id, so both are kept and
    /// either may be matched.
    section_id: Option<String>,
    section_label: Option<String>,
    /// A field's own id and label, kept the same way as the section's: a
    /// field created by an import can carry only an id, with no label at
    /// all, and `op` still lets a reference address it by that id.
    id: Option<String>,
    label: Option<String>,
    value: SecretString,
}

impl ItemField {
    fn has_no_section(&self) -> bool {
        self.section_id.is_none() && self.section_label.is_none()
    }

    /// Whether `section` names the section this field is in, matched against
    /// both the section's id and its label, case-insensitively because `op`
    /// itself resolves a reference that way.
    fn is_in_section(&self, section: &OpSegment) -> bool {
        let want = section.as_str().to_lowercase();
        [self.section_label.as_deref(), self.section_id.as_deref()]
            .into_iter()
            .flatten()
            .any(|known| known.to_lowercase() == want)
    }

    /// Whether `name` addresses this field, matched against both its label
    /// and its id, case-insensitively for the same reason as
    /// [`Self::is_in_section`].
    fn matches_name(&self, name: &str) -> bool {
        let want = name.to_lowercase();
        [self.label.as_deref(), self.id.as_deref()]
            .into_iter()
            .flatten()
            .any(|known| known.to_lowercase() == want)
    }

    /// The label if there is one, otherwise the id: `parse` never keeps a
    /// field with neither.
    fn name(&self) -> &str {
        self.label.as_deref().or(self.id.as_deref()).unwrap_or_default()
    }

    /// How this field is addressed, for an error to list when a reference
    /// resolves to nothing or to more than one field.
    fn address(&self) -> String {
        match self.section_label.as_deref().or(self.section_id.as_deref()) {
            Some(section) => format!("{section}/{}", self.name()),
            None => self.name().to_string(),
        }
    }
}

/// Every field of one item, ready to answer references against.
#[derive(Clone, Debug)]
pub struct OnePasswordItemFields {
    item: OnePasswordItem,
    fields: Vec<ItemField>,
}

impl OnePasswordItemFields {
    fn parse(item: &OnePasswordItem, stdout: &str) -> Result<Self, SecretsError> {
        let malformed = |cause: String| SecretsError::MalformedItem {
            item: item.clone(),
            cause,
        };

        let value: serde_json::Value = serde_json::from_str(stdout)
            .map_err(|e| malformed(format!("op item get did not return valid JSON: {e}")))?;
        let raw = value
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or_else(|| malformed("op item get response has no `fields` array".to_string()))?;

        // Section fields arrive in this same flat array, distinguished only by
        // their `section` object, which is why they have to be read out here
        // rather than looked for somewhere else in the document.
        let mut fields = Vec::with_capacity(raw.len());
        for field in raw {
            if field.get("purpose").and_then(|p| p.as_str()) == Some("NOTES") {
                continue;
            }
            let Some(value) = field.get("value").and_then(|v| v.as_str()) else {
                continue;
            };
            let id = field.get("id").and_then(|i| i.as_str()).map(str::to_string);
            let label = field.get("label").and_then(|l| l.as_str()).map(str::to_string);
            // A field with neither is not addressable by anything a
            // reference could name.
            if id.is_none() && label.is_none() {
                continue;
            }
            let section = field.get("section");
            fields.push(ItemField {
                section_id: section
                    .and_then(|s| s.get("id"))
                    .and_then(|i| i.as_str())
                    .map(str::to_string),
                section_label: section
                    .and_then(|s| s.get("label"))
                    .and_then(|l| l.as_str())
                    .map(str::to_string),
                id,
                label,
                value: SecretString::from(value.to_string()),
            });
        }

        Ok(Self {
            item: item.clone(),
            fields,
        })
    }

    pub fn item(&self) -> &OnePasswordItem {
        &self.item
    }

    /// Picks the one field a reference to `name` should resolve to out of
    /// `candidates`, which are already filtered down to whatever the caller
    /// considered plausible (a section, or the lack of one): none is a miss,
    /// exactly one resolves, and two or more is an ambiguity error rather
    /// than a pick among them, since either one being wrong would silently
    /// hand back a secret from the wrong place.
    fn resolve<'a>(&self, name: &str, candidates: &[&'a ItemField]) -> Result<Option<&'a ItemField>, SecretsError> {
        match candidates {
            [] => Ok(None),
            [only] => Ok(Some(only)),
            many => Err(SecretsError::AmbiguousField {
                item: self.item.clone(),
                field: name.to_string(),
                candidates: many.iter().map(|field| field.address()).collect::<Vec<_>>().join(", "),
            }),
        }
    }

    /// The value `reference` addresses, if the item holds it: `Ok(None)` when
    /// nothing matches, `Err` when more than one field is plausibly what
    /// `reference` means.
    ///
    /// When `reference` names a section, candidates are the fields of that
    /// name in that section: `op://v/i/Prod/TOKEN` cannot be answered with
    /// the staging section's `TOKEN`, but two fields both named `TOKEN` in
    /// `Prod` itself, or one addressed by that section's id and another by a
    /// same-named section's label, are as ambiguous as any other case.
    ///
    /// When `reference` names no section, the precedence is:
    /// 1. A field of that name with no section at all, which is exact and
    ///    unambiguous, so it wins outright. Two such fields sharing a name is
    ///    still an ambiguity error rather than a pick between them.
    /// 2. Otherwise, the sectioned fields of that name, resolved the same
    ///    way: none is a miss, one resolves, more than one is an ambiguity
    ///    error. Real `op` was not checked against a live vault for this
    ///    exact case, so which one it would choose is unknown; guessing
    ///    risks silently handing back the wrong secret, and an error naming
    ///    the choices is the safer failure.
    pub fn get(&self, reference: &OnePasswordReference) -> Result<Option<&SecretString>, SecretsError> {
        let name = reference.field().as_str();
        let by_name: Vec<&ItemField> = self.fields.iter().filter(|field| field.matches_name(name)).collect();

        let matched = match reference.section() {
            Some(section) => {
                let in_section: Vec<&ItemField> = by_name
                    .into_iter()
                    .filter(|field| field.is_in_section(section))
                    .collect();
                self.resolve(name, &in_section)?
            }
            None => {
                let sectionless: Vec<&ItemField> =
                    by_name.iter().copied().filter(|field| field.has_no_section()).collect();
                if sectionless.is_empty() {
                    self.resolve(name, &by_name)?
                } else {
                    self.resolve(name, &sectionless)?
                }
            }
        };

        Ok(matched.map(|field| &field.value))
    }

    /// Every field this item offers, for an error to show when a reference
    /// resolves to nothing. Sorted and deduplicated so the hint reads the same
    /// way on every run.
    pub fn addresses(&self) -> Vec<String> {
        self.fields
            .iter()
            .map(ItemField::address)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Build from a flat map, for a stand-in backend in tests: a key holding
    /// a `/` is read back as `<section>/<label>`.
    #[cfg(test)]
    pub fn from_secret_map(item: &OnePasswordItem, map: &SecretMap) -> Self {
        let fields = map
            .sorted_keys()
            .into_iter()
            .filter_map(|key| {
                let value = map.get(key)?.clone();
                let (section_label, label) = match key.as_str().split_once('/') {
                    Some((section, label)) => (Some(section.to_string()), label.to_string()),
                    None => (None, key.as_str().to_string()),
                };
                Some(ItemField {
                    section_id: None,
                    section_label,
                    id: None,
                    label: Some(label),
                    value,
                })
            })
            .collect();
        Self {
            item: item.clone(),
            fields,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("1Password account must not be empty")]
    Empty,

    /// `op` would read this as a flag rather than as the account to use, and
    /// report something about an unknown option instead of about the config.
    #[error("1Password account `{0}` must not start with `-`")]
    Dash(String),
}

/// Which signed-in `op` account a backend addresses.
///
/// Structurally validated only: `op --account` accepts a sign-in address, an
/// email, a user UUID or an account UUID, and which of those a value is can
/// only be settled by asking `op` (see [`OnePasswordBackend::accounts`]), not
/// by looking at the string.
#[derive(Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct OpAccount(String);

impl OpAccount {
    pub fn parse(raw: &str) -> Result<Self, AccountError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(AccountError::Empty);
        }
        if raw.starts_with('-') {
            return Err(AccountError::Dash(raw.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `signed_in` is the account this addresses.
    ///
    /// Matched over every form `op --account` accepts, because the config may
    /// legitimately carry any of them and refusing to recognise the one a
    /// developer chose would report a correct config as pointing nowhere.
    ///
    /// That includes the sign-in subdomain on its own — `my` for
    /// `my.1password.com` — which `op` accepts and which its own docs use, but
    /// which `op account list` never prints as a field of its own.
    fn addresses(&self, signed_in: &SignedInAccount) -> bool {
        let want = self.0.as_str();
        [
            signed_in.url.as_str(),
            signed_in.subdomain(),
            signed_in.email.as_str(),
            signed_in.user_uuid.as_str(),
            signed_in.account_uuid.as_str(),
        ]
        .into_iter()
        .any(|form| !form.is_empty() && form.eq_ignore_ascii_case(want))
    }
}

impl TryFrom<String> for OpAccount {
    type Error = AccountError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl fmt::Display for OpAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for OpAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OpAccount({:?})", self.0)
    }
}

/// One row of `op account list` — an account `op` on this machine can reach.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SignedInAccount {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub user_uuid: String,
    #[serde(default)]
    pub account_uuid: String,
}

impl SignedInAccount {
    /// The sign-in subdomain, `my` out of `my.1password.com`.
    ///
    /// Derived rather than read: `op account list` reports the full `url` and
    /// no separate field for this, though `--account` takes it.
    fn subdomain(&self) -> &str {
        self.url.split('.').next().unwrap_or_default()
    }
}

impl fmt::Display for SignedInAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.email.as_str(), self.url.as_str()) {
            ("", "") => f.write_str(&self.account_uuid),
            ("", url) => f.write_str(url),
            (email, "") => f.write_str(email),
            (email, url) => write!(f, "{email} ({url})"),
        }
    }
}

/// `op account get` — what the addressed account looks like to a live session.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CurrentAccount {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub state: String,
}

#[derive(Clone)]
pub struct OnePasswordBackend {
    account: OpAccount,
    /// The `op` executable to drive. Held as data rather than hardcoded at the
    /// call site so tests can drive a scripted one and pin the exact argv.
    bin: PathBuf,
    /// How long one `op` invocation gets; overridden in tests so a hung
    /// stub doesn't cost the suite real wall-clock time.
    timeout: Duration,
}

impl OnePasswordBackend {
    pub fn new(account: OpAccount) -> Self {
        Self {
            account,
            bin: PathBuf::from("op"),
            timeout: OP_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_bin(account: &str, bin: impl AsRef<std::path::Path>) -> Self {
        Self {
            account: OpAccount::parse(account).expect("test account"),
            bin: bin.as_ref().to_path_buf(),
            timeout: OP_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn account(&self) -> &OpAccount {
        &self.account
    }

    /// Read every field of one item in a single `op item get`.
    ///
    /// The whole item rather than one field, because a config typically
    /// addresses several fields of the same item and each `op` invocation is
    /// a subprocess plus a round trip to the desktop app.
    pub async fn get_item(&self, item: &OnePasswordItem) -> Result<Option<OnePasswordItemFields>, SecretsError> {
        let out = self
            .run(&[
                "item",
                "get",
                item.title().as_str(),
                "--vault",
                item.vault().as_str(),
                "--format",
                "json",
                "--account",
                self.account.as_str(),
            ])
            .await?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr = stderr.trim();
            if stderr.contains("isn't an item in the") {
                return Ok(None);
            }
            // `op` names neither the account it used nor the vaults that
            // account does have, so an address aimed at the wrong one of
            // several signed-in accounts otherwise reads as the vault having
            // vanished.
            let hint = if stderr.contains("isn't a vault in this account") {
                format!(" (account `{}`)", self.account)
            } else {
                String::new()
            };
            return Err(SecretsError::Unavailable(format!("op item get failed: {stderr}{hint}")));
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        OnePasswordItemFields::parse(item, &stdout).map(Some)
    }

    /// Every account `op` on this machine can reach, for `trg doctor` to
    /// resolve the configured [`OpAccount`] against.
    ///
    /// Reads local `op` state, so it answers whether or not anything is
    /// unlocked — which is the point: "you named an account `op` has never
    /// heard of" and "that account is locked" are different problems with
    /// different remedies, and conflating them sends people to `op signin`
    /// for a typo.
    pub async fn accounts(&self) -> Result<Vec<SignedInAccount>, SecretsError> {
        let out = self.run(&["account", "list", "--format", "json"]).await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(SecretsError::Unavailable(format!(
                "op account list failed: {}",
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(&stdout)
            .map_err(|e| SecretsError::Unavailable(format!("op account list did not return valid JSON: {e}")))
    }

    /// Confirm the addressed account has a session that can actually read.
    ///
    /// See the module docs on why this is `op account get` and not
    /// `op whoami`.
    pub async fn current_account(&self) -> Result<CurrentAccount, SecretsError> {
        let out = self
            .run(&["account", "get", "--format", "json", "--account", self.account.as_str()])
            .await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(SecretsError::Unavailable(format!(
                "op account get failed: {}",
                stderr.trim()
            )));
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(&stdout)
            .map_err(|e| SecretsError::Unavailable(format!("op account get did not return valid JSON: {e}")))
    }

    /// 1Password items are managed via the app or `op` CLI directly; `trg`
    /// only ever reads one that already exists.
    pub async fn set(&self, _path: &SecretPath, _map: &SecretMap) -> Result<(), SecretsError> {
        Err(SecretsError::Unsupported {
            kind: "onepassword",
            op: "put",
        })
    }

    /// See [`Self::set`] — nothing here is `trg`'s to remove.
    pub async fn delete(&self, _path: &SecretPath) -> Result<(), SecretsError> {
        Err(SecretsError::Unsupported {
            kind: "onepassword",
            op: "delete",
        })
    }

    /// See [`Self::set`] — enumerating would need a vault to scope to, which
    /// a bare prefix doesn't reliably carry, and there's no write side here to
    /// make listing useful anyway.
    pub async fn list(&self, _prefix: Option<&SecretPath>) -> Result<Vec<String>, SecretsError> {
        Err(SecretsError::Unsupported {
            kind: "onepassword",
            op: "list",
        })
    }

    async fn run(&self, args: &[&str]) -> Result<std::process::Output, SecretsError> {
        let child = Command::new(&self.bin)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SecretsError::Transport(format!("op {}: {e}", args.join(" "))))?;

        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(result) => result.map_err(|e| SecretsError::Transport(format!("op {}: {e}", args.join(" ")))),
            Err(_) => Err(SecretsError::Transport(format!(
                "op {} timed out after {:?} — is the 1Password app unlocked?",
                args.join(" "),
                self.timeout
            ))),
        }
    }
}

/// The account `filter` addresses, if `op` knows it.
pub fn resolve<'a>(filter: &OpAccount, known: &'a [SignedInAccount]) -> Option<&'a SignedInAccount> {
    known.iter().find(|a| filter.addresses(a))
}

/// The item a [`super::Backend`]-shaped `"<vault>/<item>"` path names.
#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    /// A stand-in for `op` that records its argv and answers with a fixed
    /// exit code, stdout and stderr.
    ///
    /// The backend under test is the production one driving a real child
    /// process, so what these tests pin down is the real argv this backend
    /// sends and the real stderr tokens it keys off, not a re-description of
    /// them.
    struct StubOp {
        dir: tempfile::TempDir,
    }

    fn sh_quote(raw: &str) -> String {
        format!("'{}'", raw.replace('\'', r"'\''"))
    }

    impl StubOp {
        fn answering(exit: i32, stdout: &str, stderr: &str) -> Self {
            Self::scripted(&format!(
                "printf '%s' {stdout}\nprintf '%s' {stderr} >&2\nexit {exit}\n",
                stdout = sh_quote(stdout),
                stderr = sh_quote(stderr),
            ))
        }

        fn ok(stdout: &str) -> Self {
            Self::answering(0, stdout, "")
        }

        /// A stand-in that answers per `op` subcommand, for the probes that
        /// make more than one call.
        fn routing(routes: &[(&str, i32, &str)]) -> Self {
            let mut body = String::from("case \"$1 $2\" in\n");
            for (subcommand, exit, stdout) in routes {
                body.push_str(&format!(
                    "  {subcommand}) printf '%s' {stdout}; exit {exit};;\n",
                    subcommand = sh_quote(subcommand),
                    stdout = sh_quote(stdout),
                ));
            }
            body.push_str("  *) printf 'unrouted: %s\\n' \"$*\" >&2; exit 127;;\nesac\n");
            Self::scripted(&body)
        }

        fn scripted(body: &str) -> Self {
            use std::os::unix::fs::PermissionsExt as _;

            let dir = tempfile::tempdir().expect("tempdir");
            let bin = dir.path().join("op");
            let argv = dir.path().join("argv");
            let script = format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> {argv}; done\n{body}",
                argv = sh_quote(&argv.display().to_string()),
            );
            std::fs::write(&bin, script).expect("write stub");
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).expect("chmod");

            Self { dir }
        }

        /// A stand-in that never returns on its own, for exercising the
        /// timeout — as if `op` were blocked on an unlock prompt no one is
        /// watching for.
        fn hanging() -> Self {
            use std::os::unix::fs::PermissionsExt as _;

            let dir = tempfile::tempdir().expect("tempdir");
            let bin = dir.path().join("op");
            std::fs::write(&bin, "#!/bin/sh\nsleep 300\n").expect("write stub");
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).expect("chmod");

            Self { dir }
        }

        fn backend(&self) -> OnePasswordBackend {
            OnePasswordBackend::with_bin(ACCOUNT, self.dir.path().join("op"))
        }

        fn argv(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("argv"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    const ACCOUNT: &str = "my.1password.com";

    const ITEM_JSON: &str = r#"{
        "title": "deploy-keys",
        "vault": { "id": "v1", "name": "Ops" },
        "category": "SECURE_NOTE",
        "fields": [
            { "label": "notesPlain", "type": "STRING", "purpose": "NOTES", "value": "some notes" },
            { "label": "TOKEN_A", "type": "CONCEALED", "value": "sk-ant-fake-value" },
            { "label": "TOKEN_B", "type": "CONCEALED", "value": "sk-ant-fake-value-2" }
        ]
    }"#;

    const ACCOUNTS_JSON: &str = r#"[
        { "url": "my.1password.com", "email": "someone@example.com", "user_uuid": "U1", "account_uuid": "A1" },
        { "url": "team-acme.1password.com", "email": "someone@acme.test", "user_uuid": "U2", "account_uuid": "A2" }
    ]"#;

    const CURRENT_JSON: &str = r#"{ "id": "A1", "name": "Someone's Family", "domain": "my", "state": "ACTIVE" }"#;

    const NOT_AN_ITEM: &str =
        "[ERROR] \"missing\" isn't an item in the \"Ops\" vault. Specify the item with its UUID, name, or domain.";
    const NOT_A_VAULT: &str = "[ERROR] \"Ops\" isn't a vault in this account. Specify the vault with its ID or name.";

    /// Two sections of one item holding a field of the same name is ordinary
    /// in 1Password, and `op item get --format json` reports both in the same
    /// flat `fields` array, so nothing but the section tells them apart.
    const SECTIONED_JSON: &str = r#"{
        "title": "deploy-keys",
        "vault": { "id": "v1", "name": "Ops" },
        "fields": [
            { "label": "TOKEN", "value": "loose" },
            { "section": { "id": "s1", "label": "Prod" }, "label": "TOKEN", "value": "prod" },
            { "section": { "id": "s2", "label": "Staging" }, "label": "TOKEN", "value": "staging" },
            { "section": { "id": "s3" }, "label": "TOKEN", "value": "unlabelled" }
        ]
    }"#;

    fn reference(raw: &str) -> OnePasswordReference {
        OnePasswordReference::parse(raw).expect(raw)
    }

    fn item(vault: &str, title: &str) -> OnePasswordItem {
        let at = format!("op://{vault}/{title}/FIELD");
        OnePasswordItem::new(
            OpSegment::parse(vault, &at).expect(vault),
            OpSegment::parse(title, &at).expect(title),
        )
    }

    fn sectioned_item() -> OnePasswordItemFields {
        let item = OnePasswordItem::new(
            OpSegment::parse("Ops", "op://Ops/deploy-keys/TOKEN").unwrap(),
            OpSegment::parse("deploy-keys", "op://Ops/deploy-keys/TOKEN").unwrap(),
        );
        OnePasswordItemFields::parse(&item, SECTIONED_JSON).expect("parse")
    }

    fn fields_from(json: &str) -> OnePasswordItemFields {
        OnePasswordItemFields::parse(&item("Ops", "deploy-keys"), json).expect("parse")
    }

    /// Every test here is against a reference that is not expected to be
    /// ambiguous, so the ambiguity error is unwrapped away and the assertion
    /// reads like the `Option` this returned before it had to become a
    /// `Result`.
    fn resolved<'a>(fields: &'a OnePasswordItemFields, raw: &str) -> Option<&'a str> {
        fields
            .get(&reference(raw))
            .expect("not expected to be ambiguous")
            .map(ExposeSecret::expose_secret)
    }

    /// A field's own `id`, alongside a section carrying only an `id` and no
    /// `label`, both of which an import can produce and which `op` itself
    /// still resolves a reference against.
    const ID_JSON: &str = r#"{
        "fields": [
            {
                "id": "hk4m2pq7wxrz9vnb3ldsy6tuce",
                "label": "TOKEN",
                "section": { "id": "extras" },
                "value": "id-and-label"
            },
            { "id": "idonly1", "value": "id-only" }
        ]
    }"#;

    #[test]
    fn a_three_segment_reference_names_a_field_with_no_section() {
        let parsed = reference("op://Ops/deploy-keys/TOKEN");

        assert_eq!(parsed.vault().as_str(), "Ops");
        assert_eq!(parsed.title().as_str(), "deploy-keys");
        assert_eq!(parsed.section(), None);
        assert_eq!(parsed.field().as_str(), "TOKEN");
    }

    #[test]
    fn a_four_segment_reference_names_the_section_between_item_and_field() {
        let parsed = reference("op://Ops/deploy-keys/Prod/TOKEN");

        assert_eq!(parsed.item().to_string(), "Ops/deploy-keys");
        assert_eq!(parsed.section().map(OpSegment::as_str), Some("Prod"));
        assert_eq!(parsed.field().as_str(), "TOKEN");
    }

    /// The rendering is what config errors and `trg doctor` show, so a
    /// reference has to read back exactly as it was pasted.
    #[test]
    fn a_reference_renders_back_as_it_was_written() {
        for raw in ["op://Ops/deploy-keys/TOKEN", "op://Ops/deploy-keys/Prod/TOKEN"] {
            assert_eq!(reference(raw).to_string(), raw);
        }
    }

    /// Keeping the scheme is what makes a var and its backend cross-check: a
    /// bare `Ops/deploy-keys/TOKEN` could as easily be a path meant for
    /// another backend.
    #[test]
    fn a_reference_without_the_scheme_is_refused() {
        assert!(matches!(
            OnePasswordReference::parse("Ops/deploy-keys/TOKEN"),
            Err(OpReferenceError::Scheme(_))
        ));
        assert!(matches!(
            OnePasswordReference::parse("https://Ops/deploy-keys/TOKEN"),
            Err(OpReferenceError::Scheme(_))
        ));
    }

    /// `?attribute=otp` and `?ssh-format=openssh` change what a reference
    /// resolves to, and this backend reads the stored value and nothing else.
    /// Ignoring the query would hand back a plausible wrong secret.
    #[test]
    fn a_reference_carrying_a_query_is_refused_rather_than_stripped() {
        for raw in [
            "op://Ops/deploy-keys/one-time password?attribute=otp",
            "op://Ops/deploy-keys/private key?ssh-format=openssh",
        ] {
            let err = OnePasswordReference::parse(raw).expect_err(raw);
            assert!(matches!(err, OpReferenceError::Query { .. }), "{err}");
            assert!(err.to_string().contains("not supported by this backend"), "{err}");
        }
    }

    #[test]
    fn a_reference_of_the_wrong_shape_is_refused() {
        for raw in ["op://", "op://Ops", "op://Ops/deploy-keys", "op://a/b/c/d/e"] {
            assert!(
                matches!(OnePasswordReference::parse(raw), Err(OpReferenceError::Shape(_))),
                "{raw}"
            );
        }
    }

    #[test]
    fn a_reference_with_an_empty_segment_is_refused() {
        for raw in [
            "op:///deploy-keys/TOKEN",
            "op://Ops//TOKEN",
            "op://Ops/deploy-keys/",
            "op://Ops/deploy-keys//TOKEN",
        ] {
            assert!(
                matches!(OnePasswordReference::parse(raw), Err(OpReferenceError::EmptySegment(_))),
                "{raw}"
            );
        }
    }

    /// `op item get "--help" --vault Ops` prints help text and exits success
    /// rather than reporting on the item, so a dashed vault must be refused
    /// before it ever reaches argv.
    #[test]
    fn a_reference_with_a_dashed_vault_is_refused() {
        let err = OnePasswordReference::parse("op://-Ops/deploy-keys/TOKEN").expect_err("refused");
        assert!(matches!(err, OpReferenceError::Dash { .. }), "{err:?}");
        assert!(err.to_string().contains("vault"), "{err}");
    }

    /// Same hazard as the vault, on the other argument `op item get` takes
    /// straight from the reference.
    #[test]
    fn a_reference_with_a_dashed_item_is_refused() {
        let err = OnePasswordReference::parse("op://Ops/-deploy-keys/TOKEN").expect_err("refused");
        assert!(matches!(err, OpReferenceError::Dash { .. }), "{err:?}");
        assert!(err.to_string().contains("item"), "{err}");
    }

    /// The section is never passed to `op` as argv, only matched against the
    /// JSON `op item get` returns, so a dash cannot be confused for a flag
    /// here and must keep resolving.
    #[tokio::test]
    async fn a_dashed_section_is_accepted_and_resolves_normally() {
        let stub = StubOp::ok(
            r#"{
                "fields": [
                    { "section": { "id": "s1", "label": "-Prod" }, "label": "TOKEN", "value": "prod-secret" }
                ]
            }"#,
        );
        let fields = stub
            .backend()
            .get_item(&item("Ops", "deploy-keys"))
            .await
            .expect("get")
            .expect("some");

        assert_eq!(
            resolved(&fields, "op://Ops/deploy-keys/-Prod/TOKEN"),
            Some("prod-secret")
        );
    }

    /// Same as the section: the field is matched client-side, never handed to
    /// `op` as argv, so a field legitimately named `-TOKEN` must keep working.
    #[tokio::test]
    async fn a_dashed_field_is_accepted_and_resolves_normally() {
        let stub = StubOp::ok(
            r#"{
                "fields": [
                    { "label": "-TOKEN", "value": "dashed-field-value" }
                ]
            }"#,
        );
        let fields = stub
            .backend()
            .get_item(&item("Ops", "deploy-keys"))
            .await
            .expect("get")
            .expect("some");

        assert_eq!(
            resolved(&fields, "op://Ops/deploy-keys/-TOKEN"),
            Some("dashed-field-value")
        );
    }

    /// The whole point of carrying the section: a map keyed on label alone
    /// would keep whichever `TOKEN` came last and answer every reference with
    /// it. This also covers a reference naming no section preferring the
    /// unsectioned field over any sectioned one of the same name.
    #[test]
    fn fields_sharing_a_label_in_different_sections_do_not_collide() {
        let fields = sectioned_item();

        for (raw, want) in [
            ("op://Ops/deploy-keys/TOKEN", "loose"),
            ("op://Ops/deploy-keys/Prod/TOKEN", "prod"),
            ("op://Ops/deploy-keys/Staging/TOKEN", "staging"),
            ("op://Ops/deploy-keys/s3/TOKEN", "unlabelled"),
        ] {
            assert_eq!(resolved(&fields, raw), Some(want), "{raw}");
        }
    }

    /// The same case named directly, since it is the one place an
    /// unqualified reference resolving at all depends on picking the right
    /// field rather than merely on there being one candidate.
    #[test]
    fn an_unsectioned_field_is_preferred_over_sectioned_fields_of_the_same_name() {
        assert_eq!(resolved(&sectioned_item(), "op://Ops/deploy-keys/TOKEN"), Some("loose"));
    }

    /// A reference naming no section still reaches a field that lives in a
    /// section, as long as it is the only field of that name in the item.
    /// This is a deliberate divergence from a literal reading of 1Password's
    /// own reference grammar, matched here because `op read` resolves this
    /// shape of reference in practice.
    #[test]
    fn a_sectioned_field_resolves_without_the_section_segment_when_it_is_the_only_match() {
        let fields = OnePasswordItemFields::parse(
            &item("Ops", "deploy-keys"),
            r#"{ "fields": [{ "section": { "id": "s1", "label": "Prod" }, "label": "TOKEN", "value": "prod" }] }"#,
        )
        .expect("parse");

        assert_eq!(resolved(&fields, "op://Ops/deploy-keys/TOKEN"), Some("prod"));
        assert_eq!(fields.addresses(), vec!["Prod/TOKEN".to_string()]);
    }

    /// Two sectioned fields sharing a name is precisely the case a bare
    /// reference cannot resolve safely: which one real `op` would pick was
    /// never checked against a live vault, and guessing risks silently
    /// handing back the wrong secret, so this is an error rather than a pick.
    #[test]
    fn two_sectioned_fields_sharing_a_name_are_an_ambiguity_error_not_a_guess() {
        let fields = fields_from(
            r#"{
                "fields": [
                    { "section": { "id": "s1", "label": "Prod" }, "label": "TOKEN", "value": "prod-secret" },
                    { "section": { "id": "s2", "label": "Staging" }, "label": "TOKEN", "value": "staging-secret" }
                ]
            }"#,
        );

        let err = fields
            .get(&reference("op://Ops/deploy-keys/TOKEN"))
            .expect_err("ambiguous");
        assert!(matches!(err, SecretsError::AmbiguousField { .. }), "{err:?}");

        let message = err.to_string();
        assert!(message.contains("Prod/TOKEN"), "{message}");
        assert!(message.contains("Staging/TOKEN"), "{message}");
        assert!(!message.contains("prod-secret"), "{message}");
        assert!(!message.contains("staging-secret"), "{message}");
    }

    /// A section can be named by id or by label, so a field reachable via one
    /// section's id and a different field reachable via another section's
    /// label, in a different case besides, are both plausible answers to the
    /// same section-qualified reference, and neither should win silently.
    #[test]
    fn two_fields_reachable_via_different_section_identifiers_are_an_ambiguity_error() {
        let fields = fields_from(
            r#"{
                "fields": [
                    { "section": { "id": "extras" }, "label": "TOKEN", "value": "via-id" },
                    { "section": { "id": "s9", "label": "Extras" }, "label": "TOKEN", "value": "via-label" }
                ]
            }"#,
        );

        let err = fields
            .get(&reference("op://Ops/deploy-keys/extras/TOKEN"))
            .expect_err("ambiguous");
        assert!(matches!(err, SecretsError::AmbiguousField { .. }), "{err:?}");

        let message = err.to_string();
        assert!(message.contains("extras/TOKEN"), "{message}");
        assert!(message.contains("Extras/TOKEN"), "{message}");
        assert!(!message.contains("via-id"), "{message}");
        assert!(!message.contains("via-label"), "{message}");
    }

    /// Two fields of the same name in the very same section are just as
    /// ambiguous as two in different sections; the section qualifying the
    /// reference narrows which section, not which field within it.
    #[test]
    fn two_fields_of_the_same_name_in_the_same_section_are_an_ambiguity_error() {
        let fields = fields_from(
            r#"{
                "fields": [
                    { "section": { "id": "s1", "label": "Prod" }, "label": "TOKEN", "value": "prod-a" },
                    { "section": { "id": "s1", "label": "Prod" }, "label": "TOKEN", "value": "prod-b" }
                ]
            }"#,
        );

        let err = fields
            .get(&reference("op://Ops/deploy-keys/Prod/TOKEN"))
            .expect_err("ambiguous");
        assert!(matches!(err, SecretsError::AmbiguousField { .. }), "{err:?}");

        let message = err.to_string();
        assert!(message.contains("Prod/TOKEN"), "{message}");
        assert!(!message.contains("prod-a"), "{message}");
        assert!(!message.contains("prod-b"), "{message}");
    }

    /// The same guarantee at the top of the precedence: an item can hold two
    /// unsectioned fields sharing a label, and a reference naming no section
    /// must not pick between them either.
    #[test]
    fn two_unsectioned_fields_sharing_a_label_are_an_ambiguity_error() {
        let fields = fields_from(
            r#"{
                "fields": [
                    { "label": "TOKEN", "value": "loose-a" },
                    { "label": "TOKEN", "value": "loose-b" }
                ]
            }"#,
        );

        let err = fields
            .get(&reference("op://Ops/deploy-keys/TOKEN"))
            .expect_err("ambiguous");
        assert!(matches!(err, SecretsError::AmbiguousField { .. }), "{err:?}");

        let message = err.to_string();
        assert!(message.contains("TOKEN"), "{message}");
        assert!(!message.contains("loose-a"), "{message}");
        assert!(!message.contains("loose-b"), "{message}");
    }

    /// A section-qualified reference is refused if the section it names
    /// simply is not one this item has.
    #[test]
    fn a_reference_naming_a_section_that_does_not_exist_misses() {
        assert_eq!(resolved(&sectioned_item(), "op://Ops/deploy-keys/Nope/TOKEN"), None);
    }

    /// A field addressed under the wrong section must not fall through to a
    /// match in whichever section it does live in. The field existing
    /// somewhere in the item is not enough; `Staging` in this fixture is a
    /// real section (it holds `OTHER`), just not the one `SOLO` is in.
    #[test]
    fn a_reference_naming_a_section_the_field_is_not_in_still_misses() {
        let fields = fields_from(
            r#"{
                "fields": [
                    { "section": { "id": "s1", "label": "Prod" }, "label": "SOLO", "value": "prod-solo" },
                    { "section": { "id": "s2", "label": "Staging" }, "label": "OTHER", "value": "staging-other" }
                ]
            }"#,
        );

        assert_eq!(resolved(&fields, "op://Ops/deploy-keys/Staging/SOLO"), None);
    }

    /// `label` and `id` are both accepted, and matching either is
    /// case-insensitive, because 1Password labels may be non-ASCII and `op`
    /// itself resolves a reference this loosely.
    #[test]
    fn a_field_is_matched_by_its_label_case_insensitively() {
        let fields = fields_from(ID_JSON);
        for raw in [
            "op://Ops/deploy-keys/extras/TOKEN",
            "op://Ops/deploy-keys/extras/token",
            "op://Ops/deploy-keys/EXTRAS/TOKEN",
        ] {
            assert_eq!(resolved(&fields, raw), Some("id-and-label"), "{raw}");
        }
    }

    /// A section named by its label is matched case-insensitively too.
    #[test]
    fn a_section_label_match_is_case_insensitive() {
        let fields = sectioned_item();
        for raw in ["op://Ops/deploy-keys/PROD/TOKEN", "op://Ops/deploy-keys/prod/TOKEN"] {
            assert_eq!(resolved(&fields, raw), Some("prod"), "{raw}");
        }
    }

    /// A field is addressable by its `id` too, with or without the section.
    /// This item's id-bearing field is the only one of that id, so it
    /// resolves the same way an unqualified label would.
    #[test]
    fn a_field_is_matched_by_its_id_case_insensitively() {
        let fields = fields_from(ID_JSON);
        for raw in [
            "op://Ops/deploy-keys/extras/hk4m2pq7wxrz9vnb3ldsy6tuce",
            "op://Ops/deploy-keys/hk4m2pq7wxrz9vnb3ldsy6tuce",
            "op://Ops/deploy-keys/HK4M2PQ7WXRZ9VNB3LDSY6TUCE",
        ] {
            assert_eq!(resolved(&fields, raw), Some("id-and-label"), "{raw}");
        }
    }

    /// A field imported with an `id` and no `label` at all is not dropped by
    /// `parse`, and is still addressable by that `id`.
    #[test]
    fn a_field_with_only_an_id_and_no_label_is_matched_by_that_id() {
        let fields = fields_from(ID_JSON);
        assert_eq!(resolved(&fields, "op://Ops/deploy-keys/idonly1"), Some("id-only"));
        assert_eq!(resolved(&fields, "op://Ops/deploy-keys/IDONLY1"), Some("id-only"));
    }

    /// What a miss shows, so it has to name sectioned fields the way a
    /// reference would have to write them.
    #[test]
    fn the_addresses_of_an_item_are_the_way_its_fields_are_referenced() {
        assert_eq!(
            sectioned_item().addresses(),
            vec![
                "Prod/TOKEN".to_string(),
                "Staging/TOKEN".to_string(),
                "TOKEN".to_string(),
                "s3/TOKEN".to_string(),
            ]
        );
    }

    #[test]
    fn an_account_is_rejected_when_it_is_empty_or_would_read_as_a_flag() {
        assert!(matches!(OpAccount::parse(""), Err(AccountError::Empty)));
        assert!(matches!(OpAccount::parse("   "), Err(AccountError::Empty)));
        assert!(matches!(OpAccount::parse("--account"), Err(AccountError::Dash(_))));
        assert_eq!(OpAccount::parse("  my.1password.com ").unwrap().as_str(), ACCOUNT);
    }

    #[test]
    fn an_account_resolves_by_any_form_op_itself_accepts() {
        let known: Vec<SignedInAccount> = serde_json::from_str(ACCOUNTS_JSON).unwrap();

        for form in [
            "my.1password.com",
            "MY.1Password.com",
            "someone@example.com",
            "U1",
            "A1",
        ] {
            let found = resolve(&OpAccount::parse(form).unwrap(), &known);
            assert_eq!(found.map(|a| a.account_uuid.as_str()), Some("A1"), "{form}");
        }
        assert!(resolve(&OpAccount::parse("nope.1password.com").unwrap(), &known).is_none());
    }

    /// `op --account my` reads fine, so a config saying `my` is correct and
    /// must not be reported as naming an account `op` has never heard of.
    #[test]
    fn an_account_resolves_by_the_bare_sign_in_subdomain() {
        let known: Vec<SignedInAccount> = serde_json::from_str(ACCOUNTS_JSON).unwrap();

        for (form, want) in [("my", "A1"), ("MY", "A1"), ("team-acme", "A2")] {
            let found = resolve(&OpAccount::parse(form).unwrap(), &known);
            assert_eq!(found.map(|a| a.account_uuid.as_str()), Some(want), "{form}");
        }
        assert!(resolve(&OpAccount::parse("team-other").unwrap(), &known).is_none());
    }

    /// An account row missing a field must not turn every lookup into a match
    /// on the empty string.
    #[test]
    fn an_account_with_blank_fields_matches_only_what_it_does_carry() {
        let known = vec![SignedInAccount {
            url: String::new(),
            email: String::new(),
            user_uuid: String::new(),
            account_uuid: "A9".to_string(),
        }];

        assert!(resolve(&OpAccount::parse("my.1password.com").unwrap(), &known).is_none());
        assert_eq!(
            resolve(&OpAccount::parse("A9").unwrap(), &known).map(|a| a.account_uuid.as_str()),
            Some("A9")
        );
    }

    #[tokio::test]
    async fn a_read_answers_with_every_field_but_the_note_body() {
        let stub = StubOp::ok(ITEM_JSON);
        let fields = stub
            .backend()
            .get_item(&item("Ops", "deploy-keys"))
            .await
            .expect("get")
            .expect("some");

        assert_eq!(
            resolved(&fields, "op://Ops/deploy-keys/TOKEN_A"),
            Some("sk-ant-fake-value")
        );
        assert!(!fields.addresses().contains(&"notesPlain".to_string()));
    }

    #[tokio::test]
    async fn every_read_carries_the_configured_account() {
        let stub = StubOp::ok(ITEM_JSON);
        stub.backend().get_item(&item("Ops", "deploy-keys")).await.expect("get");

        assert_eq!(
            stub.argv(),
            [
                "item",
                "get",
                "deploy-keys",
                "--vault",
                "Ops",
                "--format",
                "json",
                "--account",
                ACCOUNT
            ]
        );
    }

    #[tokio::test]
    async fn a_read_of_a_missing_item_is_a_miss_not_an_error() {
        let stub = StubOp::answering(1, "", NOT_AN_ITEM);
        assert!(stub
            .backend()
            .get_item(&item("Ops", "missing"))
            .await
            .expect("get")
            .is_none());
    }

    #[tokio::test]
    async fn a_vault_missing_from_the_account_names_the_account_that_was_asked() {
        let stub = StubOp::answering(1, "", NOT_A_VAULT);
        let err = stub
            .backend()
            .get_item(&item("Ops", "deploy-keys"))
            .await
            .expect_err("should fail");

        assert!(matches!(err, SecretsError::Unavailable(_)), "{err:?}");
        assert!(err.to_string().contains(ACCOUNT), "{err}");
    }

    #[tokio::test]
    async fn a_payload_that_is_not_valid_json_is_malformed() {
        let stub = StubOp::ok("not json");
        let err = stub
            .backend()
            .get_item(&item("Ops", "deploy-keys"))
            .await
            .expect_err("should fail");
        assert!(matches!(err, SecretsError::MalformedItem { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn set_delete_and_list_are_unsupported_and_spawn_nothing() {
        let stub = StubOp::ok(ITEM_JSON);
        let backend = stub.backend();
        let path = SecretPath::parse("Ops/deploy-keys").unwrap();

        assert!(matches!(
            backend.set(&path, &SecretMap::new()).await,
            Err(SecretsError::Unsupported { op: "put", .. })
        ));
        assert!(matches!(
            backend.delete(&path).await,
            Err(SecretsError::Unsupported { op: "delete", .. })
        ));
        assert!(matches!(
            backend.list(None).await,
            Err(SecretsError::Unsupported { op: "list", .. })
        ));
        assert!(stub.argv().is_empty(), "should not have spawned op");
    }

    #[tokio::test]
    async fn accounts_lists_what_op_can_reach_without_naming_one() {
        let stub = StubOp::routing(&[("account list", 0, ACCOUNTS_JSON)]);
        let known = stub.backend().accounts().await.expect("accounts");

        assert_eq!(known.len(), 2);
        assert_eq!(known[0].email, "someone@example.com");
        assert_eq!(stub.argv(), ["account", "list", "--format", "json"]);
    }

    #[tokio::test]
    async fn the_session_probe_asks_op_about_the_configured_account() {
        let stub = StubOp::routing(&[("account get", 0, CURRENT_JSON)]);
        let current = stub.backend().current_account().await.expect("current");

        assert_eq!(current.id, "A1");
        assert_eq!(current.state, "ACTIVE");
        assert_eq!(
            stub.argv(),
            ["account", "get", "--format", "json", "--account", ACCOUNT]
        );
    }

    #[tokio::test]
    async fn a_locked_or_signed_out_account_makes_the_session_probe_fail() {
        let stub = StubOp::answering(1, "", "[ERROR] account is not signed in");
        let err = stub.backend().current_account().await.expect_err("should fail");
        assert!(matches!(err, SecretsError::Unavailable(_)), "{err:?}");
    }

    #[tokio::test]
    async fn a_hung_op_is_reported_as_timed_out_rather_than_waited_on_forever() {
        let stub = StubOp::hanging();
        let backend = stub.backend().with_timeout(Duration::from_millis(50));

        let started = std::time::Instant::now();
        let err = backend.current_account().await.expect_err("should time out");

        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "should not wait anywhere near the stub's 300s sleep"
        );
    }

    #[tokio::test]
    async fn an_unspawnable_op_binary_is_a_transport_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let backend = OnePasswordBackend::with_bin(ACCOUNT, dir.path().join("absent"));
        let err = backend
            .get_item(&item("Ops", "deploy-keys"))
            .await
            .expect_err("should fail");
        assert!(matches!(err, SecretsError::Transport(_)), "{err:?}");
    }
}
