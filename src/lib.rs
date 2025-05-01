// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

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
