/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::collections::HashMap;
use std::collections::HashSet;

use maplit::hashmap;
use slog::Logger;
use slog::trace;

use crate::buck_processing::BuckDependency;
use crate::buck_processing::BuckManifest;
use crate::buck_processing::CODEGEN_INCLUDER_PROC_MACRO_RULE;
use crate::buck_processing::OsDepsPlatform;
use crate::buck_processing::RawBuckManifest;
use crate::buck_processing::RawFbconfigRuleType;
use crate::buck_processing::THRIFT_COMPILER_RULE;
use crate::cargo_generator::CargoGenerator;
use crate::paths::TargetsPath;

/// Dependencies in Buck are all over the place - in named, test, platform or
/// regular dependencies - and potentially spread through many rules that map
/// into a single Cargo.toml file which has dependencies per-package rather than
/// per-target. This structures is for holding a condensed list of dependencies
/// for further processing.
#[derive(Debug)]
pub struct ConsolidatedDependencies<'a> {
    pub deps: Deps<'a>,
    pub named_deps: NamedDeps<'a>,
    pub os_deps: HashMap<OsDepsPlatform, Deps<'a>>,
    pub test_deps: Deps<'a>,
    pub test_named_deps: NamedDeps<'a>,
    pub test_os_deps: HashMap<OsDepsPlatform, Deps<'a>>,
    /// Build deps don't currently exist on Buck rules, but we want to store e.g.
    /// thrift build deps here.
    pub build_deps: Deps<'a>,
}

impl<'a> ConsolidatedDependencies<'a> {
    pub fn new(
        logger: &Logger,
        cargo_generator: &CargoGenerator<'_>,
        targets_path: &TargetsPath,
        lib: &Option<&'a BuckManifest>,
        bins: &[&'a BuckManifest],
        tests: &[&'a BuckManifest],
    ) -> Self {
        // The rules that are currently being processed. We don't want to depend
        // on ourselves, e.g. when a test rule depends on lib rule, so we keep
        // track here and make sure we remove those rules from consolidated
        // deps.
        let local_rules: HashSet<_> = lib
            .iter()
            .chain(bins.iter())
            .chain(tests.iter())
            .map(|manifest| manifest.raw().name.as_str())
            .collect();

        let thrift_config = lib.and_then(|lib| lib.thrift_config().as_ref());

        // The [dependency] section is for lib and bins
        let lib_and_bins = lib.iter().chain(bins.iter());
        let deps = {
            let mut deps = Deps::from_deps(
                logger,
                cargo_generator,
                targets_path,
                &local_rules,
                lib_and_bins
                    .clone()
                    .flat_map(|manifest| manifest.deps().iter()),
            );
            if let Some(thrift_config) = thrift_config {
                deps.fbcode.insert(
                    FbcodeRule::unsafe_from_buck_rule(
                        &CODEGEN_INCLUDER_PROC_MACRO_RULE.path,
                        &CODEGEN_INCLUDER_PROC_MACRO_RULE.name,
                    ),
                    &*thrift_config.codegen_includer_proc_macro,
                );
            }
            deps
        };
        let named_deps = NamedDeps::from_named_deps(
            logger,
            cargo_generator,
            targets_path,
            &local_rules,
            lib_and_bins
                .clone()
                .flat_map(|manifest| manifest.named_deps().iter()),
        );

        // The [dev-dependency] section is for tests and cfg(test) deps of lib
        // or bins
        let test_deps = Deps::from_deps(
            logger,
            cargo_generator,
            targets_path,
            &local_rules,
            lib_and_bins
                .clone()
                .flat_map(|manifest| manifest.test_deps().iter())
                .chain(tests.iter().flat_map(|manifest| manifest.deps().iter())),
        );
        let test_named_deps = NamedDeps::from_named_deps(
            logger,
            cargo_generator,
            targets_path,
            &local_rules,
            lib_and_bins
                .clone()
                .flat_map(|manifest| manifest.test_named_deps().iter())
                .chain(
                    tests
                        .iter()
                        .flat_map(|manifest| manifest.named_deps().iter()),
                ),
        );

        let (os_deps, test_os_deps) = enum_iterator::all::<OsDepsPlatform>()
            .map(|os| {
                let os_deps = Deps::from_deps(
                    logger,
                    cargo_generator,
                    targets_path,
                    &local_rules,
                    lib_and_bins
                        .clone()
                        .flat_map(|manifest| manifest.os_deps().get(&os).into_iter().flatten()),
                );

                let test_os_deps = Deps::from_deps(
                    logger,
                    cargo_generator,
                    targets_path,
                    &local_rules,
                    lib_and_bins
                        .clone()
                        .flat_map(|manifest| manifest.test_os_deps().get(&os).into_iter().flatten())
                        .chain(tests.iter().flat_map(|manifest| {
                            manifest.os_deps().get(&os).into_iter().flatten()
                        })),
                );

                ((os, os_deps), (os, test_os_deps))
            })
            .unzip();

        let build_deps = Deps {
            third_party: HashSet::new(),
            fbcode: if let Some(thrift_config) = thrift_config {
                hashmap! {
                    FbcodeRule::unsafe_from_buck_rule(
                        &THRIFT_COMPILER_RULE.path,
                        &THRIFT_COMPILER_RULE.name,
                    ) => &*thrift_config.thrift_compiler
                }
            } else {
                HashMap::new()
            },
        };

        ConsolidatedDependencies {
            deps,
            named_deps,
            os_deps,
            test_deps,
            test_named_deps,
            test_os_deps,
            build_deps,
        }
    }
}

mod r#impl {
    use getset::Getters;

    use super::*;

    /// Putting FbcodeRule in a separate module makes sure its fields are private
    /// and forces the use of constructor.
    /// The purpose of this structure is to be a key in a hashmap for
    /// identification of a RawBuckManifest, but that still provides access to
    /// the corresponding targets_path.
    #[derive(Debug, Eq, PartialEq, Hash, Getters)]
    pub struct FbcodeRule<'a> {
        #[getset(get = "pub")]
        targets_path: &'a TargetsPath,
        buck_name: &'a str,
    }

