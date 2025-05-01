/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

mod consolidated_dependencies;
mod dependencies;
mod package;
mod product;
mod thrift_additional;

use std::borrow::Borrow;
use std::collections::HashMap;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::ensure;
use cargo_toml::FeatureSet;
use itertools::Itertools;
use pathdiff::diff_paths;
use slog::Logger;
use thrift_additional::generate_additional_thrift_files;

use self::consolidated_dependencies::ConsolidatedDependencies;
use self::dependencies::Dependencies;
use self::dependencies::DependenciesGenerator;
use self::r#impl::BoxConfig;
use self::r#impl::BoxExtraBuckDeps;
use self::package::generate_package;
use self::product::generate_product;
use super::CargoGenerator;
use crate::buck_processing::AutocargoCargoTomlConfig;
use crate::buck_processing::BuckManifest;
use crate::buck_processing::ExtraBuckDependencies;
use crate::buck_processing::FbconfigRuleType;
use crate::cargo_generator::GENERATED_PREAMBLE;
use crate::cargo_manifest::Manifest;
use crate::config::OssGitConfig;
use crate::config::ProjectConf;
use crate::config::ProjectConfDefaults;
use crate::paths::CargoTomlPath;
use crate::paths::PathInFbcode;
use crate::paths::TargetsPath;

// The cargo key for default features
const DEFAULT: &str = "default";

fn compute_cargo_toml_path(cargo_toml_dir: &PathInFbcode) -> CargoTomlPath {
    CargoTomlPath::new(cargo_toml_dir.join_to_path_in_fbcode(CargoTomlPath::filename())).unwrap()
}

#[derive(Debug)]
pub struct GenerationInput<'geninp> {
    cargo_toml_config: BoxConfig<'geninp>,
    extra_buck_dependencies: BoxExtraBuckDeps<'geninp>,
    lib: Option<&'geninp BuckManifest>,
    bins: Vec<&'geninp BuckManifest>,
    tests: Vec<&'geninp BuckManifest>,
}

impl<'geninp> GenerationInput<'geninp> {
    fn cargo_toml_config(&self) -> &AutocargoCargoTomlConfig {
        (*self.cargo_toml_config).borrow()
    }

    fn extra_buck_dependencies(&self) -> &ExtraBuckDependencies {
        (*self.extra_buck_dependencies).borrow()
    }

    /// Prepares GenerationInput by investigationg provided BuckManifests,
    /// splitting them into appropriate lib/bin/test bucket making sure that
    /// there is at most one lib rule and at most one rule that defines
    /// cargo_toml_config field. Furthermore if there is exactly one lib rule
    /// then only it is permitted to define cargo_toml_config. This is especially
    /// important since dependencies are just raw [lib] manifests, so they must
    /// contain all data for package name computations.
    pub fn new(manifests: impl IntoIterator<Item = &'geninp BuckManifest>) -> Result<Self> {
        let manifests: Vec<_> = manifests.into_iter().collect();

        let try_self: Result<_> = try {
            let err_msg_pfx = "When multiple rules map into a single Cargo.toml file";

            let (cargo_toml_config, extra_buck_dependencies) = {
                let (configs, names): (Vec<_>, Vec<_>) = manifests
                    .iter()
                    .filter_map(|manifest| {
                        manifest
                            .raw()
                            .autocargo
                            .cargo_toml_config
                            .as_ref()
                            .map(|conf| {
                                (
                                    (conf, manifest.extra_buck_dependencies()),
                                    &manifest.raw().name,
                                )
                            })
                    })
                    .unzip();

                ensure!(
                    names.len() < 2,
                    "{} only one of them might define \
                    autocargo.cargo_toml_config. Rules found with the config: \
                    {:?}",
                    err_msg_pfx,
                    names
                );

                if let (Some(rule_with_config), Some(lib)) = (
                    names.first(),
                    manifests.iter().find(|manifest| {
                        *manifest.fbconfig_rule_type() == FbconfigRuleType::RustLibrary
                    }),
                ) {
                    ensure!(
                        lib.raw().autocargo.cargo_toml_config.is_some(),
                        "{} and one of them is rust_library then only that \
                            rule is permitted to define \
                            autocargo.cargo_toml_config. Rule found with the \
                            config: {} the library rule: {}",
                        err_msg_pfx,
                        rule_with_config,
                        lib.raw().name
                    );
                }

                match configs.into_iter().next() {
                    Some((config, extra)) => (
                        Box::new(config) as BoxConfig<'geninp>,
                        Box::new(extra) as BoxExtraBuckDeps<'geninp>,
                    ),
                    None => (
                        Box::<AutocargoCargoTomlConfig>::default() as BoxConfig<'geninp>,
                        Box::<ExtraBuckDependencies>::default() as BoxExtraBuckDeps<'geninp>,
                    ),
                }
            };

            let mut type_to_manifests = manifests
                .into_iter()
                .map(|manifest| (*manifest.fbconfig_rule_type(), manifest))
                .into_group_map();

            let lib = {
                let libs = type_to_manifests
                    .remove(&FbconfigRuleType::RustLibrary)
                    .unwrap_or_default();

                ensure!(
                    libs.len() < 2,
                    "{} there can be at most one rust_library rule. Library \
                    rules found: {:?}",
                    err_msg_pfx,
                    libs.iter()
                        .map(|manifest| &manifest.raw().name)
                        .collect::<Vec<_>>()
                );

                libs.first().cloned() // cloned && -> &
            };

            let bins = type_to_manifests
                .remove(&FbconfigRuleType::RustBinary)
                .unwrap_or_default();
            let tests = type_to_manifests
                .remove(&FbconfigRuleType::RustUnittest)
                .unwrap_or_default();

            Self {
                cargo_toml_config,
                extra_buck_dependencies,
                lib,
                bins,
                tests,
            }
        };
        try_self.context(
            "One solution might be to map your rules into different Cargo.toml \
            files by setting the autocargo.cargo_toml_dir parameter on the rule.",
        )
    }

