//! Artifact-backed resource identity and admission policy.
//!
//! Large content remains in the artifact store. Protocol and session values
//! carry these inert, bounded facts; they never carry bytes, filesystem paths,
//! decoder handles, or frontend callbacks. User-configurable limits may only
//! narrow the immutable compiled ceiling defined here.

mod policy;
mod reference;

pub(crate) use policy::ResourcePolicyV1;
pub(crate) use reference::{
    AccessibilityFactsV1, AccessibilitySourceV1, MediaTypeFactsV1, ResourceKindV1,
    ResourceMetadataV1, ResourceRefV1, ResourceValidationV1,
};

pub(crate) const RESOURCE_SCHEMA_VERSION: u16 = 1;
pub(crate) const MAX_RESOURCE_SOURCE_BYTES: usize = 512 * 1024 * 1024;
pub(crate) const DEFAULT_STATIC_RASTER_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const DEFAULT_STATIC_RASTER_PIXELS: u64 = 40_000_000;
pub(crate) const DEFAULT_STATIC_RASTERS_PER_TURN: usize = 8;
pub(crate) const DEFAULT_STATIC_RASTER_TURN_BYTES: u64 = 20 * 1024 * 1024;

#[cfg(test)]
mod tests;