    impl<'a> FbcodeRule<'a> {
        /// This method filters ignored rules, rules not covered by any
        /// project or rules that are not rust_library.
        pub fn try_new(
            logger: &Logger,
            cargo_generator: &CargoGenerator<'_>,
            targets_path: &'a TargetsPath,
            raw: &'a RawBuckManifest,
        ) -> Option<Self> {
            if raw.autocargo.ignore_rule
                || !cargo_generator
                    .targets_to_projects()
                    .contains_key(targets_path)
            {
                None
            } else if raw.fbconfig_rule_type == RawFbconfigRuleType::RustLibrary {
                Some(Self {
                    targets_path,
                    buck_name: raw.name.as_str(),
                })
            } else {
                trace!(
                    logger,
                    "Rule {} from {:?} was listed as a dependency, but it is not a \
                    rust_library rule. In Cargo you cannot depend on a non-library, \
                    so ignoring it.",
                    raw.name,
                    targets_path,
                );
                None
            }
        }

        /// Make sure on your own that it is valid to create such an fbcode rule.
        pub fn unsafe_from_buck_rule(targets_path: &'a TargetsPath, buck_name: &'a str) -> Self {
            Self {
                targets_path,
                buck_name,
            }
        }
    }
}
pub use r#impl::FbcodeRule;

#[derive(Debug, Default)]
pub struct Deps<'a> {
    pub third_party: HashSet<&'a str>,
    pub fbcode: HashMap<FbcodeRule<'a>, &'a RawBuckManifest>,
}

impl<'a> Deps<'a> {
    fn from_deps(
        logger: &Logger,
        cargo_generator: &CargoGenerator<'_>,
        local_targets_path: &TargetsPath,
        local_rules: &HashSet<&str>,
        deps: impl Iterator<Item = &'a BuckDependency> + Clone,
    ) -> Self {
        Self {
            third_party: deps
                .clone()
                .filter_map(|dep| match dep {
                    BuckDependency::ThirdPartyCrate(name) => Some(name.as_str()),
                    BuckDependency::FbcodeCrate(_, _) => None,
                })
                .collect(),
            fbcode: deps
                .filter_map(|dep| match dep {
                    BuckDependency::ThirdPartyCrate(_) => None,
                    BuckDependency::FbcodeCrate(targets_path, raw) => {
                        if &**targets_path == local_targets_path
                            && local_rules.contains(raw.name.as_str())
                        {
                            None
                        } else {
                            FbcodeRule::try_new(logger, cargo_generator, targets_path, raw)
                                .map(|rule| (rule, &**raw))
                        }
                    }
                })
                .collect(),
        }
    }
}

#[derive(Debug, Default)]
pub struct NamedDeps<'a> {
    pub third_party: HashSet<(&'a str, &'a str)>,
    pub fbcode: HashMap<(&'a str, FbcodeRule<'a>), &'a RawBuckManifest>,
}

impl<'a> NamedDeps<'a> {
    fn from_named_deps(
        logger: &Logger,
        cargo_generator: &CargoGenerator<'_>,
        local_targets_path: &TargetsPath,
        local_rules: &HashSet<&str>,
        named_deps: impl Iterator<Item = (&'a String, &'a BuckDependency)> + Clone,
    ) -> Self {
        Self {
            third_party: named_deps
                .clone()
                .filter_map(|(alias, dep)| match dep {
                    BuckDependency::ThirdPartyCrate(name) => Some((alias.as_str(), name.as_str())),
                    BuckDependency::FbcodeCrate(_, _) => None,
                })
                .collect(),
            fbcode: named_deps
                .filter_map(|(alias, dep)| match dep {
                    BuckDependency::ThirdPartyCrate(_) => None,
                    BuckDependency::FbcodeCrate(targets_path, raw) => {
                        if &**targets_path == local_targets_path
                            && local_rules.contains(raw.name.as_str())
                        {
                            None
                        } else {
                            FbcodeRule::try_new(logger, cargo_generator, targets_path, raw)
                                .map(|rule| ((alias.as_str(), rule), &**raw))
                        }
                    }
                })
                .collect(),
        }
    }
}
