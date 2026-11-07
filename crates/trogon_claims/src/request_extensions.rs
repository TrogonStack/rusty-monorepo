use std::sync::Arc;

use http::Request;

use crate::claim_set::ClaimSet;
use crate::credential_outcome::CredentialOutcome;
use crate::presented_proof::PresentedProof;
use crate::principal::Principal;
use crate::verified_binding::Method;

pub fn insert_principal<C, B>(request: &mut Request<B>, principal: Principal<C>)
where
    C: ClaimSet + Send + Sync + 'static,
{
    request.extensions_mut().insert(Arc::new(principal));
}

pub fn insert_shared_principal<C, B>(request: &mut Request<B>, principal: Arc<Principal<C>>)
where
    C: ClaimSet + Send + Sync + 'static,
{
    request.extensions_mut().insert(principal);
}

pub fn insert_credential_outcome<B>(request: &mut Request<B>, outcome: CredentialOutcome) {
    request.extensions_mut().insert(outcome);
}

pub fn insert_presented_proof<M, B>(request: &mut Request<B>, proof: PresentedProof<M>)
where
    M: Method + 'static,
{
    request.extensions_mut().insert(proof);
}
