/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

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
