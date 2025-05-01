// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use std::collections::BTreeMap;
use std::collections::HashSet;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::ensure;
use cargo_toml::Dependency;
use cargo_toml::DependencyDetail;
use cargo_toml::DepsSet;
use cargo_toml::FeatureSet;
use cargo_toml::Target;
use pathdiff::diff_paths;

use super::compute_cargo_toml_path;
use super::consolidated_dependencies::ConsolidatedDependencies;
use super::consolidated_dependencies::Deps;
use super::consolidated_dependencies::NamedDeps;
use super::package::generate_dependency_package_name;
use super::package::generate_dependency_package_version;
use crate::buck_processing::BuckDependency;
use crate::buck_processing::BuckDependencyOverride;
use crate::buck_processing::BuckTargetDependencies;
use crate::buck_processing::CargoDependencyOverride;
use crate::buck_processing::DependenciesOverride;
use crate::buck_processing::ExtraBuckDependencies;
use crate::buck_processing::OsDepsPlatform;
use crate::buck_processing::RawBuckManifest;
use crate::buck_processing::TargetDependenciesOverride;
use crate::cargo_generator::CargoGenerator;
use crate::cargo_manifest::KeyedTargetDepsSet;
use crate::config::OssGitConfig;
use crate::config::ProjectConf;
use crate::paths::CargoTomlPath;
use crate::paths::TargetsPath;

pub struct Dependencies {
    pub dependencies: DepsSet,
    pub dev_dependencies: DepsSet,
    pub build_dependencies: DepsSet,
    pub target: KeyedTargetDepsSet,
}

/// Struct to hold inputs for dependency generation.
pub struct DependenciesGenerator<'a> {
    pub cargo_generator: &'a CargoGenerator<'a>,
    pub features: &'a FeatureSet,
    pub cargo_toml_path: &'a CargoTomlPath,
    pub consolidated_dependencies: ConsolidatedDependencies<'a>,
    pub extra_buck_dependencies: &'a ExtraBuckDependencies,
    pub dependencies_override: &'a DependenciesOverride,
    pub oss_git_config: Option<&'a OssGitConfig>,
}

