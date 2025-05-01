// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::LazyLock;

use anyhow::Result;
use enum_iterator::Sequence;
use getset::Getters;
use itertools::Itertools;
use slog::Logger;
use slog::trace;

use super::ProcessOutput;
use super::loader::BuckManifestLoader;
use super::loader::ThriftCratemapLoader;
use super::raw_manifest::RawBuckDependencyOverride;
use super::raw_manifest::RawBuckManifest;
use super::raw_manifest::RawBuckManifestDependencies;
use super::raw_manifest::RawBuckTargetDependencies;
use super::raw_manifest::RawExtraBuckDependencies;
use super::raw_manifest::RawFbconfigRuleType;
use super::raw_manifest::RawOsDepsPlatform;
use super::rules::BuckRuleParseOutput;
use super::rules::FbcodeBuckRule;
use crate::cargo_manifest::TargetKey;
use crate::paths::FbcodeRoot;
use crate::paths::TargetsPath;
use crate::util::command_runner::MockableCommandRunner;

/// Rule identifying thrift_compiler, used by thrift generation.
pub static THRIFT_COMPILER_RULE: LazyLock<FbcodeBuckRule> = LazyLock::new(|| FbcodeBuckRule {
    path: TargetsPath::from_buck_rule("common/rust/shed/thrift_compiler"),
    name: "lib".to_owned(),
});

/// Rule identifying codegen_includer_proc_macro, used by thrift generation.
pub static CODEGEN_INCLUDER_PROC_MACRO_RULE: LazyLock<FbcodeBuckRule> =
    LazyLock::new(|| FbcodeBuckRule {
        path: TargetsPath::from_buck_rule("common/rust/shed/codegen_includer_proc_macro"),
        name: "codegen_includer_proc_macro".to_owned(),
    });

/// Enum describing type of rule that the manifest describes. Includes only the
/// ones supported by this library.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub enum FbconfigRuleType {
    /// Binary
    RustBinary,
    /// Library
    RustLibrary,
    /// Unittest
    RustUnittest,
}

impl FbconfigRuleType {
    fn try_from_raw(
        logger: &'_ Logger,
        targets_path: &'_ TargetsPath,
        value: &'_ RawFbconfigRuleType,
    ) -> Option<Self> {
        match value {
            RawFbconfigRuleType::RustBinary => Some(Self::RustBinary),
            RawFbconfigRuleType::RustLibrary => Some(Self::RustLibrary),
            RawFbconfigRuleType::RustUnittest => Some(Self::RustUnittest),
            RawFbconfigRuleType::RustBindgenLibrary | RawFbconfigRuleType::Other => {
                trace!(
                    logger,
                    "Build file at {}: Rule type {:#?} is not supported",
                    targets_path.as_dir().as_ref().display(),
                    value
                );
                None
            }
        }
    }
}

/// Enum describing platform for which a given dependency is added. Includes only
/// the ones supported by this library.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Sequence)]
pub enum OsDepsPlatform {
    /// Linux
    Linux,
    /// Macos
    Macos,
    /// Windows
    Windows,
}

impl OsDepsPlatform {
    fn try_from_raw(
        logger: &'_ Logger,
        targets_path: &'_ TargetsPath,
        value: &'_ RawOsDepsPlatform,
    ) -> Option<Self> {
        match value {
            RawOsDepsPlatform::Linux => Some(Self::Linux),
            RawOsDepsPlatform::Macos => Some(Self::Macos),
            RawOsDepsPlatform::Windows => Some(Self::Windows),
            RawOsDepsPlatform::Other => {
                trace!(
                    logger,
                    "Build file at {}: Os platform {:#?} is not supported",
                    targets_path.as_dir().as_ref().display(),
                    value
                );
                None
            }
        }
    }

