//! Mirroring a private SSH key into an OpenBao Transit key.
//!
//! Transit never gives a private key back, so the two sides are compared by
//! public key: the one the source key implies and the one Transit reports for
//! its newest version. Equal means in sync, provided that version can sign: a
//! version imported as a public key alone reports the same public key and is
//! completed with the private one. A missing Transit key is imported;
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

    let mut completes_public_only = false;
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
            // The same public key is not enough: OpenBao accepts a public
            // key alone, and such a version can verify but never sign. Only
            // the engine can say which it is (see `transit_signs`), and when
            // it holds just the public half, importing the private key
            // completes that version in place.
            completes_public_only = &current == key.public();
            if completes_public_only
                && bao
                    .transit_signs(mount, name, transit_key::sign_parameters(key.public()))
                    .await
                    .map_err(MirrorError::TargetRead)?
            {
                return Ok(MirrorOutcome::UpToDate);
            }
            // A version is added to the existing key, so the key must be one
            // the private half cannot leave. A dry run fails the same way,
            // since it could never be applied.
            if found.exportable || found.allow_plaintext_backup {
                return Err(MirrorError::TransitKeyExposed {
                    exportable: found.exportable,
                    allow_plaintext_backup: found.allow_plaintext_backup,
                });
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
    // The public key was already right, so only a signature shows that the
    // private half arrived.
    if completes_public_only
        && !bao
            .transit_signs(mount, name, transit_key::sign_parameters(key.public()))
            .await
            .map_err(MirrorError::TargetRead)?
    {
        return Err(MirrorError::ReadbackMismatch);
    }
    Ok(match outcome {
        MirrorOutcome::WouldCreate => MirrorOutcome::Created,
        _ => MirrorOutcome::Updated,
    })
}