impl DependenciesGenerator<'_> {
    /// This method generates dependencies as follows:
    /// - first generate [dependencies] making some of them optional based on
    ///   provided list of features of the crate
    /// - then generate [dev-dependencies] making sure not to include the ones
    ///   already mentioned in [dependencies] unless they were optional. Test deps
    ///   cannot be optional so they wouldn't be exact duplicates.
    /// - while generating the above remove any entries that are marked as to be
    ///   removed in extra_buck_dependencies
    /// - now add any entries that are supposed to be added from
    ///   extra_buck_dependencies
    /// - (Note) the previous step might have created a [build-dependency]
    ///   section if extra_buck_dependencies includes one
    /// - lastly apply any transformations that the dependencies_override defines
    /// - now do the above for each target dependency set
    pub fn generate(self) -> Result<Dependencies> {
        let ConsolidatedDependencies {
            deps,
            named_deps,
            os_deps,
            test_deps,
            test_named_deps,
            test_os_deps,
            build_deps,
        } = &self.consolidated_dependencies;

        let ExtraBuckDependencies {
            deps:
                BuckTargetDependencies {
                    dependencies: extra_dependencies,
                    dev_dependencies: extra_dev_dependencies,
                    build_dependencies: extra_build_dependencies,
                },
            target: extra_target,
        } = &self.extra_buck_dependencies;

        let DependenciesOverride {
            deps:
                TargetDependenciesOverride {
                    dependencies: dependencies_override,
                    dev_dependencies: dev_dependencies_override,
                    build_dependencies: build_dependencies_override,
                },
            target: target_override,
        } = &self.dependencies_override;

        let optional_deps: HashSet<_> = self
            .features
            .values()
            .flatten()
            .map(|s| s.as_str())
            .collect();

        let dependencies = self
            .gen_regular_dependencies(
                &optional_deps,
                deps,
                named_deps,
                extra_dependencies,
                dependencies_override,
            )
            .context("In dependencies")?;

        let dev_dependencies = self
            .gen_dev_dependencies(
                &dependencies,
                test_deps,
                test_named_deps,
                extra_dev_dependencies,
                dev_dependencies_override,
            )
            .context("In dev_dependencies")?;

        let build_dependencies = self
            .gen_build_dependencies(
                build_deps,
                extra_build_dependencies,
                build_dependencies_override,
            )
            .context("In build_dependencies")?;

        let target = enum_iterator::all::<OsDepsPlatform>()
            .map(|os| {
                (
                    os_deps.get(&os),
                    test_os_deps.get(&os),
                    os.to_cargo_target(),
                )
            })
            .chain({
                let os_deps_platform_names: HashSet<_> = enum_iterator::all::<OsDepsPlatform>()
                    .map(|os| os.to_cargo_target())
                    .collect();
                extra_target
                    .keys()
                    .chain(target_override.keys())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .filter_map(move |name| {
                        if os_deps_platform_names.contains(name) {
                            None
                        } else {
                            Some((None, None, name))
                        }
                    })
            })
            .map(|(target_os_deps, target_test_os_deps, target_name)| {
                let result: Result<_> = try {
                    (target_name.to_owned(), {
                        // Default &T is a pain to get, so just create defaults to
                        // refer to them.
                        let default_deps = Deps::default();
                        let extra_default = Vec::new();
                        let default_overrides = BTreeMap::new();

                        let dependencies = self
                            .gen_regular_dependencies(
                                &optional_deps,
                                target_os_deps.unwrap_or(&default_deps),
                                &NamedDeps::default(),
                                extra_target
                                    .get(target_name)
                                    .map_or(&extra_default, |dep| &dep.dependencies),
                                target_override
                                    .get(target_name)
                                    .map_or(&default_overrides, |dep| &dep.dependencies),
                            )
                            .context("In dependencies")?;

                        let dev_dependencies = self
                            .gen_dev_dependencies(
                                &dependencies,
                                target_test_os_deps.unwrap_or(&default_deps),
                                &NamedDeps::default(),
                                extra_target
                                    .get(target_name)
                                    .map_or(&extra_default, |dep| &dep.dev_dependencies),
                                target_override
                                    .get(target_name)
                                    .map_or(&default_overrides, |dep| &dep.dev_dependencies),
                            )
                            .context("In dev_dependencies")?;

                        let build_dependencies = self
                            .gen_build_dependencies(
                                &default_deps,
                                extra_target
                                    .get(target_name)
                                    .map_or(&extra_default, |dep| &dep.build_dependencies),
                                target_override
                                    .get(target_name)
                                    .map_or(&default_overrides, |dep| &dep.build_dependencies),
                            )
                            .context("In build_dependencies")?;

                        Target {
                            dependencies,
                            dev_dependencies,
                            build_dependencies,
                        }
                    })
                };
                result.with_context(|| format!("In target for {target_name:?}"))
            })
            .collect::<Result<_>>()?;

        Ok(Dependencies {
            dependencies,
            dev_dependencies,
            build_dependencies,
            target,
        })
    }

    /// Regular dependencies might be optional, so passing optional_deps here.
    fn gen_regular_dependencies(
        &self,
        optional_deps: &HashSet<&str>,
        deps: &Deps<'_>,
        named_deps: &NamedDeps<'_>,
        extra_buck_dependencies: &[BuckDependencyOverride],
        dependencies_override: &BTreeMap<String, CargoDependencyOverride>,
    ) -> Result<DepsSet> {
        ComputeDependencies {
            cargo_generator: self.cargo_generator,
            optional_deps,
            cargo_toml_path: self.cargo_toml_path,
            deps,
            named_deps,
            extra_buck_dependencies,
            dependencies_override,
            oss_git_config: self.oss_git_config,
        }
        .compute()
    }

    /// Dev dependencies cannot be optional, so no optional_deps here,
    /// but if a dependency is already present in regular dependencies then there
    /// is no need to repeate it for the dev section, so we are passing
    /// regular_dependencies and using deps_difference on it and the generation
    /// result.
    fn gen_dev_dependencies(
        &self,
        regular_dependencies: &DepsSet,
        deps: &Deps<'_>,
        named_deps: &NamedDeps<'_>,
        extra_buck_dependencies: &[BuckDependencyOverride],
        dependencies_override: &BTreeMap<String, CargoDependencyOverride>,
    ) -> Result<DepsSet> {
        Ok(deps_difference(
            regular_dependencies,
            ComputeDependencies {
                cargo_generator: self.cargo_generator,
                optional_deps: &HashSet::new(),
                cargo_toml_path: self.cargo_toml_path,
                deps,
                named_deps,
                extra_buck_dependencies,
                dependencies_override,
                oss_git_config: self.oss_git_config,
            }
            .compute()?,
        ))
    }

    /// Build dependencies are completely indendent from regular and
    /// dev dependencies, so they must be computed separately.
    fn gen_build_dependencies(
        &self,
        deps: &Deps<'_>,
        extra_buck_dependencies: &[BuckDependencyOverride],
        dependencies_override: &BTreeMap<String, CargoDependencyOverride>,
    ) -> Result<DepsSet> {
        ComputeDependencies {
            cargo_generator: self.cargo_generator,
            optional_deps: &HashSet::new(),
            cargo_toml_path: self.cargo_toml_path,
            deps,
            named_deps: &NamedDeps::default(),
            extra_buck_dependencies,
            dependencies_override,
            oss_git_config: self.oss_git_config,
        }
        .compute()
    }
}