    /// Identifier that might be put in the Cargo.toml file to know what rules
    /// were it generated from.
    pub fn generation_identifier(&self, targets_path: &TargetsPath) -> String {
        let targets: Vec<_> = self
            .lib
            .iter()
            .chain(self.bins.iter())
            .chain(self.tests.iter())
            .map(|manifest| manifest.raw().name.as_str())
            .sorted()
            .collect();
        let targets = if targets.len() > 1 {
            format!("[{}]", targets.join(","))
        } else {
            targets.join(",")
        };
        format!("//{}:{}", targets_path.as_dir().as_ref().display(), targets)
    }

    /// Generate a Cargo.toml manifest.
    pub fn generate_manifest(
        &self,
        logger: &Logger,
        cargo_generator: &CargoGenerator<'_>,
        conf: &ProjectConf,
        targets_path: &TargetsPath,
        cargo_toml_dir: &PathInFbcode,
    ) -> Result<(CargoTomlPath, Manifest)> {
        self.generate_manifest_impl(
            logger,
            cargo_generator,
            conf,
            targets_path,
            cargo_toml_dir,
            None,
        )
    }

    /// Generate a oss version of Cargo.toml manifest if the project configures
    /// a oss_git_config.public_cargo_dir.
    pub fn generate_oss_manifest(
        &self,
        logger: &Logger,
        cargo_generator: &CargoGenerator<'_>,
        conf: &ProjectConf,
        targets_path: &TargetsPath,
        cargo_toml_dir: &PathInFbcode,
    ) -> Result<Option<(CargoTomlPath, Manifest)>> {
        conf.oss_git_config()
            .as_ref()
            .and_then(|oss_git_config| {
                oss_git_config
                    .public_cargo_dir
                    .as_ref()
                    .map(|public_cargo_dir| (oss_git_config, public_cargo_dir))
            })
            .map(|(oss_git_config, public_cargo_dir)| -> Result<_> {
                let (cargo_toml_path, manifest) = self.generate_manifest_impl(
                    logger,
                    cargo_generator,
                    conf,
                    targets_path,
                    cargo_toml_dir,
                    Some(oss_git_config),
                )?;

                let cargo_toml_path = {
                    // We have to put the cargo_toml_path under public_cargo_dir
                    let public_cargo_dir_parent =
                        public_cargo_dir.as_ref().parent().ok_or_else(|| {
                            anyhow!(
                                "Failed to get parent of public_cargo_dir: {:?}",
                                public_cargo_dir
                            )
                        })?;
                    let cargo_toml_relative_path = cargo_toml_path
                        .as_file()
                        .as_ref()
                        .strip_prefix(public_cargo_dir_parent)
                        .with_context(|| {
                            anyhow!(
                                "Failed to strip prefix {} from {:?}, make \
                                sure project's generated Cargo.toml files are \
                                all inside of public_cargo_dir parent directory",
                                public_cargo_dir_parent.display(),
                                cargo_toml_path
                            )
                        })?;
                    CargoTomlPath::new(
                        public_cargo_dir.join_to_path_in_fbcode(cargo_toml_relative_path),
                    )?
                };

                Ok((cargo_toml_path, manifest))
            })
            .transpose()
            .with_context(|| format!("While generating oss manifest for project {}", conf.name()))
    }

