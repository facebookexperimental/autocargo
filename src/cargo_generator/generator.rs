/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use anyhow::Context;
use anyhow::Error;
use anyhow::Result;
use anyhow::anyhow;
use cargo_toml::Dependency;
use cargo_toml::DependencyDetail;
use cargo_toml::DepsSet;
use cargo_toml::PatchSet;
use cargo_toml::Resolver;
use cargo_toml::Workspace;
use futures::FutureExt;
use futures::future::LocalBoxFuture;
use getset::Getters;
use itertools::Itertools;
use maplit::hashmap;
use slog::Logger;
use slog::o;
use tokio::fs::read;

use super::generation::GenerationInput;
use crate::buck_processing::BuckManifest;
use crate::cargo_manifest::Manifest;
use crate::config::AllProjects;
use crate::config::PatchGeneration;
use crate::config::PatchGenerationInputDep;
use crate::config::PatchGenerationInputIterItem;
use crate::config::PatchGenerationMode;
use crate::config::ProjectConf;
use crate::config::SelectedProjects;
use crate::config::WorkspaceConfig;
use crate::paths::CargoTomlPath;
use crate::paths::FbsourceRoot;
use crate::paths::PathInFbcode;
use crate::paths::TargetsPath;
use crate::project_loader::ProjectFiles;

static THIRD_PARTY_CARGO_TOML: &str = "third-party/rust/Cargo.toml";

/// Struct holding result of successful generation.
#[derive(Default)]
pub struct GenerationOutput {
    /// Generated Cargo.toml files' paths and content.
    pub cargo_manifests: HashMap<CargoTomlPath, Manifest>,
    /// Additional files generated, e.g. thrift build files
    pub additional_files: HashMap<PathInFbcode, String>,
}

/// This is the main Cargo generator of autocargo.
#[derive(Debug, Getters)]
#[getset(get = "pub")]
pub struct CargoGenerator<'r#gen> {
    /// Third party crates defined in fbsource.
    third_party_crates: DepsSet,
    /// Third party patches defined in fbsource.
    third_party_patches: PatchSet,
    /// Map from targets paths to projects that cover them.
    targets_to_projects: HashMap<&'r#gen TargetsPath, &'r#gen ProjectConf>,
}