    /// Returns a cfg directive that defines the target platform for Cargo.
    pub fn to_cargo_target(&self) -> &'static TargetKey {
        static LINUX: LazyLock<TargetKey> =
            LazyLock::new(|| TargetKey::try_from(r#"'cfg(target_os = "linux")'"#).unwrap());
        static MACOS: LazyLock<TargetKey> =
            LazyLock::new(|| TargetKey::try_from(r#"'cfg(target_os = "macos")'"#).unwrap());
        static WINDOWS: LazyLock<TargetKey> =
            LazyLock::new(|| TargetKey::try_from(r#"'cfg(target_os = "windows")'"#).unwrap());
        match self {
            OsDepsPlatform::Linux => &LINUX,
            OsDepsPlatform::Macos => &MACOS,
            OsDepsPlatform::Windows => &WINDOWS,
        }
    }
}

/// Dependency of a crate that can be handled by this library.
#[derive(Debug)]
pub enum BuckDependency {
    /// Name of a crate from registry.
    ThirdPartyCrate(String),
    /// Path to and manifest of a dependency in fbcode.
    FbcodeCrate(Arc<TargetsPath>, Arc<RawBuckManifest>),
}

/// Processed manifest containing the original raw manifest and resolved
/// dependencies as pointers to manifests.
#[derive(Debug, Getters)]
#[getset(get = "pub")]
pub struct BuckManifest {
    /// Raw manifest as parsed from buck build output.
    raw: Arc<RawBuckManifest>,
    /// Type of the rule.
    fbconfig_rule_type: FbconfigRuleType,
    /// List of direct dependencies.
    deps: Vec<BuckDependency>,
    /// Map where the value is the dependency and the key is the name it should
    /// be renamed to.
    named_deps: HashMap<String, BuckDependency>,
    /// Dependencies that are platfrom specific.
    os_deps: HashMap<OsDepsPlatform, Vec<BuckDependency>>,
    /// Tests that excercise this rule.
    tests: Vec<BuckDependency>,
    /// Extra test dependencies.
    test_deps: Vec<BuckDependency>,
    /// Extra test named dependencies.
    test_named_deps: HashMap<String, BuckDependency>,
    /// Test dependencies that are platfrom specific.
    test_os_deps: HashMap<OsDepsPlatform, Vec<BuckDependency>>,
    /// Contains processed [RawExtraBuckDependencies], check its documentation
    /// for more.
    extra_buck_dependencies: ExtraBuckDependencies,
    /// If raw.autocargo.thrift is present then this value will contain more
    /// configuration required for generating files for thrift.
    thrift_config: Option<ThriftConfig>,
}

/// Proccessed [RawExtraBuckDependencies].
#[derive(Debug, Default)]
#[allow(missing_docs)]
pub struct ExtraBuckDependencies {
    pub deps: BuckTargetDependencies,
    pub target: BTreeMap<TargetKey, BuckTargetDependencies>,
}

/// Processed [RawBuckTargetDependencies]
#[derive(Debug, Default)]
#[allow(missing_docs)]
pub struct BuckTargetDependencies {
    pub dependencies: Vec<BuckDependencyOverride>,
    pub dev_dependencies: Vec<BuckDependencyOverride>,
    pub build_dependencies: Vec<BuckDependencyOverride>,
}

/// Processed [RawBuckDependencyOverride]
#[derive(Debug)]
#[allow(missing_docs)]
pub enum BuckDependencyOverride {
    Dep(BuckDependency),
    NamedDep(String, BuckDependency),
    RemovedDep(BuckDependency),
}

#[derive(Debug)]
/// Configuration required for generating files for thrift.
pub struct ThriftConfig {
    /// Content of the raw.autocargo.thrift.cratemap file.
    pub cratemap_content: String,
    /// This is a build dependency for thrift generated Cargo files.
    pub thrift_compiler: Arc<RawBuckManifest>,
    /// This is a runtime dependency for thrift generated Cargo files.
    pub codegen_includer_proc_macro: Arc<RawBuckManifest>,
}

/// Given map of raw manifests process their dependencies, if necessary load
/// their manifests and created the processed BuckManifest. Also return set of
/// TARGETS that were found in dependencies, but were not mentioned in the input.
pub async fn process_raw_manifests(
    logger: &'_ Logger,
    fbcode_root: &'_ FbcodeRoot,
    use_isolation_dir: bool,
    raw_manifests: HashMap<FbcodeBuckRule, RawBuckManifest>,
) -> Result<ProcessOutput> {
    let manifest_builders: HashMap<_, _> = raw_manifests
        .into_iter()
        .filter_map(|(k, v)| {
            let v = BuckManifestBuilder::from_raw_manifest(logger, &k.path, v)?;
            Some((k, v))
        })
        .collect();

    let all_raw_manifests = compute_all_raw_manifests(
        logger,
        fbcode_root,
        use_isolation_dir,
        &manifest_builders,
        MockableCommandRunner::default(),
    )
    .await?;
    let all_thrift_cratemaps = read_all_thrift_cratemaps(
        logger,
        fbcode_root,
        use_isolation_dir,
        &manifest_builders,
        MockableCommandRunner::default(),
    )
    .await?;

    Ok(process_manifest_builders(
        logger,
        manifest_builders,
        all_raw_manifests,
        all_thrift_cratemaps,
    ))
}

/// Given map of manifests find all manifests that are either in this input or
/// are mentioned in dependencies. Loads the latter using buck loader.
/// Returns values wrapped in Arc to save on space since the dependencies might
/// appear many times in rules - unverified if it actually makes a noticeable
/// difference.
async fn compute_all_raw_manifests(
    logger: &'_ Logger,
    fbcode_root: &'_ FbcodeRoot,
    use_isolation_dir: bool,
    manifest_builders: &HashMap<FbcodeBuckRule, BuckManifestBuilder>,
    cmd_runner: MockableCommandRunner,
) -> Result<HashMap<FbcodeBuckRule, (Arc<TargetsPath>, Arc<RawBuckManifest>)>> {
    let loaded_rules: HashSet<_> = manifest_builders.keys().collect();
    let dependency_rules = extract_dependencies(manifest_builders.values());
    let missing_rules = dependency_rules.difference(&loaded_rules).cloned(); // && -> & with cloned

    let raw_manifests_of_missing_rules = BuckManifestLoader::from_rust_buck_rules(
        logger,
        fbcode_root,
        use_isolation_dir,
        missing_rules,
        cmd_runner,
    )
    .await?
    .load()
    .await?;

    Ok(manifest_builders
        .iter()
        .map(|(k, v)| {
            let v = (Arc::new(k.path.clone()), v.raw.clone());
            (k.clone(), v)
        })
        .chain(raw_manifests_of_missing_rules.into_iter().map(|(k, v)| {
            let v = (Arc::new(k.path.clone()), Arc::new(v));
            (k, v)
        }))
        .collect())
}

async fn read_all_thrift_cratemaps(
    logger: &'_ Logger,
    fbcode_root: &'_ FbcodeRoot,
    use_isolation_dir: bool,
    manifest_builders: &HashMap<FbcodeBuckRule, BuckManifestBuilder>,
    cmd_runner: MockableCommandRunner,
) -> Result<HashMap<FbcodeBuckRule, String>> {
    ThriftCratemapLoader::from_rules_and_raw(
        logger,
        fbcode_root,
        use_isolation_dir,
        manifest_builders
            .iter()
            .map(|(rule, builder)| (rule, &*builder.raw)),
        cmd_runner,
    )
    .load()
    .await
}

/// Given manifests extract all rules that are within fbcode mentioned in
/// dependencies. Also include thrift specific ones.
fn extract_dependencies<'a>(
    manifest_builders: impl IntoIterator<Item = &'a BuckManifestBuilder>,
) -> HashSet<&'a FbcodeBuckRule> {
    manifest_builders
        .into_iter()
        .flat_map(
            |BuckManifestBuilder {
                 raw,
                 fbconfig_rule_type: _,
                 deps,
                 named_deps,
                 os_deps,
                 tests,
                 test_deps,
                 test_named_deps,
                 test_os_deps,
                 extra_buck_dependencies,
             }| {
                deps.iter()
                    .filter_map(UnprocessedBuckDependency::fbcode_crate)
                    .chain(
                        named_deps
                            .values()
                            .filter_map(UnprocessedBuckDependency::fbcode_crate),
                    )
                    .chain(os_deps.values().flat_map(|deps| {
                        deps.iter()
                            .filter_map(UnprocessedBuckDependency::fbcode_crate)
                    }))
                    .chain(
                        tests
                            .iter()
                            .filter_map(UnprocessedBuckDependency::fbcode_crate),
                    )
                    .chain(
                        test_deps
                            .iter()
                            .filter_map(UnprocessedBuckDependency::fbcode_crate),
                    )
                    .chain(
                        test_named_deps
                            .values()
                            .filter_map(UnprocessedBuckDependency::fbcode_crate),
                    )
                    .chain(test_os_deps.values().flat_map(|deps| {
                        deps.iter()
                            .filter_map(UnprocessedBuckDependency::fbcode_crate)
                    }))
                    .chain(if raw.autocargo.thrift.is_some() {
                        vec![&*THRIFT_COMPILER_RULE, &*CODEGEN_INCLUDER_PROC_MACRO_RULE]
                    } else {
                        Vec::new()
                    })
                    .chain(extra_buck_dependencies.fbcode_crates())
            },
        )
        .collect()
}

/// Performs the last step of processing buck manifests. Given the manifest
/// builders (intermediate result of parsing) and pre-fetched all raw manifests
/// it stiches all together via builder's build() call and return ProcessOutput
/// that contains the unprocessed_paths computed based on the difference between
/// all_raw_manifests and provided manifest_builders.
fn process_manifest_builders(
    logger: &'_ Logger,
    manifest_builders: HashMap<FbcodeBuckRule, BuckManifestBuilder>,
    all_raw_manifests: HashMap<FbcodeBuckRule, (Arc<TargetsPath>, Arc<RawBuckManifest>)>,
    all_thrift_cratemaps: HashMap<FbcodeBuckRule, String>,
) -> ProcessOutput {
    let processed_manifests = manifest_builders
        .into_iter()
        .map(|(rule, builder)| {
            let maybe_cratemap = builder
                .raw
                .autocargo
                .thrift
                .as_ref()
                .and_then(|thrift| {
                    all_thrift_cratemaps.get(&FbcodeBuckRule {
                        path: rule.path.clone(),
                        name: thrift.unsuffixed_name.clone(),
                    })
                })
                .cloned();
            (
                rule.path,
                builder.build(logger, &all_raw_manifests, maybe_cratemap),
            )
        })
        .into_group_map();

    let unprocessed_paths = all_raw_manifests
        .into_iter()
        .filter_map(|(FbcodeBuckRule { path, .. }, _)| {
            if processed_manifests.contains_key(&path) {
                None
            } else {
                Some(path)
            }
        })
        .collect();

    ProcessOutput {
        processed_manifests,
        unprocessed_paths,
    }
}

/// A structure that helps with parsing RawBuckManifest as BuckManifest by
/// holding the intermediate result after initial parsing, but before loading
/// more data from buck.
#[derive(Debug)]
struct BuckManifestBuilder {
    raw: Arc<RawBuckManifest>,
    fbconfig_rule_type: FbconfigRuleType,
    deps: Vec<UnprocessedBuckDependency>,
    named_deps: HashMap<String, UnprocessedBuckDependency>,
    os_deps: HashMap<OsDepsPlatform, Vec<UnprocessedBuckDependency>>,
    tests: Vec<UnprocessedBuckDependency>,
    test_deps: Vec<UnprocessedBuckDependency>,
    test_named_deps: HashMap<String, UnprocessedBuckDependency>,
    extra_buck_dependencies: UnprocessedExtraBuckDependencies,
    test_os_deps: HashMap<OsDepsPlatform, Vec<UnprocessedBuckDependency>>,
}

impl BuckManifestBuilder {
    /// Given raw manifest process its dependencies into
    /// [UnprocessedBuckDependency].
    fn from_raw_manifest(
        logger: &'_ Logger,
        targets_path: &'_ TargetsPath,
        raw: RawBuckManifest,
    ) -> Option<Self> {
        let fbconfig_rule_type =
            FbconfigRuleType::try_from_raw(logger, targets_path, &raw.fbconfig_rule_type)?;

        let raw = Arc::new(raw);

        let mut rule_parse =
            |rule: &_| UnprocessedBuckDependency::try_from_rule(logger, targets_path, rule);

        let RawBuckManifestDependencies {
            deps,
            named_deps,
            os_deps,
            tests,
            test_deps,
            test_named_deps,
            test_os_deps,
        } = &raw.dependencies;

        let deps = deps.iter().filter_map(rule_parse).collect();
        let named_deps = named_deps
            .iter()
            .filter_map(|(k, v)| rule_parse(v).map(|v| (k.clone(), v)))
            .collect();
        let os_deps = os_deps
            .iter()
            .filter_map(|(k, vs)| {
                let k = OsDepsPlatform::try_from_raw(logger, targets_path, k)?;
                let vs = vs.iter().filter_map(rule_parse);
                // The raw OsDeps are of type Vec<(OsDepsPlatform, Vec<T>>), so
                // the OsDeps might be not unique. That is why here we are
                // creating Iterator of (OsDepsPlatform, T) which is later
                // flattened and collected via into_group_map() to a
                // HashMap<OsDepsPlatform, Vec<T>> effectively concatenating
                // all repeated OsDepsPlatform dependencies.
                Some(vs.map(move |v| (k, v)))
            })
            .flatten()
            .into_group_map();
        let tests = tests.iter().filter_map(rule_parse).collect();
        let test_deps = test_deps.iter().filter_map(rule_parse).collect();
        let test_named_deps = test_named_deps
            .iter()
            .filter_map(|(k, v)| rule_parse(v).map(|v| (k.clone(), v)))
            .collect();
        let test_os_deps = test_os_deps
            .iter()
            .filter_map(|(k, vs)| {
                let k = OsDepsPlatform::try_from_raw(logger, targets_path, k)?;
                let vs = vs.iter().filter_map(rule_parse);
                // See `os_deps` above for discussion about what is going on
                // here.
                Some(vs.map(move |v| (k, v)))
            })
            .flatten()
            .into_group_map();

        let extra_buck_dependencies =
            if let Some(cargo_toml_config) = &raw.autocargo.cargo_toml_config {
                UnprocessedExtraBuckDependencies::from_raw(
                    &cargo_toml_config.extra_buck_dependencies,
                    &mut rule_parse,
                )
            } else {
                UnprocessedExtraBuckDependencies::default()
            };

        Some(Self {
            raw,
            fbconfig_rule_type,
            deps,
            named_deps,
            os_deps,
            tests,
            test_deps,
            test_named_deps,
            test_os_deps,
            extra_buck_dependencies,
        })
    }

    /// Given loaded all manifest rules process the [UnprocessedBuckDependency]
    /// into [BuckDependency].
    fn build(
        self,
        logger: &'_ Logger,
        all_raw_manifests: &HashMap<FbcodeBuckRule, (Arc<TargetsPath>, Arc<RawBuckManifest>)>,
        thrift_cratemap_content: Option<String>,
    ) -> BuckManifest {
        let Self {
            raw,
            fbconfig_rule_type,
            deps,
            named_deps,
            os_deps,
            tests,
            test_deps,
            test_named_deps,
            test_os_deps,
            extra_buck_dependencies,
        } = self;

        BuckManifest {
            raw,
            fbconfig_rule_type,
            deps: deps
                .into_iter()
                .filter_map(|d| d.process(logger, all_raw_manifests))
                .collect(),
            named_deps: named_deps
                .into_iter()
                .filter_map(|(k, d)| Some((k, d.process(logger, all_raw_manifests)?)))
                .collect(),
            os_deps: os_deps
                .into_iter()
                .filter_map(|(k, v)| {
                    let v = v
                        .into_iter()
                        .filter_map(|d| d.process(logger, all_raw_manifests))
                        .collect::<Vec<_>>();
                    if v.is_empty() { None } else { Some((k, v)) }
                })
                .collect(),
            tests: tests
                .into_iter()
                .filter_map(|d| d.process(logger, all_raw_manifests))
                .collect(),
            test_deps: test_deps
                .into_iter()
                .filter_map(|d| d.process(logger, all_raw_manifests))
                .collect(),
            test_named_deps: test_named_deps
                .into_iter()
                .filter_map(|(k, d)| Some((k, d.process(logger, all_raw_manifests)?)))
                .collect(),
            test_os_deps: test_os_deps
                .into_iter()
                .filter_map(|(k, v)| {
                    let v = v
                        .into_iter()
                        .filter_map(|d| d.process(logger, all_raw_manifests))
                        .collect::<Vec<_>>();
                    if v.is_empty() { None } else { Some((k, v)) }
                })
                .collect(),
            extra_buck_dependencies: extra_buck_dependencies.process(logger, all_raw_manifests),
            thrift_config: thrift_cratemap_content.map(|cratemap_content| ThriftConfig {
                cratemap_content,
                thrift_compiler: all_raw_manifests
                    .get(&THRIFT_COMPILER_RULE)
                    .expect("Logic error: Missing thrift_compiler from all_raw_manifests")
                    .1
                    .clone(),
                codegen_includer_proc_macro: all_raw_manifests
                    .get(&CODEGEN_INCLUDER_PROC_MACRO_RULE)
                    .expect(
                        "Logic error: Missing codegen_includer_proc_macro from all_raw_manifests",
                    )
                    .1
                    .clone(),
            }),
        }
    }
}

#[derive(Debug, Default)]
struct UnprocessedExtraBuckDependencies {
    pub deps: UnprocessedBuckTargetDependencies,
    pub target: BTreeMap<TargetKey, UnprocessedBuckTargetDependencies>,
}

impl UnprocessedExtraBuckDependencies {
    fn from_raw(
        raw: &RawExtraBuckDependencies,
        process: &mut dyn for<'a> FnMut(
            &'a BuckRuleParseOutput,
        ) -> Option<UnprocessedBuckDependency>,
    ) -> Self {
        let RawExtraBuckDependencies { deps, target } = raw;

        Self {
            deps: UnprocessedBuckTargetDependencies::from_raw(deps, process),
            target: target
                .iter()
                .map(|(k, deps)| {
                    (
                        k.clone(),
                        UnprocessedBuckTargetDependencies::from_raw(deps, process),
                    )
                })
                .collect(),
        }
    }

    fn fbcode_crates(&self) -> impl Iterator<Item = &'_ FbcodeBuckRule> {
        let Self { deps, target } = self;

        deps.fbcode_crates().chain(
            target
                .values()
                .flat_map(UnprocessedBuckTargetDependencies::fbcode_crates),
        )
    }

    fn process(
        self,
        logger: &'_ Logger,
        all_raw_manifests: &HashMap<FbcodeBuckRule, (Arc<TargetsPath>, Arc<RawBuckManifest>)>,
    ) -> ExtraBuckDependencies {
        let Self { deps, target } = self;

        ExtraBuckDependencies {
            deps: deps.process(logger, all_raw_manifests),
            target: target
                .into_iter()
                .map(|(k, deps)| (k, deps.process(logger, all_raw_manifests)))
                .collect(),
        }
    }
}

#[derive(Debug, Default)]
struct UnprocessedBuckTargetDependencies {
    pub dependencies: Vec<UnprocessedBuckDependencyOverride>,
    pub dev_dependencies: Vec<UnprocessedBuckDependencyOverride>,
    pub build_dependencies: Vec<UnprocessedBuckDependencyOverride>,
}

impl UnprocessedBuckTargetDependencies {
    fn from_raw(
        raw: &RawBuckTargetDependencies,
        process: &mut dyn for<'a> FnMut(
            &'a BuckRuleParseOutput,
        ) -> Option<UnprocessedBuckDependency>,
    ) -> Self {
        let RawBuckTargetDependencies {
            dependencies,
            dev_dependencies,
            build_dependencies,
        } = raw;

        Self {
            dependencies: dependencies
                .iter()
                .filter_map(|raw_override| {
                    UnprocessedBuckDependencyOverride::from_raw(raw_override, process)
                })
                .collect(),
            dev_dependencies: dev_dependencies
                .iter()
                .filter_map(|raw_override| {
                    UnprocessedBuckDependencyOverride::from_raw(raw_override, process)
                })
                .collect(),
            build_dependencies: build_dependencies
                .iter()
                .filter_map(|raw_override| {
                    UnprocessedBuckDependencyOverride::from_raw(raw_override, process)
                })
                .collect(),
        }
    }

