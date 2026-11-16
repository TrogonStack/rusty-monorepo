//! Mirroring a private SSH key into an OpenBao Transit key.
//!
//! Transit never gives a private key back, so the two sides are compared by
//! public key: the one the source key implies and the one Transit reports for
//! its newest version. Equal means in sync. A missing Transit key is imported;
//! a different one gets the source key as a new version, which OpenBao only
//! allows on a key that was itself imported.

use super::openssh::{SourceKey, SourceKeyError};
use super::{MirrorError, MirrorOutcome, TransitTarget};
use crate::git::ssh::transit_key;
use crate::secrets::openbao::wrap_for_transit;
use crate::secrets::Backend;

pub(super) async fn sync(
    backend: &Backend,
    target: &TransitTarget,
    key: &Result<SourceKey, SourceKeyError>,
    apply: bool,
) -> Result<MirrorOutcome, MirrorError> {
    let key = key.as_ref().map_err(|e| MirrorError::SourceKey(e.clone()))?;
    let Backend::OpenBao(bao) = backend else {
        return Err(MirrorError::TransitNeedsOpenbao {
            backend: target.backend.clone(),
            kind: backend.kind(),
        });
    };
    let (mount, name) = (target.mount.as_str(), target.key.as_str());

    let held = bao
        .transit_public_key(mount, name)
        .await
        .map_err(MirrorError::TargetRead)?;
    let outcome = match &held {
        None => MirrorOutcome::WouldCreate,
        Some(found) => {
            if found.key_type != key.transit_type() {
                return Err(MirrorError::TransitTypeChange {
                    held: found.key_type.clone(),
                    wanted: key.transit_type(),
                });
            }
            let current = transit_key::public_key(found).map_err(MirrorError::TransitKey)?;
            if &current == key.public() {
                return Ok(MirrorOutcome::UpToDate);
            }
            MirrorOutcome::WouldUpdate
        }
    };
    if !apply {
        return Ok(outcome);
    }

    let wrapping = bao.transit_wrapping_key(mount).await.map_err(MirrorError::TargetRead)?;
    let ciphertext = wrap_for_transit(&wrapping, key.pkcs8()).map_err(MirrorError::Wrap)?;
    match outcome {
        MirrorOutcome::WouldCreate => bao
            .transit_import(mount, name, key.transit_type(), &ciphertext)
            .await
            .map_err(MirrorError::TargetWrite)?,
        _ => bao
            .transit_import_version(mount, name, &ciphertext)
            .await
            .map_err(MirrorError::TargetWrite)?,
    }

    let back = bao
        .transit_public_key(mount, name)
        .await
        .map_err(MirrorError::TargetRead)?
        .and_then(|found| transit_key::public_key(&found).ok());
    if back.as_ref() != Some(key.public()) {
        return Err(MirrorError::ReadbackMismatch);
    }
    Ok(match outcome {
        MirrorOutcome::WouldCreate => MirrorOutcome::Created,
        _ => MirrorOutcome::Updated,
    })
}
