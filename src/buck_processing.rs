/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

//! Module containing methods for discovering and processing buck's rust
//! manifests together with structures that represent the results of this
//! processing.

mod commands;
mod loader;
mod manifest;
mod raw_manifest;
mod rules;
#[cfg(test)]
mod test_utils;

use std::collections::HashMap;
use std::collections::HashSet;

use anyhow::Result;
pub use manifest::BuckDependency;
pub use manifest::BuckDependencyOverride;
pub use manifest::BuckManifest;
pub use manifest::BuckTargetDependencies;
pub use manifest::CODEGEN_INCLUDER_PROC_MACRO_RULE;
pub use manifest::ExtraBuckDependencies;
pub use manifest::FbconfigRuleType;
pub use manifest::OsDepsPlatform;
pub use manifest::THRIFT_COMPILER_RULE;
pub use manifest::ThriftConfig;
pub use raw_manifest::AutocargoCargoTomlConfig;
pub use raw_manifest::AutocargoField;
pub use raw_manifest::AutocargoPackageConfig;
pub use raw_manifest::AutocargoTargetConfig;
pub use raw_manifest::AutocargoThrift;
pub use raw_manifest::AutocargoThriftOptions;
pub use raw_manifest::CargoDependencyOverride;
pub use raw_manifest::DependenciesOverride;
pub use raw_manifest::RawBuckManifest;
pub use raw_manifest::RawBuckManifestDependencies;
pub use raw_manifest::RawBuckManifestRustConfig;
pub use raw_manifest::RawBuckManifestSources;
pub use raw_manifest::RawFbconfigRuleType;
pub use raw_manifest::RawOsDepsPlatform;
pub use raw_manifest::TargetDependenciesOverride;
use slog::Logger;

use self::loader::BuckManifestLoader;
use self::manifest::process_raw_manifests;
use crate::paths::FbcodeRoot;
use crate::paths::TargetsPath;
use crate::util::command_runner::MockableCommandRunner;

/// Result of processing buck's rust manifests from given TARGETS files.
pub struct ProcessOutput {
    /// The manifests that have been processed grouped by TARGETS files that hold
    /// their definitions.
    pub processed_manifests: HashMap<TargetsPath, Vec<BuckManifest>>,
    /// Some dependencies of processed manifests were not mentioned by the
    /// provided TARGETS files, i.e. they might come from a project that has not
    /// been selected by the user or they might be not covered at all by any
    /// project. This field holds a set of all TARGETS files that were holding
    /// the aforementioned unprocessed dependencies. Later these files will be
    /// check if they are covered by any project and that information will be fed
    /// into cargo generator.
    pub unprocessed_paths: HashSet<TargetsPath>,
}

/// Uses Buck for querying and building of rust manifests contained in provided
/// TARGETS as well as parsing and resolving their dependencies even if they are
/// outside of the provided TARGETS.
pub async fn process_targets<'a>(
    logger: &'a Logger,
    fbcode_root: &'a FbcodeRoot,
    use_isolation_dir: bool,
    targets: impl IntoIterator<Item = &'a TargetsPath> + 'a,
) -> Result<ProcessOutput> {
    let raw_manifests = BuckManifestLoader::from_targets_paths(
        logger,
        fbcode_root,
        use_isolation_dir,
        targets,
        MockableCommandRunner::default(),
    )
    .await?
    .load()
    .await?;
    process_raw_manifests(logger, fbcode_root, use_isolation_dir, raw_manifests).await
}