    fn fbcode_crates(&self) -> impl Iterator<Item = &FbcodeBuckRule> {
        let Self {
            dependencies,
            dev_dependencies,
            build_dependencies,
        } = self;

        dependencies
            .iter()
            .filter_map(UnprocessedBuckDependencyOverride::fbcode_crate)
            .chain(
                dev_dependencies
                    .iter()
                    .filter_map(UnprocessedBuckDependencyOverride::fbcode_crate),
            )
            .chain(
                build_dependencies
                    .iter()
                    .filter_map(UnprocessedBuckDependencyOverride::fbcode_crate),
            )
    }

    fn process(
        self,
        logger: &'_ Logger,
        all_raw_manifests: &HashMap<FbcodeBuckRule, (Arc<TargetsPath>, Arc<RawBuckManifest>)>,
    ) -> BuckTargetDependencies {
        let Self {
            dependencies,
            dev_dependencies,
            build_dependencies,
        } = self;

        BuckTargetDependencies {
            dependencies: dependencies
                .into_iter()
                .filter_map(|dep| dep.process(logger, all_raw_manifests))
                .collect(),
            dev_dependencies: dev_dependencies
                .into_iter()
                .filter_map(|dep| dep.process(logger, all_raw_manifests))
                .collect(),
            build_dependencies: build_dependencies
                .into_iter()
                .filter_map(|dep| dep.process(logger, all_raw_manifests))
                .collect(),
        }
    }
}