/// Struct to hold input for computing dependencies.
struct ComputeDependencies<'a> {
    cargo_generator: &'a CargoGenerator<'a>,
    optional_deps: &'a HashSet<&'a str>,
    cargo_toml_path: &'a CargoTomlPath,
    deps: &'a Deps<'a>,
    named_deps: &'a NamedDeps<'a>,
    extra_buck_dependencies: &'a [BuckDependencyOverride],
    dependencies_override: &'a BTreeMap<String, CargoDependencyOverride>,
    oss_git_config: Option<&'a OssGitConfig>,
}

impl ComputeDependencies<'_> {
    /// Take all the regular and named deps to produce a dependency set.
    fn compute(self) -> Result<DepsSet> {
        let ComputeDependencies {
            cargo_generator,
            optional_deps,
            cargo_toml_path,
            deps,
            named_deps,
            extra_buck_dependencies,
            dependencies_override,
            oss_git_config,
        } = self;

        let mut deps_set = DepsSet::new();
        let mut add_to_deps = |key: String, value: Dependency| {
            if let Some(old_value) = deps_set.get(&key) {
                ensure!(
                    value.eq(old_value),
                    "Found duplicate key {} with one value {:?} and other {:?}",
                    key,
                    value,
                    old_value
                )
            }
            deps_set.insert(key, value);
            Ok(())
        };

        let removed_third_party: HashSet<_> = extra_buck_dependencies
            .iter()
            .filter_map(|dep_override| match dep_override {
                BuckDependencyOverride::RemovedDep(BuckDependency::ThirdPartyCrate(name)) => {
                    Some(name.as_str())
                }
                _ => None,
            })
            .collect();
        let removed_fbcode: HashSet<_> = extra_buck_dependencies
            .iter()
            .filter_map(|dep_override| match dep_override {
                BuckDependencyOverride::RemovedDep(BuckDependency::FbcodeCrate(path, raw)) => {
                    Some((&**path, &raw.name))
                }
                _ => None,
            })
            .collect();

        for tp_name in &deps.third_party {
            if !removed_third_party.contains(tp_name) {
                let (name, dep) = get_third_party_dependency(
                    cargo_generator,
                    optional_deps,
                    Alias(None),
                    tp_name,
                )?;
                add_to_deps(name, dep)?;
            }
        }
        for (rule, raw) in &deps.fbcode {
            if !removed_fbcode.contains(&(&**rule.targets_path(), &raw.name)) {
                if let Some((name, dep)) = get_fbcode_dependency(
                    cargo_generator,
                    optional_deps,
                    Alias(None),
                    cargo_toml_path,
                    oss_git_config,
                    rule.targets_path(),
                    raw,
                )? {
                    add_to_deps(name, dep)?;
                }
            }
        }

        for (alias, tp_name) in &named_deps.third_party {
            if !removed_third_party.contains(tp_name) {
                add_to_deps(
                    (*alias).to_owned(),
                    get_third_party_dependency(
                        cargo_generator,
                        optional_deps,
                        Alias(Some(alias)),
                        tp_name,
                    )?
                    .1,
                )?;
            }
        }
        for ((alias, rule), raw) in &named_deps.fbcode {
            if !removed_fbcode.contains(&(&**rule.targets_path(), &raw.name)) {
                if let Some((_, dep)) = get_fbcode_dependency(
                    cargo_generator,
                    optional_deps,
                    Alias(Some(alias)),
                    cargo_toml_path,
                    oss_git_config,
                    rule.targets_path(),
                    raw,
                )? {
                    add_to_deps((*alias).to_owned(), dep)?;
                }
            }
        }

        for dep_override in extra_buck_dependencies {
            match dep_override {
                BuckDependencyOverride::Dep(BuckDependency::ThirdPartyCrate(tp_name)) => {
                    let (name, dep) = get_third_party_dependency(
                        cargo_generator,
                        optional_deps,
                        Alias(None),
                        tp_name,
                    )?;
                    add_to_deps(name, dep)?;
                }
                BuckDependencyOverride::Dep(BuckDependency::FbcodeCrate(path, raw)) => {
                    if let Some((name, dep)) = get_fbcode_dependency(
                        cargo_generator,
                        optional_deps,
                        Alias(None),
                        cargo_toml_path,
                        oss_git_config,
                        path,
                        raw,
                    )? {
                        add_to_deps(name, dep)?;
                    }
                }
                BuckDependencyOverride::NamedDep(
                    alias,
                    BuckDependency::ThirdPartyCrate(tp_name),
                ) => {
                    add_to_deps(
                        (*alias).to_owned(),
                        get_third_party_dependency(
                            cargo_generator,
                            optional_deps,
                            Alias(Some(alias)),
                            tp_name,
                        )?
                        .1,
                    )?;
                }
                BuckDependencyOverride::NamedDep(alias, BuckDependency::FbcodeCrate(path, raw)) => {
                    if let Some((_, dep)) = get_fbcode_dependency(
                        cargo_generator,
                        optional_deps,
                        Alias(Some(alias)),
                        cargo_toml_path,
                        oss_git_config,
                        path,
                        raw,
                    )? {
                        add_to_deps((*alias).to_owned(), dep)?;
                    }
                }
                BuckDependencyOverride::RemovedDep(_) => {}
            }
        }

        let default_override = CargoDependencyOverride::default();
        Ok(dependencies_override
            .iter()
            .filter_map(|(key, dep_override)| {
                if deps_set.contains_key(key) {
                    None
                } else {
                    Some((
                        key.to_owned(),
                        Dependency::Detailed(Box::default()),
                        dep_override,
                    ))
                }
            })
            .collect::<Vec<_>>()
            .into_iter()
            .chain(deps_set.into_iter().map(|(key, dep)| {
                let dep_override = dependencies_override.get(&key).unwrap_or(&default_override);
                (key, dep, dep_override)
            }))
            .map(|(key, dep, dep_override)| {
                (
                    key.clone(),
                    apply_override(cargo_generator, optional_deps, &key, dep, dep_override),
                )
            })
            .collect())
    }
}

