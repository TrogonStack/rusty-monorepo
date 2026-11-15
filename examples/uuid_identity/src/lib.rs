#![no_std]
#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

pub mod generated;

#[derive(Debug, Clone, Copy, PartialEq, Eq, trogon_object_id::ObjectId)]
#[object_id(prefix = "sku", template = generated::SkuIdentityV1)]
#[repr(transparent)]
pub struct SkuId(trogon_object_id::Uuid);