#[derive(Debug)]
enum UnprocessedBuckDependencyOverride {
    Dep(UnprocessedBuckDependency),
    NamedDep(String, UnprocessedBuckDependency),
    RemovedDep(UnprocessedBuckDependency),
}

impl UnprocessedBuckDependencyOverride {
    fn from_raw(
        raw: &RawBuckDependencyOverride,
        process: &mut dyn for<'a> FnMut(
            &'a BuckRuleParseOutput,
        ) -> Option<UnprocessedBuckDependency>,
    ) -> Option<Self> {
        match raw {
            RawBuckDependencyOverride::Dep(rule) => process(rule).map(Self::Dep),
            RawBuckDependencyOverride::NamedOrRemovedDep(Some(alias), rule) => {
                process(rule).map(|dep| Self::NamedDep(alias.clone(), dep))
            }
            RawBuckDependencyOverride::NamedOrRemovedDep(None, rule) => {
                process(rule).map(Self::RemovedDep)
            }
        }
    }

    fn fbcode_crate(&self) -> Option<&FbcodeBuckRule> {
        match self {
            Self::Dep(dep) | Self::NamedDep(_, dep) | Self::RemovedDep(dep) => dep.fbcode_crate(),
        }
    }

    fn process(
        self,
        logger: &'_ Logger,
        all_raw_manifests: &HashMap<FbcodeBuckRule, (Arc<TargetsPath>, Arc<RawBuckManifest>)>,
    ) -> Option<BuckDependencyOverride> {
        match self {
            Self::Dep(dep) => dep
                .process(logger, all_raw_manifests)
                .map(BuckDependencyOverride::Dep),
            Self::NamedDep(alias, dep) => dep
                .process(logger, all_raw_manifests)
                .map(|dep| BuckDependencyOverride::NamedDep(alias, dep)),
            Self::RemovedDep(dep) => dep
                .process(logger, all_raw_manifests)
                .map(BuckDependencyOverride::RemovedDep),
        }
    }
}

/// Intermediate result of parsing dependencies of a manifest.
#[derive(Debug, Eq, PartialEq)]
enum UnprocessedBuckDependency {
    /// Name of a crate from registry.
    ThirdPartyCrate(String),
    /// Rule in fbcode which might be a rust rule, but isn't necessary. It has to
    /// be processed further by querying buck.
    FbcodeCrate(FbcodeBuckRule),
}

impl UnprocessedBuckDependency {
    /// Return fbcode rule if this is the right enum variant.
    fn fbcode_crate(&self) -> Option<&FbcodeBuckRule> {
        use UnprocessedBuckDependency::*;
        match self {
            FbcodeCrate(rule) => Some(rule),
            ThirdPartyCrate(_) => None,
        }
    }

    /// Given a BuckRuleParseOutput dependency turn it into Self if possible.
    /// `fbsource//third-party/rust:<crate>` is turned into ThirdPartyCrate.
    /// `[fbcode]//foo:bar` is turned into FbcodeCrate.
    /// Other rules are ignored as they are not supported by this library.
    fn try_from_rule(
        logger: &'_ Logger,
        targets_path: &'_ TargetsPath,
        rule: &'_ BuckRuleParseOutput,
    ) -> Option<Self> {
        use UnprocessedBuckDependency::*;
        match rule {
            BuckRuleParseOutput::FullyQualified(rule)
                if rule.repo() == "fbsource"
                    && rule.path().as_path() == Path::new("third-party/rust") =>
            {
                Some(ThirdPartyCrate(rule.name().clone()))
            }
            BuckRuleParseOutput::FullyQualified(rule) => {
                trace!(
                    logger,
                    "Build file at {}: This type of dependency is not supported: {:#?}",
                    targets_path.as_dir().as_ref().display(),
                    rule
                );
                None
            }
            BuckRuleParseOutput::FullyQualifiedInFbcode(rule) => Some(FbcodeCrate(rule.clone())),
            BuckRuleParseOutput::RuleName(rule) => match &rule.subtarget {
                Some(_subtarget) => None,
                None => Some(FbcodeCrate(FbcodeBuckRule {
                    path: targets_path.clone(),
                    name: rule.name.clone(),
                })),
            },
        }
    }

