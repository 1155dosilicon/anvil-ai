//! Offline USB bundle builder, upstream syncer, and SHA-256 verifier for Anvil.

pub mod error;
pub mod recipe;
pub mod repack;
pub mod usb;

pub use error::{BundleError, Result};
pub use recipe::{BundleRecipe, RecipeBook, UpstreamArtifact, UpstreamLayout};
pub use usb::{
    init_usb, pack_bundle, sync_usb, verify_usb, BundleVerifyItem, InitUsbReport, PackReport, PackRequest, SyncOptions,
    SyncPlanItem, SyncReport, VerifyUsbReport,
};
