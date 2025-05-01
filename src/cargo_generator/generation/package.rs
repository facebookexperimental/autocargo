/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use anyhow::Context;
use anyhow::Result;
use cargo_util_schemas::manifest::StringOrBool;
use itertools::Itertools;

use super::GenerationInput;
use super::generate_field;
use super::generate_path_field;
use super::product::generate_product_name;
use crate::buck_processing::AutocargoCargoTomlConfig;
use crate::buck_processing::AutocargoPackageConfig;
use crate::buck_processing::RawBuckManifest;
use crate::cargo_manifest::Package;
use crate::config::PackageDefaults;
use crate::paths::CargoTomlPath;
use crate::paths::TargetsPath;

impl GenerationInput<'_> {
    /// Package name if not provided via cargo_toml_config will be computed based
    /// on lib or else on bin (if exactly one) or else test (if no bins and
    /// exactly one). If all fails then package name will be made up from
    /// targets_path.
    pub(super) fn generate_package_name(&self, targets_path: &TargetsPath) -> String {
        generate_package_name(
            targets_path,
            self.cargo_toml_config().package.name.as_ref(),
            if let Some(lib) = self.lib {
                Some(lib.raw())
            } else if let Ok(bin) = self.bins.iter().exactly_one() {
                Some(bin.raw())
            } else if let (Ok(test), true) = (self.tests.iter().exactly_one(), self.bins.is_empty())
            {
                Some(test.raw())
            } else {
                None
            },
        )
    }
}

/// Only libraries can be dependencies, so it is fine to assume that the provided
/// "raw" is a [lib] and it's name can be used to compute dependency's package
/// name.
pub fn generate_dependency_package_name(
    targets_path: &TargetsPath,
    raw: &RawBuckManifest,
) -> String {
    generate_package_name(
        targets_path,
        raw.autocargo
            .cargo_toml_config
            .as_ref()
            .and_then(|conf| conf.package.name.as_ref()),
        Some(raw),
    )
}

fn generate_package_name(
    targets_path: &TargetsPath,
    name_from_package_config: Option<&String>,
    maybe_raw: Option<&RawBuckManifest>,
) -> String {
    name_from_package_config
        .cloned()
        .or_else(|| maybe_raw.map(generate_product_name))
        // This happens only when the package doesn't contain a [lib] section,
        // so there is no risk of others depending on this package, but still
        // we have to provide a unique-ish identifier, so create one from targets_path
        .unwrap_or_else(|| {
            format!("{}", targets_path.as_dir().as_ref().display()).replace('/', "_")
        })
}

pub fn generate_dependency_package_version(
    package_config: Option<&AutocargoCargoTomlConfig>,
    package_defaults: &PackageDefaults,
) -> String {
    generate_field(
        package_config.map_or(&None, |conf| &conf.package.version),
        &package_defaults.version,
    )
}

fn generate_package_version(
    first_choice: &Option<String>,
    package_defaults: &PackageDefaults,
) -> String {
    generate_field(first_choice, &package_defaults.version)
}

/// Generate package based on provided input. Not-None Autocargo fields take
/// precedence over PackageDefaults fields.
pub fn generate_package(
    name: String,
    package_config: &AutocargoPackageConfig,
    package_defaults: &PackageDefaults,
    cargo_toml_path: &CargoTomlPath,
    is_thrift: bool,
) -> Result<Package> {
    let AutocargoPackageConfig {
        name: _,
        version,
        authors,
        edition,
        rust_version,
        description,
        documentation,
        readme,
        homepage,
        repository,
        license,
        license_file,
        keywords,
        categories,
        workspace,
        build,
        links,
        exclude,
        include,
        publish,
        metadata,
        default_run,
        autobins,
        autoexamples,
        autotests,
        autobenches,
    } = package_config;

    let PackageDefaults {
        version: _,
        authors: default_authors,
        edition: default_edition,
        rust_version: default_rust_version,
        description: default_description,
        documentation: default_documentation,
        readme: default_readme,
        homepage: default_homepage,
        repository: default_repository,
        license: default_license,
        license_file: default_license_file,
        keywords: default_keywords,
        categories: default_categories,
        workspace: default_workspace,
        links: default_links,
        exclude: default_exclude,
        include: default_include,
        publish: default_publish,
        metadata: default_metadata,
    } = package_defaults;

    Ok(Package {
        name,
        version: generate_package_version(version, package_defaults),
        authors: generate_field(authors, default_authors),
        edition: generate_field(edition, default_edition),
        rust_version: generate_field(rust_version, default_rust_version),
        description: generate_field(description, default_description),
        documentation: generate_field(documentation, default_documentation),
        readme: generate_path_field(readme, default_readme, cargo_toml_path)
            .context("For field readme")?,
        homepage: generate_field(homepage, default_homepage),
        repository: generate_field(repository, default_repository),
        license: generate_field(license, default_license),
        license_file: generate_path_field(license_file, default_license_file, cargo_toml_path)
            .context("For field license-file")?,
        keywords: generate_field(keywords, default_keywords),
        categories: generate_field(categories, default_categories),
        workspace: generate_path_field(workspace, default_workspace, cargo_toml_path)
            .context("For field workspace")?,
        build: build.clone().or_else(|| {
            if is_thrift {
                Some(StringOrBool::String("thrift_build.rs".to_owned()))
            } else {
                None
            }
        }),
        links: generate_field(links, default_links),
        exclude: generate_field(exclude, default_exclude),
        include: generate_field(include, default_include),
        publish: generate_field(publish, default_publish),
        metadata: generate_field(metadata, default_metadata),
        default_run: default_run.clone(),
        autobins: *autobins,
        autoexamples: *autoexamples,
        autotests: *autotests,
        autobenches: *autobenches,
    })
}