impl<'r#gen> CargoGenerator<'r#gen> {
    /// Prepare a new generator. It will parse
    /// fbsource/third-party/rust/Cargo.toml to get list of available third party
    /// crates.
    pub fn new<'fut>(
        logger: &'fut Logger,
        fbsource_root: &'fut FbsourceRoot,
        all_configs: &'r#gen AllProjects,
        project_files: impl IntoIterator<Item = &'r#gen ProjectFiles<'r#gen>>,
        unprocessed_paths: impl IntoIterator<Item = &'r#gen TargetsPath>,
    ) -> LocalBoxFuture<'fut, Result<Self>>
    where
        'r#gen: 'fut,
    {
        let targets_to_projects = {
            let mut targets_to_projects = all_configs.resolve_projects_for_paths(unprocessed_paths);
            targets_to_projects.extend(project_files.into_iter().flat_map(|pfiles| {
                pfiles
                    .targets()
                    .iter()
                    .map(move |path| (path, *pfiles.conf()))
            }));
            targets_to_projects
        };

        async move {
            let manifest = {
                let path = Path::join(fbsource_root.as_ref(), THIRD_PARTY_CARGO_TOML);
                let try_manifest: Result<_> =
                    try { cargo_toml::Manifest::from_slice(&read(&path).await?)? };
                try_manifest.with_context(|| format!("While processing file {}", path.display()))?
            };

            let mut third_party_crates = manifest
                .dependencies
                .into_iter()
                .chain(
                    manifest
                        .target
                        .into_iter()
                        .flat_map(|(_, t)| t.dependencies),
                )
                .collect::<BTreeMap<_, _>>();

            // The third-party crate may be partitioned (via Reindeer config)
            // into "universes" which enable different feature sets.
            // Only the default universe is currently supported in autocargo.
            // It is specified via the "default" feature in the third-party
            // crate manifest, so to get the same feature set in our generated
            // manifests, we need to enable those features on each dependency.
            if let Some(features) = manifest.features.get("default") {
                for feature in features {
                    let warn = |kind: &str, syntax| {
                        slog::warn!(logger,
                            "The manifest at {THIRD_PARTY_CARGO_TOML} specifies {kind} in its \"default\" feature: {feature:?}{}",
                            if syntax { ". Only \"<crate>/<feature>\" syntax is currently supported." } else { "" },
                        );
                    };
                    if feature.starts_with("dep:") {
                        warn("an optional dep", true);
                        continue;
                    }
                    let Some((krate, feature)) = feature.split_once('/') else {
                        warn("an unexpected feature", true);
                        continue;
                    };
                    let krate = krate.strip_suffix('?').unwrap_or(krate);
                    let Some(dep) = third_party_crates.get_mut(krate) else {
                        warn("a non-dependency crate", false);
                        continue;
                    };

                    if let Dependency::Simple(version) = dep {
                        *dep = Dependency::Detailed(Box::new(DependencyDetail {
                            version: Some(std::mem::take(version)),
                            ..DependencyDetail::default()
                        }));
                    }
                    let Dependency::Detailed(det) = dep else { unreachable!() };

                    // Add the feature specified for the default universe.
                    if feature == "default" {
                        det.default_features = true;
                    } else if !det.features.iter().map(String::as_str).contains(&feature) {
                        det.features.push(feature.to_owned());
                    }

                    // Convert to Dependency::Simple if possible.
                    if det.version.is_some() && det.default_features {
                        let mut clone = det.clone();
                        clone.version = None;
                        clone.default_features = true;
                        if *clone == DependencyDetail::default() {
                            if let Some(version) = &mut det.version {
                                *dep = Dependency::Simple(std::mem::take(version));
                            }
                        }
                    }
                }
            }

            Ok(Self {
                third_party_crates,
                third_party_patches: manifest.patch,
                targets_to_projects,
            })
        }
        .boxed_local()
    }

    /// Generate Cargo files for the given TARGETS files and additional workspace
    /// manifest for selected projects.
    pub fn generate_for_projects<'input, Manifests: IntoIterator<Item = &'input BuckManifest>>(
        &self,
        logger: &Logger,
        selected_projects: &SelectedProjects<'_>,
        many_targets: impl IntoIterator<Item = (&'input TargetsPath, Manifests)>,
    ) -> Result<GenerationOutput> {
        let mut output = generate_and_combine(
            many_targets,
            |targets_path, manifests| self.generate_for_targets(logger, targets_path, manifests),
            |path, tp, other_tp| {
                anyhow!(
                    "Path '{:?}' has been generated by both TARGETS '{:?}' and '{:?}'",
                    path,
                    tp,
                    other_tp,
                )
            },
        )?;

        self.generate_workspaces(selected_projects, &mut output.cargo_manifests)?;

        Ok(output)
    }

    /// Generate Cargo files for single TARGETS file. Multiple Cargo.toml files
    /// might be computed from a single TARGETS file, but only one TARGETS file
    /// might be the source of a Cargo.toml file.
    fn generate_for_targets<'input>(
        &self,
        logger: &Logger,
        targets_path: &TargetsPath,
        manifests: impl IntoIterator<Item = &'input BuckManifest>,
    ) -> Result<GenerationOutput> {
        if self
            .targets_to_projects
            .get(targets_path)
            .map(|proj| *proj.manual_cargo_toml())
            .unwrap_or_default()
        {
            return Ok(GenerationOutput::default());
        }

        let cargo_toml_dir_to_manifests = manifests
            .into_iter()
            .map(|manifest| {
                (
                    targets_path
                        .as_dir()
                        .join_to_path_in_fbcode(&manifest.raw().autocargo.cargo_toml_dir),
                    manifest,
                )
            })
            .into_group_map();

        generate_and_combine(
            cargo_toml_dir_to_manifests,
            |cargo_toml_dir, manifests| {
                self.generate_for_cargo_toml(logger, targets_path, cargo_toml_dir, manifests)
            },
            |path, ctd, other_ctd| {
                anyhow!(
                    "Path '{:?}' has been generated by both manifests generating \
                cargo in dirs '{:?}' and '{:?}'",
                    path,
                    ctd,
                    other_ctd,
                )
            },
        )
        .with_context(|| {
            format!(
                "While generating cargo files for build file at {}",
                targets_path.as_dir().as_ref().display(),
            )
        })
    }

    /// Generate Cargo files that correspond to single Cargo.toml file.
    fn generate_for_cargo_toml<'input>(
        &self,
        logger: &Logger,
        targets_path: &TargetsPath,
        cargo_toml_dir: &PathInFbcode,
        manifests: impl IntoIterator<Item = &'input BuckManifest>,
    ) -> Result<GenerationOutput> {
        let manifests: Vec<_> = manifests
            .into_iter()
            .filter(
                // Those should already be filtered on some previous stage, but
                // just to be clear here lets do this again.
                |manifest| !manifest.raw().autocargo.ignore_rule,
            )
            .collect();

        if manifests.is_empty() {
            return Ok(GenerationOutput::default());
        }

        let conf = self.targets_to_projects.get(targets_path).ok_or_else(|| {
            anyhow!(
                "Logic error: Failed to find {:?} in list of all targets \
                covered by projects",
                targets_path,
            )
        })?;

        let generation_input = GenerationInput::new(manifests).with_context(|| {
            format!(
                "While preparing GenerationInput for targets {targets_path:?} and cargo in \
                dir {cargo_toml_dir:?}"
            )
        })?;

        let cargo_manifests = {
            let logger = &logger.new(o!(
                "targets_path" => format!("{targets_path:?}"),
                "cargo_toml_dir" => format!("{cargo_toml_dir:?}")
            ));

            let (cargo_toml_path, cargo_manifest) = generation_input.generate_manifest(
                logger,
                self,
                conf,
                targets_path,
                cargo_toml_dir,
            )?;

            let mut cargo_manifests = hashmap! { cargo_toml_path => cargo_manifest };

            if let Some((cargo_toml_path, cargo_manifest)) = generation_input
                .generate_oss_manifest(logger, self, conf, targets_path, cargo_toml_dir)?
            {
                cargo_manifests.insert(cargo_toml_path, cargo_manifest);
            }

            cargo_manifests
        };

        let additional_files =
            generation_input.generate_additional_files(targets_path, cargo_toml_dir)?;

        Ok(GenerationOutput {
            cargo_manifests,
            additional_files,
        })
    }

    /// For each selected project that has workspace_config configured create a
    /// workspace section with a third-party patch section and put it in a new or
    /// already generated Cargo.toml file inside of cargo_manifest.
    fn generate_workspaces(
        &self,
        selected_projects: &SelectedProjects<'_>,
        cargo_manifests: &mut HashMap<CargoTomlPath, Manifest>,
    ) -> Result<()> {
        let workspaces = selected_projects
            .projects()
            .iter()
            .filter_map(|conf| {
                conf.workspace_config().as_ref().map(
                    |WorkspaceConfig {
                         scrape_dir,
                         prefix_for_dir,
                         save_to_dir,
                         patch_generation,
                         patch,
                     }| {
                        let manifests = cargo_manifests
                            .iter()
                            .filter_map(|(cargo_toml_path, manifest)| {
                                cargo_toml_path
                                    .as_dir()
                                    .as_ref()
                                    .strip_prefix(scrape_dir.as_ref())
                                    .ok()
                                    .map(|member| (member, manifest))
                            })
                            .collect::<Vec<_>>();

                        check_packages_are_unique(manifests.iter().map(|(_, manifest)| *manifest))
                            .with_context(|| {
                                format!("Cannot generate Workspace including {scrape_dir:?}")
                            })?;

                        Ok((
                            CargoTomlPath::new(
                                save_to_dir
                                    .as_ref()
                                    .unwrap_or(scrape_dir)
                                    .join_to_path_in_fbcode(CargoTomlPath::filename()),
                            )
                            .expect(
                                "Failed to create a CargoTomlPath for \
                                workspace even though a proper filename was \
                                joined to path",
                            ),
                            Workspace {
                                members: manifests
                                    .into_iter()
                                    .map(|(member, _)| {
                                        let member = prefix_for_dir.as_ref().map_or_else(
                                            || member.to_string_lossy().into_owned(),
                                            |prefix| {
                                                prefix.join(member).to_string_lossy().into_owned()
                                            },
                                        );
                                        if member.is_empty() {
                                            ".".to_owned()
                                        } else {
                                            member
                                        }
                                    })
                                    .collect(),
                                default_members: Vec::new(),
                                package: None,
                                exclude: Vec::new(),
                                metadata: None,
                                resolver: Some(Resolver::V2),
                                dependencies: DepsSet::new(),
                                lints: BTreeMap::new(),
                            },
                            self.generate_patch(patch_generation, patch.iter())
                                .context("While generating patch for workspace")?,
                        ))
                    },
                )
            })
            .collect::<Result<Vec<_>>>()?;

        for (workspace_path, workspace, patch) in workspaces {
            let manifest = cargo_manifests.entry(workspace_path).or_default();
            manifest.workspace = Some(workspace);
            manifest.patch = patch;
        }

        Ok(())
    }

    /// Resolve the PatchGenerationInputOrThirdParty using third_party_patches.
    pub(super) fn generate_patch<'input>(
        &self,
        patch_generation: &PatchGeneration,
        additional_patches: impl IntoIterator<Item = PatchGenerationInputIterItem<'input>>,
    ) -> Result<PatchSet> {
        let mut patch_set = match patch_generation.mode {
            PatchGenerationMode::Empty => PatchSet::new(),
            PatchGenerationMode::ThirdPartyFull => self.third_party_patches().clone(),
        };

        let empty_third_party_patches = DepsSet::default();
        for (source, patches) in additional_patches {
            let third_party_patches = self
                .third_party_patches
                .get(source)
                .unwrap_or(&empty_third_party_patches);

            let deps_set = patch_set.entry(source.to_owned()).or_default();
            for patch in patches.iter() {
                let (name, deps) = match patch {
                    PatchGenerationInputDep::FromFbsourceThirdParty(name) => (
                        name.clone(),
                        third_party_patches
                            .get(name.as_str())
                            .ok_or_else(|| {
                                anyhow!(
                                    "Missing patch for '{}'.{} in {}",
                                    source,
                                    name,
                                    THIRD_PARTY_CARGO_TOML,
                                )
                            })?
                            .clone(),
                    ),
                    PatchGenerationInputDep::Dependency(name, dep) => (name.clone(), dep.clone()),
                };
                deps_set.insert(name, deps);
            }
        }

        for (source, exclusions) in patch_generation.exclude.iter() {
            if let Some(deps_set) = patch_set.get_mut(source) {
                for name in exclusions {
                    deps_set.remove(name);
                }
            }
        }

        Ok(patch_set)
    }
}