    /// Given all manifests process Self into BuckManifest by resolving the bare
    /// FbcodeCrate rules into pointers to manifest. If a rule is not found then
    /// the dependency is non-rust and ignored.
    fn process(
        self,
        logger: &'_ Logger,
        all_raw_manifests: &HashMap<FbcodeBuckRule, (Arc<TargetsPath>, Arc<RawBuckManifest>)>,
    ) -> Option<BuckDependency> {
        match self {
            UnprocessedBuckDependency::ThirdPartyCrate(name) => {
                Some(BuckDependency::ThirdPartyCrate(name))
            }
            UnprocessedBuckDependency::FbcodeCrate(rule) => match all_raw_manifests.get(&rule) {
                Some((path, raw_manifest)) => Some(BuckDependency::FbcodeCrate(
                    path.clone(),
                    raw_manifest.clone(),
                )),
                None => {
                    trace!(
                        logger,
                        "Rule {:?} is a non-rust rule since it doesn't have a manifest", rule
                    );
                    None
                }
            },
        }
    }
}

#[cfg(test)]
mod test {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::process::Output;

    use assert_matches::assert_matches;
    use maplit::btreemap;
    use maplit::hashmap;
    use maplit::hashset;
    use mockall::Sequence;
    use serde_json::from_str;
    use serde_json::json;
    use serde_json::to_vec;
    use slog::o;

    use super::*;
    use crate::buck_processing::rules::BuckRule;
    use crate::buck_processing::rules::RuleName;
    use crate::buck_processing::test_utils::TmpManifests;
    use crate::paths::PathInFbcode;

    fn tk(s: &str) -> TargetKey {
        TargetKey::try_from(s).unwrap()
    }

    struct BuckManifestBuilderTestInput {
        targets_path: TargetsPath,
        manifest1: Arc<RawBuckManifest>,
        manifest2: Arc<RawBuckManifest>,
        codegen_includer_proc_macro_manifest: Arc<RawBuckManifest>,
        thrift_compiler_manifest: Arc<RawBuckManifest>,
        rule1: FbcodeBuckRule,
        rule2: FbcodeBuckRule,
        builder: BuckManifestBuilder,
        builder_with_thrift: BuckManifestBuilder,
    }

    impl BuckManifestBuilderTestInput {
        fn new() -> Self {
            let targets_path = TargetsPath::new(PathInFbcode::new_mock("foo/bar/TARGETS")).unwrap();

            let manifest1 = Arc::new(
                from_str::<RawBuckManifest>(include_str!(
                    "../../buck_generated/autocargo_rust_manifest.json"
                ))
                .unwrap(),
            );
            let manifest2 = Arc::new(
                from_str::<RawBuckManifest>(include_str!(
                    "../../buck_generated/autocargo_lib_rust_manifest.json"
                ))
                .unwrap(),
            );
            let codegen_includer_proc_macro_manifest = Arc::new(
                from_str::<RawBuckManifest>(include_str!(
                    "../../buck_generated/codegen_includer_proc_macro_rust_manifest.json"
                ))
                .unwrap(),
            );
            let thrift_compiler_manifest = Arc::new(
                from_str::<RawBuckManifest>(include_str!(
                    "../../buck_generated/thrift_compiler_rust_manifest.json"
                ))
                .unwrap(),
            );
            let thrift_test_manifest = Arc::new(
                from_str::<RawBuckManifest>(include_str!(
                    "../../buck_generated/thrift_test_rust_manifest.json"
                ))
                .unwrap(),
            );

            let make_rule = |name: &str| FbcodeBuckRule {
                path: targets_path.clone(),
                name: name.to_owned(),
            };

            let rule1 = make_rule("autocargo");
            let rule2 = make_rule("autocargo_lib");

            let builder = BuckManifestBuilder {
                // Raw manifest is not affected by build(), but extract
                // dependencies will check if it has thrift-specific content.
                raw: manifest1.clone(),
                fbconfig_rule_type: FbconfigRuleType::RustBinary,
                deps: vec![
                    UnprocessedBuckDependency::ThirdPartyCrate("foo".to_owned()),
                    UnprocessedBuckDependency::FbcodeCrate(make_rule("cpp_foo")),
                ],
                named_deps: hashmap! {
                    "bar".to_owned() => UnprocessedBuckDependency::FbcodeCrate(rule1.clone()),
                    "cpp_bar".to_owned() => UnprocessedBuckDependency::FbcodeCrate(make_rule("cpp_bar")),
                },
                os_deps: hashmap! {
                    OsDepsPlatform::Linux => vec![
                        UnprocessedBuckDependency::ThirdPartyCrate("fiz".to_owned()),
                        UnprocessedBuckDependency::FbcodeCrate(make_rule("cpp_fiz")),
                    ],
                    OsDepsPlatform::Macos => vec![
                        UnprocessedBuckDependency::FbcodeCrate(make_rule("cpp_mac")),
                    ]
                },
                tests: vec![UnprocessedBuckDependency::ThirdPartyCrate(
                    "fiz2".to_owned(),
                )],
                test_deps: vec![
                    UnprocessedBuckDependency::FbcodeCrate(rule2.clone()),
                    UnprocessedBuckDependency::FbcodeCrate(make_rule("cpp_biz")),
                ],
                test_named_deps: hashmap! {
                    "bar2".to_owned() => UnprocessedBuckDependency::ThirdPartyCrate("foo2".to_owned()),
                },
                test_os_deps: hashmap! {
                    OsDepsPlatform::Windows => vec![
                        UnprocessedBuckDependency::ThirdPartyCrate("fiz_windows".to_owned()),
                    ],
                },
                extra_buck_dependencies: UnprocessedExtraBuckDependencies {
                    deps: UnprocessedBuckTargetDependencies {
                        dependencies: vec![
                            UnprocessedBuckDependencyOverride::Dep(
                                UnprocessedBuckDependency::FbcodeCrate(make_rule("extra_foo")),
                            ),
                            UnprocessedBuckDependencyOverride::NamedDep(
                                "bar".to_owned(),
                                UnprocessedBuckDependency::ThirdPartyCrate("extra_biz".to_owned()),
                            ),
                        ],
                        ..UnprocessedBuckTargetDependencies::default()
                    },
                    target: btreemap! {
                        tk("unix") => UnprocessedBuckTargetDependencies {
                            build_dependencies: vec![
                                UnprocessedBuckDependencyOverride::RemovedDep(
                                    UnprocessedBuckDependency::FbcodeCrate(make_rule("extra_fiz")),
                                ),
                                UnprocessedBuckDependencyOverride::RemovedDep(
                                    UnprocessedBuckDependency::ThirdPartyCrate("extra_fuz".to_owned()),
                                ),
                            ],
                            ..UnprocessedBuckTargetDependencies::default()
                        }
                    },
                },
            };

            let builder_with_thrift = BuckManifestBuilder {
                raw: thrift_test_manifest,
                fbconfig_rule_type: FbconfigRuleType::RustLibrary,
                deps: Vec::new(),
                named_deps: HashMap::new(),
                os_deps: HashMap::new(),
                tests: Vec::new(),
                test_deps: Vec::new(),
                test_named_deps: HashMap::new(),
                extra_buck_dependencies: UnprocessedExtraBuckDependencies::default(),
                test_os_deps: HashMap::new(),
            };

            Self {
                targets_path,
                manifest1,
                manifest2,
                rule1,
                rule2,
                codegen_includer_proc_macro_manifest,
                thrift_compiler_manifest,
                builder,
                builder_with_thrift,
            }
        }
    }