struct Alias<'a>(Option<&'a str>);

/// Take a detailed dependency 'foo', set appropriate fields on it and check if
/// it can be simplified to just the 'foo = "version"' type.
fn detail_to_dep(
    package_name: &str,
    mut detail: DependencyDetail,
    optional_deps: &HashSet<&str>,
    Alias(alias): Alias<'_>,
) -> Dependency {
    detail.optional = match alias {
        Some(alias) => optional_deps.contains(alias),
        None => optional_deps.contains(package_name),
    };

    detail.package = alias.and_then(|alias| {
        if alias == package_name {
            None
        } else {
            Some(package_name.to_owned())
        }
    });

    dependency_detail_to_dependency(detail)
}

/// Resolve a third party dependency from fbsource/third-party/rust/Cargo.toml.
/// Note that the aforementioned Cargo.toml sometimes uses the following schema:
///
/// [dependencies]
/// foo = "2"
/// foo-1 = { package = "foo", version = "1" }
///
/// In this case if the tp_name = "foo-1" then the resulting package name would
/// be "foo" and this should be used by Cargo as an alias for dependency, unless
/// it is overwritten via named_deps.
fn get_third_party_dependency(
    cargo_generator: &CargoGenerator<'_>,
    optional_deps: &HashSet<&str>,
    alias: Alias<'_>,
    tp_name: &str,
) -> Result<(String, Dependency)> {
    cargo_generator
        .third_party_crates()
        .get(tp_name)
        .cloned()
        .map(|dep| {
            let package_name = match &dep {
                Dependency::Inherited(_) => unimplemented!(
                    "third-party dependency `{tp_name}` uses inherited dependency syntax which is not supported"
                ),
                _ => {
                    dep.package().map_or(tp_name.to_owned(), |p| p.to_owned())
                }
            };

            let dep = {
                let detail = dependency_to_dependency_detail(tp_name, dep);
                detail_to_dep(&package_name, detail, optional_deps, alias)
            };

            (package_name, dep)
        })
        .ok_or_else(|| {
            anyhow!(
                "Missing third-party dependency {}. List of known third-party crates: {:?}",
                tp_name,
                cargo_generator
                    .third_party_crates()
                    .keys()
                    .collect::<Vec<_>>(),
            )
        })
}