    fn generate_manifest_impl(
        &self,
        logger: &Logger,
        cargo_generator: &CargoGenerator<'_>,
        conf: &ProjectConf,
        targets_path: &TargetsPath,
        cargo_toml_dir: &PathInFbcode,
        oss_git_config: Option<&OssGitConfig>,
    ) -> Result<(CargoTomlPath, Manifest)> {
        let result: Result<_> = try {
            let cargo_toml_path = compute_cargo_toml_path(cargo_toml_dir);

            let AutocargoCargoTomlConfig {
                cargo_features,
                package,
                workspace,
                extra_buck_dependencies: _,
                dependencies_override,
                features: _,
                lib,
                bin,
                test,
                bench,
                example,
                patch_generation,
                patch,
                profile,
                lints,
            } = self.cargo_toml_config();

            let ProjectConfDefaults {
                cargo_features: default_cargo_features,
                package: default_package,
                patch_generation: default_patch_generation,
                patch: default_patch,
                profile: default_profile,
            } = conf.defaults();

            let features = self.generate_features();

            let features = match (oss_git_config, features.get(DEFAULT)) {
                (Some(oss_git_config), Some(default_features)) => {
                    let mut default_features = default_features.clone();
                    default_features.retain(|f| {
                        for strip in oss_git_config.default_features_to_strip.iter() {
                            if f == strip || f.ends_with(&("/".to_owned() + strip)) {
                                return false;
                            }
                        }
                        true
                    });
                    let mut features = features.clone();
                    features.insert(DEFAULT.to_string(), default_features);
                    features
                }
                _ => features,
            };

            let consolidated_dependencies = ConsolidatedDependencies::new(
                logger,
                cargo_generator,
                targets_path,
                &self.lib,
                &self.bins,
                &self.tests,
            );

            let Dependencies {
                dependencies,
                dev_dependencies,
                build_dependencies,
                target,
            } = DependenciesGenerator {
                cargo_generator,
                features: &features,
                cargo_toml_path: &cargo_toml_path,
                consolidated_dependencies,
                extra_buck_dependencies: self.extra_buck_dependencies(),
                dependencies_override,
                oss_git_config,
            }
            .generate()
            .context("In dependencies generation")?;

            let prefix_comment = format!(
                "# {GENERATED_PREAMBLE} from {}\n\n",
                self.generation_identifier(targets_path),
            );

            let manifest = Manifest {
                prefix_comment: Some(prefix_comment),

                cargo_features: generate_field(cargo_features, default_cargo_features),
                package: Some(
                    generate_package(
                        self.generate_package_name(targets_path),
                        package,
                        default_package,
                        &cargo_toml_path,
                        self.lib
                            .as_ref()
                            .map(|lib| lib.thrift_config().is_some())
                            .unwrap_or_default(),
                    )
                    .context("In package generation")?,
                ),

                lib: self
                    .lib
                    .map(|manifest| {
                        generate_product(
                            *manifest.fbconfig_rule_type(),
                            manifest.raw(),
                            targets_path,
                            &cargo_toml_path,
                        )
                        .with_context(|| {
                            format!("In lib '{}' product generation", manifest.raw().name)
                        })
                    })
                    .transpose()?
                    .or_else(|| lib.clone()),
                bin: self
                    .bins
                    .iter()
                    .map(|manifest| {
                        generate_product(
                            *manifest.fbconfig_rule_type(),
                            manifest.raw(),
                            targets_path,
                            &cargo_toml_path,
                        )
                        .with_context(|| {
                            format!("In bin '{}' product generation", manifest.raw().name)
                        })
                    })
                    .chain(bin.iter().cloned().map(Ok))
                    .collect::<Result<_>>()?,
                example: example.clone(),
                test: self
                    .tests
                    .iter()
                    .map(|manifest| {
                        generate_product(
                            *manifest.fbconfig_rule_type(),
                            manifest.raw(),
                            targets_path,
                            &cargo_toml_path,
                        )
                        .with_context(|| {
                            format!("In test '{}' product generation", manifest.raw().name)
                        })
                    })
                    .chain(test.iter().cloned().map(Ok))
                    .collect::<Result<_>>()?,
                bench: bench.clone(),

                dependencies,
                dev_dependencies,
                build_dependencies,
                target,

                features,
                patch: cargo_generator
                    .generate_patch(
                        patch_generation
                            .as_ref()
                            .unwrap_or(default_patch_generation),
                        default_patch.iter().chain(patch.iter()),
                    )
                    .context("In patch generation")?,
                profile: generate_field(profile, default_profile),
                workspace: workspace.clone(),
                lints: lints.clone(),
            };
            (cargo_toml_path, manifest)
        };

        result.with_context(|| {
            format!(
                "While generating cargo manifest for {targets_path:?} in dir {cargo_toml_dir:?}",
            )
        })
    }