    #[tokio::test]
    async fn compute_all_raw_manifests_test() {
        if cfg!(windows) {
            return; // Broken on Windows
        }

        let TmpManifests {
            autocargo_file,
            autocargo_lib_file,
            codegen_includer_proc_macro_file,
            thrift_compiler_file,
            thrift_test_file,
        } = TmpManifests::new();

        let logger = Logger::root(slog::Discard, o!());
        let fbcode_root = FbcodeRoot::new_mock("/foofoo");

        let BuckManifestBuilderTestInput {
            targets_path,
            builder,
            builder_with_thrift,
            ..
        } = BuckManifestBuilderTestInput::new();

        let cmd_runner = {
            let mut cmd_runner = MockableCommandRunner::default();
            let mut seq = Sequence::new();

            cmd_runner
                .expect_run()
                .once()
                .return_once(move |_, _, _, _| {
                    Ok(Output {
                        status: ExitStatus::from_raw(0),
                        stderr: vec![],
                        stdout: to_vec(&json!([
                            "//foo/bar:autocargo-rust-manifest",
                            "//foo/bar:autocargo_lib-rust-manifest",
                            "//foo/bar:codegen_includer_proc_macro-rust-manifest",
                            "//foo/bar:lib-rust-manifest",
                            "//foo/bar:if-rust-rust-manifest",
                        ]))
                        .unwrap(),
                    })
                })
                .in_sequence(&mut seq);

            cmd_runner
                .expect_run()
                .once()
                .return_once({
                    let p1 = autocargo_file.path().to_owned();
                    let p2 = autocargo_lib_file.path().to_owned();
                    let p3 = codegen_includer_proc_macro_file.path().to_owned();
                    let p4 = thrift_compiler_file.path().to_owned();
                    let p5 = thrift_test_file.path().to_owned();
                    move |_, _, _, _| {
                        Ok(Output {
                            status: ExitStatus::from_raw(0),
                            stderr: vec![],
                            stdout: to_vec(&json!({
                                "//foo/bar:autocargo-rust-manifest": p1,
                                "//foo/bar:autocargo_lib-rust-manifest": p2,
                                "//foo/bar:codegen_includer_proc_macro-rust-manifest": p3,
                                "//foo/bar:lib-rust-manifest": p4,
                                "//foo/bar:if-rust-rust-manifest": p5,
                            }))
                            .unwrap(),
                        })
                    }
                })
                .in_sequence(&mut seq);

            cmd_runner
        };

        assert_matches!(
            compute_all_raw_manifests(
                &logger,
                &fbcode_root,
                false, // use_isolation_dir
                &hashmap! {
                    FbcodeBuckRule {
                        path: targets_path.clone(),
                        name: "foobarbiz".to_owned(),
                    } => builder,
                    FbcodeBuckRule {
                        path: targets_path.clone(),
                        name: "if-rust".to_owned(),
                    } => builder_with_thrift,
                },
                cmd_runner
            ).await,
            Ok(all_raw_manifests) => {
                assert_eq!(
                    all_raw_manifests
                        .into_iter()
                        .sorted_by(|(k1, _), (k2, _)| Ord::cmp(k1, k2))
                        .map(|(k, (path, raw))| (k, (*path).clone(), raw.name.clone()))
                        .collect::<Vec<_>>(),
                    vec![
                        (
                            FbcodeBuckRule {
                                path: targets_path.clone(),
                                name: "autocargo".to_owned(),
                            },
                            targets_path.clone(),
                            "autocargo".to_owned()
                        ),
                        (
                            FbcodeBuckRule {
                                path: targets_path.clone(),
                                name: "autocargo_lib".to_owned(),
                            },
                            targets_path.clone(),
                            "autocargo_lib".to_owned()
                        ),
                        (
                            FbcodeBuckRule {
                                path: targets_path.clone(),
                                name: "codegen_includer_proc_macro".to_owned(),
                            },
                            targets_path.clone(),
                            "codegen_includer_proc_macro".to_owned()
                        ),
                        (
                            FbcodeBuckRule {
                                path: targets_path.clone(),
                                name: "foobarbiz".to_owned(),
                            },
                            targets_path.clone(),
                            "autocargo".to_owned()
                        ),
                        (
                            FbcodeBuckRule {
                                path: targets_path.clone(),
                                name: "if-rust".to_owned(),
                            },
                            targets_path.clone(),
                            "if-rust".to_owned()
                        ),
                        (
                            FbcodeBuckRule {
                                path: targets_path.clone(),
                                name: "lib".to_owned(),
                            },
                            targets_path.clone(),
                            "lib".to_owned()
                        ),
                    ]
                );
            }
        );
    }

    #[test]
    fn extract_dependencies_test() {
        if cfg!(windows) {
            return; // Broken on Windows
        }

        let BuckManifestBuilderTestInput {
            targets_path,
            rule1,
            rule2,
            builder,
            builder_with_thrift,
            ..
        } = BuckManifestBuilderTestInput::new();

        let make_rule = |name: &str| FbcodeBuckRule {
            path: targets_path.clone(),
            name: name.to_owned(),
        };
        let cpp_rules = vec![
            make_rule("cpp_foo"),
            make_rule("cpp_bar"),
            make_rule("cpp_fiz"),
            make_rule("cpp_mac"),
            make_rule("cpp_biz"),
            make_rule("extra_foo"),
            make_rule("extra_fiz"),
        ];

        assert_eq!(
            extract_dependencies(&[builder, builder_with_thrift]),
            vec![
                &rule1,
                &rule2,
                &*THRIFT_COMPILER_RULE,
                &*CODEGEN_INCLUDER_PROC_MACRO_RULE
            ]
            .into_iter()
            .chain(cpp_rules.iter())
            .collect::<HashSet<_>>(),
        )
    }

