use crate::role_name::RoleName;

pub trait ClaimSet {}

pub trait HasRoles: ClaimSet {
    fn roles(&self) -> &[RoleName];
}