    /// If not provided via cargo_toml_config the features will be taken from
    /// combined rules' default_features attributes.
    fn generate_features(&self) -> FeatureSet {
        if let Some(features) = self.cargo_toml_config().features.clone() {
            features
        } else {
            let default_features: Vec<_> = self
                .lib
                .iter()
                .chain(self.bins.iter())
                .chain(self.tests.iter())
                .flat_map(|manifest| {
                    let rust_config = &manifest.raw().rust_config;
                    rust_config
                        .features
                        .iter()
                        .chain(rust_config.test_features.iter())
                })
                .cloned()
                .collect();

            let mut features = FeatureSet::default();
            if !default_features.is_empty() {
                features.extend(default_features.iter().filter_map(|f| {
                    if f.contains('/') {
                        None
                    } else {
                        Some((f.clone(), Vec::new()))
                    }
                }));
                features.insert(DEFAULT.to_owned(), default_features);
            }
            features
        }
    }

    pub fn generate_additional_files(
        &self,
        targets_path: &TargetsPath,
        cargo_toml_dir: &PathInFbcode,
    ) -> Result<HashMap<PathInFbcode, String>> {
        let cargo_toml_path = compute_cargo_toml_path(cargo_toml_dir);

        if let Some(lib) = &self.lib {
            if let (Some(thrift_config), Some(autocargo_thrift)) =
                (lib.thrift_config(), &lib.raw().autocargo.thrift)
            {
                return generate_additional_thrift_files(
                    targets_path,
                    &cargo_toml_path,
                    thrift_config,
                    autocargo_thrift,
                );
            }
        }
        Ok(HashMap::new())
    }
}

fn generate_field<T: Clone>(first_choice: &Option<T>, second_choice: &T) -> T {
    first_choice
        .clone()
        .unwrap_or_else(|| second_choice.clone())
}

fn generate_path_field(
    first_choice: &Option<Option<String>>,
    second_choice: &Option<PathInFbcode>,
    cargo_toml_path: &CargoTomlPath,
) -> Result<Option<String>> {
    let val = if let Some(val) = first_choice.clone() {
        val
    } else if let Some(path) = second_choice.clone() {
        Some(
            diff_paths(path.as_ref(), cargo_toml_path.as_dir().as_ref())
                .and_then(|path| path.to_str().map(|s| s.to_owned()))
                .ok_or_else(|| {
                    anyhow!(
                        "Couldn't construct a relative path between project \
                        configured {:?} and {:?}. Did you provide a path \
                        relative to root of fbcode?",
                        path,
                        cargo_toml_path,
                    )
                })?,
        )
    } else {
        None
    };

    Ok(val)
}

mod r#impl {
    use std::fmt::Debug;

    use super::*;

    /// This is just a workaround for the inability of having (dyn T + U).
    pub trait ConfTrait: Borrow<AutocargoCargoTomlConfig> + Debug {}
    impl<T: Borrow<AutocargoCargoTomlConfig> + Debug> ConfTrait for T {}

    /// This is just a workaround for the inability of having (dyn T + U).
    pub trait ExtraBuckDepsTrait: Borrow<ExtraBuckDependencies> + Debug {}
    impl<T: Borrow<ExtraBuckDependencies> + Debug> ExtraBuckDepsTrait for T {}

    /// Wraps both a value and a reference to AutocargoCargoTomlConfig.
    pub type BoxConfig<'a> = Box<dyn ConfTrait + 'a>;

    /// Wraps both a value and a reference to ExtraBuckDependencies.
    pub type BoxExtraBuckDeps<'a> = Box<dyn ExtraBuckDepsTrait + 'a>;
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::paths::PathInFbcode;

    #[test]
    fn compute_cargo_toml_path_test() {
        let cp = |s: &str| CargoTomlPath::new(PathInFbcode::new_mock(s)).unwrap();

        assert_eq!(
            compute_cargo_toml_path(&PathInFbcode::new_mock("")),
            cp("Cargo.toml"),
        );

        assert_eq!(
            compute_cargo_toml_path(&PathInFbcode::new_mock("biz/fiz")),
            cp("biz/fiz/Cargo.toml"),
        );

        assert_eq!(
            compute_cargo_toml_path(&PathInFbcode::new_mock("../biz")),
            cp("../biz/Cargo.toml"),
        );
    }
}