    #[test]
    fn process_manifest_builders_test() {
        if cfg!(windows) {
            return; // Broken on Windows
        }

        let logger = Logger::root(slog::Discard, o!());
        let BuckManifestBuilderTestInput {
            targets_path,
            manifest1,
            manifest2,
            codegen_includer_proc_macro_manifest,
            thrift_compiler_manifest,
            rule1,
            rule2,
            builder,
            ..
        } = BuckManifestBuilderTestInput::new();

        let processed_targets_path =
            TargetsPath::new(PathInFbcode::new_mock("fuz/buz/TARGETS")).unwrap();
        let processed_rule = FbcodeBuckRule {
            path: processed_targets_path.clone(),
            name: "foobarbiz".to_owned(),
        };
        let processed_raw = builder.raw.clone();

        let ProcessOutput {
            processed_manifests,
            unprocessed_paths,
        } = process_manifest_builders(
            &logger,
            hashmap! {
                processed_rule.clone() => builder,
            },
            hashmap! {
                processed_rule.clone() => (Arc::new(processed_targets_path.clone()), processed_raw),
                rule1 => (Arc::new(targets_path.clone()), manifest1),
                rule2 => (Arc::new(targets_path.clone()), manifest2),
                CODEGEN_INCLUDER_PROC_MACRO_RULE.clone() => (Arc::new(targets_path.clone()), codegen_includer_proc_macro_manifest),
                THRIFT_COMPILER_RULE.clone() => (Arc::new(targets_path.clone()), thrift_compiler_manifest),
            },
            hashmap! {
                processed_rule => "foocratemap".to_owned(),
            },
        );

        assert_matches!(
            processed_manifests.into_iter().exactly_one(),
            Ok((path, manifests)) => {
                assert_eq!(path, processed_targets_path);
                assert_matches!(
                    manifests.into_iter().exactly_one(),
                    Ok(manifest) => {
                        // This is "autocargo" here and not "foobarbiz", because
                        // the raw manifest is coming from the parsed
                        // autocargo-rust-manifest. The rule name is discarded,
                        // since the verification that rule has the same name as
                        // manifest is done in BuckManifestLoader::load.
                        assert_eq!(manifest.raw().name, "autocargo");
                        // Builder verification is done in
                        // buck_manifest_builder_test_build lets not be
                        // overzaelous here and skip it.
                    }
                )
            }
        );
        assert_eq!(
            unprocessed_paths,
            hashset! {
                targets_path,
                CODEGEN_INCLUDER_PROC_MACRO_RULE.path.clone(),
                THRIFT_COMPILER_RULE.path.clone(),
            }
        );
    }

    #[test]
    fn buck_manifest_builder_test_build() {
        if cfg!(windows) {
            return; // Broken on Windows
        }

        let logger = Logger::root(slog::Discard, o!());
        let BuckManifestBuilderTestInput {
            targets_path,
            manifest1,
            manifest2,
            codegen_includer_proc_macro_manifest,
            thrift_compiler_manifest,
            rule1,
            rule2,
            builder,
            ..
        } = BuckManifestBuilderTestInput::new();

        assert_matches!(
            builder.build(
                &logger,
                &hashmap! {
                    rule1 => (Arc::new(targets_path.clone()), manifest1),
                    rule2 => (Arc::new(targets_path.clone()), manifest2),
                    // We have to provide a raw manifest here, so just use the same as above.
                    CODEGEN_INCLUDER_PROC_MACRO_RULE.clone() => (Arc::new(targets_path.clone()), codegen_includer_proc_macro_manifest),
                    THRIFT_COMPILER_RULE.clone() => (Arc::new(targets_path.clone()), thrift_compiler_manifest),
                },
                Some("foo_cratemap".to_owned()),
            ),
            BuckManifest {
                raw: _,
                fbconfig_rule_type,
                deps,
                named_deps,
                os_deps,
                tests,
                test_deps,
                test_named_deps,
                test_os_deps,
                thrift_config,
                extra_buck_dependencies,
            } => {
                assert_eq!(fbconfig_rule_type, FbconfigRuleType::RustBinary);
                assert_matches!(
                    deps.into_iter().exactly_one(),
                    Ok(BuckDependency::ThirdPartyCrate(name)) => {
                        assert_eq!(&name, "foo")
                    }
                );
                assert_matches!(
                    named_deps.into_iter().exactly_one(),
                    Ok((key, BuckDependency::FbcodeCrate(path, raw))) => {
                        assert_eq!(&key, "bar");
                        assert_eq!(&*path, &targets_path);
                        assert_eq!(&raw.name, "autocargo");
                    }
                );
                assert_matches!(
                    os_deps.into_iter().exactly_one(),
                    Ok((OsDepsPlatform::Linux, deps)) => {
                        assert_matches!(
                            deps.into_iter().exactly_one(),
                            Ok(BuckDependency::ThirdPartyCrate(name)) => {
                                assert_eq!(&name, "fiz")
                            }
                        );
                    }
                );
                assert_matches!(
                    tests.into_iter().exactly_one(),
                    Ok(BuckDependency::ThirdPartyCrate(name)) => {
                        assert_eq!(&name, "fiz2")
                    }
                );
                assert_matches!(
                    test_deps.into_iter().exactly_one(),
                    Ok(BuckDependency::FbcodeCrate(path, raw)) => {
                        assert_eq!(&*path, &targets_path);
                        assert_eq!(&raw.name, "autocargo_lib");
                    }
                );
                assert_matches!(
                    test_named_deps.into_iter().exactly_one(),
                    Ok((key, BuckDependency::ThirdPartyCrate(name))) => {
                        assert_eq!(&key, "bar2");
                        assert_eq!(&name, "foo2")
                    }
                );
                assert_matches!(
                    test_os_deps.into_iter().exactly_one(),
                    Ok((OsDepsPlatform::Windows, deps)) => {
                        assert_matches!(
                            deps.into_iter().exactly_one(),
                            Ok(BuckDependency::ThirdPartyCrate(name)) => {
                                assert_eq!(&name, "fiz_windows")
                            }
                        );
                    }
                );
                let ThriftConfig {
                    cratemap_content,
                    thrift_compiler,
                    codegen_includer_proc_macro,
                } = thrift_config.unwrap();
                assert_eq!(&cratemap_content, "foo_cratemap");
                assert_eq!(&thrift_compiler.name, "lib");
                assert_eq!(&codegen_includer_proc_macro.name, "codegen_includer_proc_macro");
                assert_matches!(
                    extra_buck_dependencies,
                    ExtraBuckDependencies {
                        deps: BuckTargetDependencies {
                            dependencies,
                            dev_dependencies,
                            build_dependencies,
                        },
                        target
                    } => {
                        assert_matches!(
                            dependencies.into_iter().exactly_one(),
                            Ok(BuckDependencyOverride::NamedDep(alias, BuckDependency::ThirdPartyCrate(name))) => {
                                assert_eq!(&alias, "bar");
                                assert_eq!(&name, "extra_biz");
                            }
                        );
                        assert!(dev_dependencies.is_empty());
                        assert!(build_dependencies.is_empty());
                        assert_matches!(
                            target.into_iter().exactly_one(),
                            Ok((target, BuckTargetDependencies {
                                dependencies,
                                dev_dependencies,
                                build_dependencies,
                            })) => {
                                assert_eq!(target.get(), "unix");
                                assert!(dependencies.is_empty());
                                assert!(dev_dependencies.is_empty());
                                assert_matches!(
                                    build_dependencies.into_iter().exactly_one(),
                                    Ok(BuckDependencyOverride::RemovedDep(BuckDependency::ThirdPartyCrate(name))) => {
                                        assert_eq!(&name, "extra_fuz");
                                    }
                                );
                            }
                        )
                    }
                )
            }
        );
    }