/// Given input and generation function produce GenerationOutput, check the
/// generated paths for uniqueness, reporting with bail function if not unique,
/// and finally combine all GenerationOutput into a single struct.
fn generate_and_combine<TKey: Clone, TValue>(
    input: impl IntoIterator<Item = (TKey, TValue)>,
    mut gen_fun: impl FnMut(&TKey, TValue) -> Result<GenerationOutput>,
    bail_fun: impl FnOnce(&Path, &TKey, &TKey) -> Error,
) -> Result<GenerationOutput> {
    let mut all_cargo_manifests = HashMap::new();
    let mut all_additional_files = HashMap::new();
    for (key, value) in input {
        let GenerationOutput {
            cargo_manifests,
            additional_files,
        } = gen_fun(&key, value)?;

        for path in cargo_manifests.keys() {
            if let Some((_, other_key)) = all_cargo_manifests.get(path) {
                return Err(bail_fun(path.as_file().as_ref(), &key, other_key));
            }
        }

        for path in additional_files.keys() {
            if let Some((_, other_key)) = all_additional_files.get(path) {
                return Err(bail_fun(path.as_ref(), &key, other_key));
            }
        }

        all_cargo_manifests.extend(
            cargo_manifests
                .into_iter()
                .map(|(path, manifest)| (path, (manifest, key.clone()))),
        );
        all_additional_files.extend(
            additional_files
                .into_iter()
                .map(|(path, content)| (path, (content, key.clone()))),
        );
    }

    Ok(GenerationOutput {
        cargo_manifests: all_cargo_manifests
            .into_iter()
            .map(|(path, (manifest, _))| (path, manifest))
            .collect(),
        additional_files: all_additional_files
            .into_iter()
            .map(|(path, (content, _))| (path, content))
            .collect(),
    })
}

fn check_packages_are_unique<'a>(
    manifests: impl IntoIterator<Item = &'a Manifest>,
) -> Result<(), Error> {
    let mut all_packages = HashSet::new();

    for manifest in manifests {
        if let Some(package) = manifest.package.as_ref() {
            if all_packages.insert(&package.name) {
                continue;
            }

            return Err(anyhow!("Duplicate package name: {}", package.name));
        }
    }

    Ok(())
}
