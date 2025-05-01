/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

//! Main crate used by autocargo binary to handle generating Cargo.toml files out
//! of Buck files.

#![deny(clippy::all)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(warnings)]
#![allow(clippy::needless_lifetimes)]
#![cfg_attr(not(test), deny(missing_docs))]
#![feature(iter_advance_by)]
#![feature(try_blocks)]

extern crate pretty_assertions;

pub mod buck_processing;
pub mod cargo_generator;
mod cargo_manifest;
pub mod config;
pub mod paths;
pub mod project_loader;
mod util;
pub use crate::util::future_timeout::future_soft_timeout;
