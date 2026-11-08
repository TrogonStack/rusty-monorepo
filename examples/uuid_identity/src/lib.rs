#![no_std]
#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

#[cfg(test)]
extern crate std;

pub mod generated;

#[derive(Debug, Clone, Copy, PartialEq, Eq, trogon_object_id::ObjectId)]
#[object_id(prefix = "sku", template = generated::SkuIdentityV1)]
#[repr(transparent)]
pub struct SkuId(trogon_object_id::Uuid);

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
