// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

//! This module provides structures representing Cargo.toml content and methods
//! to serialize those structures to toml.

mod dependencies;
mod manifest;
mod package;
mod product;
mod profiles;
mod target_key;
mod toml_util;

pub use manifest::Manifest;
pub use package::Package;
pub use product::Product;
pub use target_key::KeyedTargetDepsSet;
pub use target_key::TargetKey;