#[derive(Clone, Copy)]
struct OssDepConfigs<'a> {
    from_oss_git_config: &'a OssGitConfig,
    to_project_config: &'a ProjectConf,
    to_oss_git_config: &'a OssGitConfig,
}

fn get_fbcode_dependency(
    cargo_generator: &CargoGenerator<'_>,
    optional_deps: &HashSet<&str>,
    alias: Alias<'_>,
    from_cargo_toml_path: &CargoTomlPath,
    maybe_from_oss_git_config: Option<&OssGitConfig>,
    to_targets_path: &TargetsPath,
    to_raw: &RawBuckManifest,
) -> Result<Option<(String, Dependency)>> {
    let maybe_to_project_conf = cargo_generator.targets_to_projects().get(to_targets_path);

    let oss_dep_configs = {
        let maybe_to_configs = maybe_to_project_conf
            .and_then(|proj| proj.oss_git_config().as_ref().map(|git| (proj, git)));
        match (maybe_from_oss_git_config, maybe_to_configs) {
            (Some(from_oss_git_config), Some((to_project_config, to_oss_git_config))) => {
                Some(OssDepConfigs {
                    from_oss_git_config,
                    to_project_config,
                    to_oss_git_config,
                })
            }
            (None, _) => None,
            (Some(_), None) => {
                // Since maybe_from_oss_git_config is some then we are making a
                // oss-compliant Cargo manifest. If our dependency doesn't have
                // OSS config then we have to ignore it.
                return Ok(None);
            }
        }
    };

    let package_name = generate_dependency_package_name(to_targets_path, to_raw);

    let features = match maybe_to_project_conf {
        // For autocargo maintained Cargo.toml files the features defined on
        // buck rules should be included as default features. With manually
        // maintained Cargo.toml files it might not be the case, so add the
        // features to the dependency.
        Some(project) if *project.manual_cargo_toml() => {
            if let Some(features) = to_raw
                .autocargo
                .cargo_toml_config
                .as_ref()
                .and_then(|conf| conf.features.as_ref())
            {
                features.get("default").cloned().unwrap_or_default()
            } else {
                to_raw.rust_config.features.clone()
            }
        }
        _ => Vec::new(),
    };

    let dep = {
        // The version must be present if we are generating manifest for oss,
        // this helps later with publishing crates to registry.
        let version = oss_dep_configs.map(
            |OssDepConfigs {
                 to_project_config, ..
             }| {
                generate_dependency_package_version(
                    to_raw.autocargo.cargo_toml_config.as_ref(),
                    &to_project_config.defaults().package,
                )
            },
        );
        let detail = match oss_dep_configs {
            Some(OssDepConfigs {
                from_oss_git_config,
                to_oss_git_config,
                ..
            }) if from_oss_git_config.git != to_oss_git_config.git => {
                // Dependency between two different git repositories
                let OssGitConfig {
                    public_cargo_dir: _,
                    git,
                    branch,
                    tag,
                    rev,
                    default_features_to_strip: _,
                } = to_oss_git_config;
                DependencyDetail {
                    version,
                    git: Some(git.clone()),
                    branch: branch.clone(),
                    tag: tag.clone(),
                    rev: rev.clone(),
                    features,
                    ..DependencyDetail::default()
                }
            }
            _ => {
                // Either dependency inside the same git repository or not a
                // oss-generation and all dependencies are path dependencies
                let to_cargo_toml_path = compute_cargo_toml_path(
                    &to_targets_path
                        .as_dir()
                        .join_to_path_in_fbcode(&to_raw.autocargo.cargo_toml_dir),
                );

                DependencyDetail {
                    version,
                    path: Some(
                        diff_paths(
                            to_cargo_toml_path.as_dir().as_ref(),
                            from_cargo_toml_path.as_dir().as_ref(),
                        )
                        .and_then(|p| p.to_str().map(|s| s.to_owned()))
                        .ok_or_else(|| {
                            anyhow!(
                                "Failed to make a relative path from {:?} to {:?} while \
                                    creating a fbcode dependency",
                                to_cargo_toml_path,
                                from_cargo_toml_path
                            )
                        })?,
                    ),
                    features,
                    ..DependencyDetail::default()
                }
            }
        };

        detail_to_dep(&package_name, detail, optional_deps, alias)
    };

    Ok(Some((package_name, dep)))
}

