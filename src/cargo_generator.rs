/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

//! Cargo.toml generation logic.

mod generation;
mod generator;

pub use generator::CargoGenerator;
pub use generator::GenerationOutput;

/// Preamble that can be found on the first line of an autocargo generated file
pub static GENERATED_PREAMBLE: &str = "\x40generated by autocargo";