    #[test]
    fn buck_manifest_builder_test_from_raw_manifest() {
        let logger = Logger::root(slog::Discard, o!());
        let targets_path = TargetsPath::new(PathInFbcode::new_mock(
            "common/rust/cargo_from_buck/autocargo/TARGETS",
        ))
        .unwrap();

        {
            let manifest = from_str::<RawBuckManifest>(include_str!(
                "../../buck_generated/autocargo_rust_manifest.json"
            ))
            .unwrap();

            assert_matches!(
                BuckManifestBuilder::from_raw_manifest(&logger, &targets_path, manifest),
                Some(BuckManifestBuilder {
                    raw,
                    deps,
                    ..
                }) => {
                    assert_eq!(&raw.name, "autocargo");
                    for expected in &[
                        UnprocessedBuckDependency::ThirdPartyCrate("clap".to_owned()),
                        UnprocessedBuckDependency::FbcodeCrate(
                            FbcodeBuckRule {
                                path: targets_path.clone(),
                                name: "autocargo_lib".to_owned(),
                            }
                        ),
                    ] {
                        assert!(deps.contains(expected), "Missing {expected:?} in {deps:#?}");
                    }
                }
            );
        }

        {
            let mut manifest = from_str::<RawBuckManifest>(include_str!(
                "../../buck_generated/autocargo_rust_manifest.json"
            ))
            .unwrap();

            manifest.fbconfig_rule_type = RawFbconfigRuleType::RustBindgenLibrary;

            assert!(
                BuckManifestBuilder::from_raw_manifest(&logger, &targets_path, manifest).is_none()
            );
        }

        {
            let mut manifest = from_str::<RawBuckManifest>(include_str!(
                "../../buck_generated/autocargo_lib_rust_manifest.json"
            ))
            .unwrap();

            manifest.dependencies.os_deps = vec![
                (
                    RawOsDepsPlatform::Other,
                    vec![BuckRuleParseOutput::RuleName(RuleName {
                        name: "not_existing".to_owned(),
                        subtarget: None,
                    })],
                ),
                (
                    RawOsDepsPlatform::Linux,
                    vec![
                        BuckRuleParseOutput::FullyQualified(BuckRule::new_mock(
                            "fbsource",
                            "third-party/rust",
                            "linux_stuff",
                        )),
                        BuckRuleParseOutput::FullyQualified(BuckRule::new_mock(
                            "fbsource",
                            "third-party/rust",
                            "more_linux_stuff",
                        )),
                    ],
                ),
                (
                    RawOsDepsPlatform::Linux,
                    vec![BuckRuleParseOutput::RuleName(RuleName {
                        name: "local_linux_stuff".to_owned(),
                        subtarget: None,
                    })],
                ),
            ];

            assert_matches!(
                BuckManifestBuilder::from_raw_manifest(&logger, &targets_path, manifest),
                Some(BuckManifestBuilder {
                    raw,
                    deps,
                    test_deps,
                    os_deps,
                    ..
                }) => {
                    assert_eq!(&raw.name, "autocargo_lib");
                    {
                        let expected = UnprocessedBuckDependency::ThirdPartyCrate(
                            "mockall".to_owned()
                        );
                        assert!(deps.contains(&expected), "Missing {expected:?} in {deps:#?}");
                    }
                    {
                        let expected = UnprocessedBuckDependency::ThirdPartyCrate(
                            "assert_matches".to_owned()
                        );
                        assert!(test_deps.contains(&expected), "Missing {expected:?} in {test_deps:#?}");
                    }
                    assert_eq!(
                        os_deps,
                        hashmap! {
                            OsDepsPlatform::Linux => vec![
                                UnprocessedBuckDependency::ThirdPartyCrate(
                                    "linux_stuff".to_owned()
                                ),
                                UnprocessedBuckDependency::ThirdPartyCrate(
                                    "more_linux_stuff".to_owned()
                                ),
                                UnprocessedBuckDependency::FbcodeCrate(
                                    FbcodeBuckRule {
                                        path: targets_path.clone(),
                                        name: "local_linux_stuff".to_owned(),
                                    }
                                ),
                            ]
                        }
                    )
                }
            );
        }
    }

    #[test]
    fn unprocessed_buck_dependency_test_try_from_rule() {
        let logger = Logger::root(slog::Discard, o!());
        let targets_path = TargetsPath::new(PathInFbcode::new_mock("foo/bar/TARGETS")).unwrap();
        let test = |rule| UnprocessedBuckDependency::try_from_rule(&logger, &targets_path, &rule);

        {
            let test = |(repo, path, name)| {
                test(BuckRuleParseOutput::FullyQualified(BuckRule::new_mock(
                    repo, path, name,
                )))
            };

            assert_eq!(
                vec![
                    ("fbsource", "third-party/rust", "biz"),
                    ("fbsource", "foo/bar", "biz"),
                    ("xplat", "third-party/rust", "biz"),
                ]
                .into_iter()
                .map(test)
                .collect::<Vec<_>>(),
                vec![
                    Some(UnprocessedBuckDependency::ThirdPartyCrate("biz".to_owned())),
                    None,
                    None,
                ],
            );
        }

        {
            let fbcode_rule = FbcodeBuckRule {
                path: TargetsPath::new(PathInFbcode::new_mock("fiz/biz/TARGETS")).unwrap(),
                name: "foobar".to_owned(),
            };
            assert_eq!(
                test(BuckRuleParseOutput::FullyQualifiedInFbcode(
                    fbcode_rule.clone()
                )),
                Some(UnprocessedBuckDependency::FbcodeCrate(fbcode_rule)),
            )
        }

        assert_eq!(
            test(BuckRuleParseOutput::RuleName(RuleName {
                name: "biz".to_owned(),
                subtarget: None,
            })),
            Some(UnprocessedBuckDependency::FbcodeCrate(FbcodeBuckRule {
                path: targets_path.clone(),
                name: "biz".to_owned(),
            })),
        )
    }

    #[test]
    fn unprocessed_buck_dependency_test_process() {
        let logger = Logger::root(slog::Discard, o!());
        let targets_path = TargetsPath::new(PathInFbcode::new_mock("foo/bar/TARGETS")).unwrap();
        let all_test_manifests = {
            let manifest1 = from_str::<RawBuckManifest>(include_str!(
                "../../buck_generated/autocargo_rust_manifest.json"
            ))
            .unwrap();
            let manifest2 = from_str::<RawBuckManifest>(include_str!(
                "../../buck_generated/autocargo_lib_rust_manifest.json"
            ))
            .unwrap();

            hashmap! {
                FbcodeBuckRule {
                    path: targets_path.clone(),
                    name: "autocargo".to_owned(),
                } => (Arc::new(targets_path.clone()), Arc::new(manifest1)),
                FbcodeBuckRule {
                    path: targets_path.clone(),
                    name: "autocargo_lib".to_owned(),
                } => (Arc::new(targets_path.clone()), Arc::new(manifest2)),
            }
        };

        assert_matches!(
            UnprocessedBuckDependency::ThirdPartyCrate("biz".to_owned()).process(&logger, &all_test_manifests),
            Some(BuckDependency::ThirdPartyCrate(name)) => {
                assert_eq!(&name, "biz");
            }
        );

        assert_matches!(
            UnprocessedBuckDependency::FbcodeCrate(FbcodeBuckRule {
                path: targets_path.clone(),
                name: "autocargo".to_owned(),
            }).process(&logger, &all_test_manifests),
            Some(BuckDependency::FbcodeCrate(path, manifest)) => {
                assert_eq!(&*path, &targets_path);
                assert_eq!(&manifest.name, "autocargo");
            }
        );

        assert_matches!(
            UnprocessedBuckDependency::FbcodeCrate(FbcodeBuckRule {
                path: targets_path,
                name: "some_cpp_rule".to_owned(),
            })
            .process(&logger, &all_test_manifests),
            None
        );
    }

    #[test]
    fn os_deps_platform_test_to_cargo_target_valid_returns() {
        for platform in enum_iterator::all::<OsDepsPlatform>() {
            assert!(!platform.to_cargo_target().is_empty());
        }
    }
}