fn deps_difference(base_dependencies: &DepsSet, other_dependencies: DepsSet) -> DepsSet {
    other_dependencies
        .into_iter()
        .filter(|(k, v)| base_dependencies.get(k) != Some(v))
        .collect()
}

fn apply_override(
    cargo_generator: &CargoGenerator<'_>,
    optional_deps: &HashSet<&str>,
    key: &str,
    dep: Dependency,
    dep_override: &CargoDependencyOverride,
) -> Dependency {
    let CargoDependencyOverride {
        version: version_override,
        registry: registry_override,
        registry_index: registry_index_override,
        path: path_override,
        git: git_override,
        branch: branch_override,
        tag: tag_override,
        rev: rev_override,
        features: features_override,
        optional: optional_override,
        default_features: default_features_override,
        package: package_override,
    } = dep_override;

    let DependencyDetail {
        version,
        registry,
        registry_index,
        path,
        inherited,
        git,
        branch,
        tag,
        rev,
        features,
        optional,
        default_features,
        package,
        unstable: _,
    } = dependency_to_dependency_detail(key, dep);
    let fixed_up_version = if key == "cxx-build" {
        match get_third_party_dependency(cargo_generator, optional_deps, Alias(None), "cxx") {
            Ok((_, cxx_dep)) => dependency_to_dependency_detail("cxx", cxx_dep).version,
            Err(_) => version_override.clone().unwrap_or(version),
        }
    } else {
        version_override.clone().unwrap_or(version)
    };
    dependency_detail_to_dependency(DependencyDetail {
        version: fixed_up_version,
        registry: registry_override.clone().unwrap_or(registry),
        registry_index: registry_index_override.clone().unwrap_or(registry_index),
        path: path_override.clone().unwrap_or(path),
        inherited,
        git: git_override.clone().unwrap_or(git),
        branch: branch_override.clone().unwrap_or(branch),
        tag: tag_override.clone().unwrap_or(tag),
        rev: rev_override.clone().unwrap_or(rev),
        features: features_override.clone().unwrap_or(features),
        optional: (*optional_override).unwrap_or(optional),
        default_features: (*default_features_override).unwrap_or(default_features),
        package: package_override.clone().unwrap_or(package),
        unstable: BTreeMap::new(),
    })
}

fn dependency_to_dependency_detail(name: &str, dep: Dependency) -> DependencyDetail {
    match dep {
        Dependency::Simple(version) => DependencyDetail {
            version: Some(version),
            ..DependencyDetail::default()
        },
        Dependency::Detailed(detail) => *detail,
        Dependency::Inherited(_) => unimplemented!(
            "dependency `{name}` uses inherited dependency syntax which is not supported"
        ),
    }
}

fn dependency_detail_to_dependency(detail: DependencyDetail) -> Dependency {
    match detail {
        DependencyDetail {
            version: Some(version),
            registry: None,
            registry_index: None,
            path: None,
            inherited: false,
            git: None,
            branch: None,
            tag: None,
            rev: None,
            features,
            optional: false,
            default_features: true,
            package: None,
            unstable,
        } if features.is_empty() && unstable.is_empty() => Dependency::Simple(version),
        detail => Dependency::Detailed(Box::new(detail)),
    }
}
