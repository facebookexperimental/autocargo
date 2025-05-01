/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

//! Project configuration structures which can be deserialized from json files,
//! materialized Configerator files and directly from Configerator

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Error;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use cargo_toml::Dependency;
use cargo_toml::Edition;
use cargo_toml::Profiles;
use cargo_toml::Publish;
use cargo_toml::Value;
use futures::StreamExt;
use futures::TryFutureExt;
use futures::TryStreamExt;
use futures::stream;
use futures::stream::BoxStream;
use getset::Getters;
use glob::Pattern;
use glob::PatternError;
use serde::Deserialize;
use tokio::fs::read_dir;
use tokio::fs::read_to_string;
use tokio_stream::wrappers::ReadDirStream;
use toml::from_str;

use crate::paths::PathInFbcode;
use crate::paths::TargetsPath;
use crate::util::deserialize::deserialize_globs;

/// A newtype for better tracking list of all projects.
#[derive(Debug, Getters)]
#[getset(get = "pub")]
pub struct AllProjects {
    /// Map from name of the project to its config.
    projects: HashMap<String, ProjectConf>,
}

impl AllProjects {
    /// Return SelectedProjects containing all projects.
    pub fn select_all(&self) -> SelectedProjects {
        SelectedProjects::new(self.projects().values().collect())
    }

    /// Return SelectedProjects that cover the provided paths or that depend
    /// on projects that cover them.
    pub fn select_based_on_paths_and_names(
        &self,
        paths: &[PathInFbcode],
        names: &[String],
    ) -> Result<SelectedProjects> {
        let mut selected_by_path: HashSet<_> = self
            .projects()
            .iter()
            .filter_map(|(name, c)| {
                if paths.iter().any(|p| c.covers_path(p)) {
                    Some(name)
                } else {
                    None
                }
            })
            .collect();

        // Making BFS on reverse graph of deps to gather all dependent projects
        let mut to_process: HashSet<_> = selected_by_path.clone();
        while !to_process.is_empty() {
            to_process = self
                .projects()
                .iter()
                .filter_map(|(name, c)| {
                    if to_process.iter().any(|p| c.dependencies().contains(*p)) {
                        Some(name)
                    } else {
                        None
                    }
                })
                .collect();
            // .copied() changes && -> &
            to_process = to_process.difference(&selected_by_path).copied().collect();
            selected_by_path.extend(to_process.iter().copied());
        }

        // Now process projects specified, including their dependencies (this
        // time in the forward direction).
        for name in names {
            ensure!(
                self.projects().contains_key(name),
                "Project '{}' not recognised",
                name
            );
        }
        let mut selected_by_name: HashSet<_> = names.iter().collect();
        let mut to_process = selected_by_name.clone();
        while !to_process.is_empty() {
            to_process = to_process
                .iter()
                .flat_map(|p| self.projects().get(*p).unwrap().dependencies().iter())
                .collect();
            to_process = to_process.difference(&selected_by_name).copied().collect();
            selected_by_name.extend(to_process.iter().copied());
        }

        let selected = &selected_by_path | &selected_by_name;

        Ok(SelectedProjects::new(
            selected
                .into_iter()
                .map(|name| self.projects().get(name).unwrap())
                .collect(),
        ))
    }

    /// Build up a map from path to project that covers that path. Uncovered
    /// paths are ignored.
    pub fn resolve_projects_for_paths<'a>(
        &'a self,
        paths: impl IntoIterator<Item = &'a TargetsPath>,
    ) -> HashMap<&'a TargetsPath, &'a ProjectConf> {
        paths
            .into_iter()
            .filter_map(|path| {
                self.projects
                    .values()
                    .find(|project| project.covers_path(&path.as_buck_path()))
                    .map(|project| (path, project))
            })
            .collect()
    }
}

/// Wrappping SelectedProjects in a module will prevent from using its struct
/// constructor, forcing usage of SelectedProjects::new that sorts the input.
mod selected_projects {
    use super::*;

    /// A newtype for better tracking list of projects selected to be processed
    #[derive(Debug, Getters)]
    #[getset(get = "pub")]
    pub struct SelectedProjects<'a> {
        /// List of selected projects.
        projects: Vec<&'a ProjectConf>,
    }

    impl<'a> SelectedProjects<'a> {
        pub(super) fn new(mut projects: Vec<&'a ProjectConf>) -> Self {
            projects.sort_unstable_by_key(|c| c.name());
            Self { projects }
        }
    }
}
pub use selected_projects::SelectedProjects;

/// Configuration of a project
#[derive(Debug, Deserialize, Getters)]
#[getset(get = "pub")]
#[serde(deny_unknown_fields)]
pub struct ProjectConf {
    /// Name of the project, used mostly as ID and for printing.
    name: String,
    /// Project roots which contain the files.
    #[serde(default)]
    roots: HashSet<String>,
    /// Set of globs that point to folders containing TARGETS and Cargo.toml
    /// files.
    #[serde(default, deserialize_with = "deserialize_globs")]
    include_globs: HashSet<Pattern>,
    /// Set of globs that exclude folders or files added by include_globs.
    #[serde(default, deserialize_with = "deserialize_globs")]
    exclude_globs: HashSet<Pattern>,
    /// Oncall that is responsible for this project.
    oncall: String,
    /// manual_cargo_toml if it is true then no files will be generated.
    /// This is useful when an autocargo maintained project has to depend on a
    /// manually maintained project.
    #[serde(default)]
    manual_cargo_toml: bool,
    /// Set of direct dependencies of this project. If one of the dependencies
    /// will change then all projects that depend on it (directly or indirectly)
    /// will be regenerated.
    #[serde(default)]
    dependencies: HashSet<String>,
    /// Configuration for project if it is being shipped to an external git
    /// repository
    oss_git_config: Option<OssGitConfig>,
    /// Configuration for creating a [workspace] section in an existing
    /// Cargo.toml file or a new one (virtual manifest).
    workspace_config: Option<WorkspaceConfig>,
    /// Default values to put in generated files for this project.
    #[serde(default)]
    defaults: ProjectConfDefaults,
    /// Paths to generate a Cargo.lock
    #[serde(default)]
    cargo_locks: Vec<PathInFbcode>,
}

/// Holds configuration for projects that are being shipped to external git
/// repository.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OssGitConfig {
    /// If set, this is the place where oss-ready Cargo.toml files will be stored
    /// for the project. Those files will have adjusted dependencies so that:
    /// - fbcode dependencies under the same git url will continue to use
    ///   path-dependencies
    /// - fbcode dependencies on crates from projects of different git url will
    ///   use git-dependencies as per their OssGitConfig setup
    /// - fbcode dependencies on crates from projects with no OssGitConfig will
    ///   be stripped
    ///
    /// The layout of oss-ready Cargo.toml files inside public_cargo_dir will
    /// match the layout of non-oss-ready Cargo.toml files realtive to parent of
    /// public_cargo_dir, so all the files generated for the project must be
    /// inside of parent of public_cargo_dir.
    ///
    /// # Example
    ///
    /// For project layout:
    ///   my_project
    ///   ├── Cargo.toml
    ///   └── foo
    ///       └── Cargo.toml
    /// and public_cargo_dir = "my_project/public_autocargo" the generation would
    /// look like this:
    ///   my_project
    ///   ├── Cargo.toml
    ///   ├── foo
    ///   │   └── Cargo.toml
    ///   └── public_autocargo
    ///       ├── Cargo.toml
    ///       └── foo
    ///          └── Cargo.toml
    ///
    /// # Note 1
    ///
    /// Autocargo will clean up the entire content of this directory on every
    /// regeneration, so it is advisable to keep it separate from `publid_tld`,
    /// `oss` and any other directories, also to not share public_cargo_dir with
    /// other projects.
    ///
    /// # Note 2
    ///
    /// If you choose "public_autocargo" as the name of this public_cargo_dir
    /// then the mergedriver will be able to automatically resolve merge
    /// conflicts in that directory when you rebase your commits.
    pub public_cargo_dir: Option<PathInFbcode>,
    /// Url of the git repo. Used to identify projects that are shipped to the
    /// same repo.
    pub git: String,
    /// Optional branch that will be used in dependencies.
    pub branch: Option<String>,
    /// Optional tag that will be used in dependencies.
    pub tag: Option<String>,
    /// Optional rev that will be used in dependencies.
    pub rev: Option<String>,
    /// Values to remove from  "default" features in published Cargo.toml.
    /// Cargo features are path structured, so if you specify foo, it will also strip bar/foo
    #[serde(default)]
    pub default_features_to_strip: Vec<String>,
}

/// Configuration for generating root Cargo.toml with autodiscovered [workspace]
/// section. The workspace members will consist of Cargo.toml files generated by
/// autocargo that are under the configured `scrape_dir`. Additionally this root
/// Cargo.toml will contain a [patch] section based on
/// fbsource/third-party/rust/Cargo.toml.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// All Cargo.toml files generated by autocargo under the `scrape_dir`
    /// directory will be included as members of this workspace.
    pub scrape_dir: PathInFbcode,
    /// Prefix to attach to path of each workspace member, useful when combined
    /// with `save_to_dir` and the project is using ShipIt that moves Cargo.toml
    /// files around.
    pub prefix_for_dir: Option<PathBuf>,
    /// Directory in the repo where to save the generated Cargo.toml file with
    /// [workspace] section. Defaults to scrape_dir. If it points to a Cargo.toml
    /// file generated by autocargo then the generated file will contain both the
    /// content it had generated and the workspace section, otherwise a new
    /// Cargo.toml file will be created with only workspace section (so called
    /// "virtual manifest").
    pub save_to_dir: Option<PathInFbcode>,
    /// How to generate the [patch] section.
    #[serde(default = "PatchGeneration::third_party_full")]
    pub patch_generation: PatchGeneration,
    /// Specify additional [patch] section entries for this workspace.
    ///
    /// Example:
    /// ```text
    /// [workspace_config.patch]
    /// "crates-io" = [
    ///   "addr2line",
    ///   ("bytecount", { git = "https://github.com/llogiq/bytecount", rev = "469eaf8395c99397cd64d059737a9054aa014088" }),
    /// ]
    /// ```
    ///
    /// This example copies the patch for `addr2line` from the third-party crates Cargo.toml
    /// and introduces a custom patch for `bytecount`.
    #[serde(default)]
    pub patch: PatchGenerationInput,
}

/// Decide how to generate the [patch] section.
///
/// The patch section can be generated based on the `mode`.  See
/// `PatchGenerationMode` for a description of each mode.
///
/// Once generated, entries can be excluded by adding them to
/// the `exclude` entry.
///
/// Example:
/// ```text
/// exclude = {
///     "crates-io": ["foo", "bar"]
/// }
/// ```
///
/// This example will exclude the `foo` and `bar` crates from the `crates-io`
/// registry patches.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PatchGeneration {
    /// Mode of patch generation to use.
    pub mode: PatchGenerationMode,

    /// Names of packages to exclude for each source.
    #[serde(default)]
    pub exclude: HashMap<String, Vec<String>>,
}

impl PatchGeneration {
    /// Patch generation generates no entries.
    pub fn empty() -> Self {
        PatchGeneration {
            mode: PatchGenerationMode::Empty,
            ..PatchGeneration::default()
        }
    }

    /// Patch generation copies all entries from third-party.
    pub fn third_party_full() -> Self {
        PatchGeneration {
            mode: PatchGenerationMode::ThirdPartyFull,
            ..PatchGeneration::default()
        }
    }
}

/// Modes of patch generation.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PatchGenerationMode {
    /// Generate no entries.
    #[default]
    Empty,
    /// Copy all entries from third-party.
    ThirdPartyFull,
}

/// A structure for describing a custom [patch] section that might mix values
/// copied from fbsource/third-party/rust/Cargo.toml and custom ones.
#[derive(Debug, Default, Deserialize)]
pub struct PatchGenerationInput(pub BTreeMap<String, Vec<PatchGenerationInputDep>>);

/// Iterator of patch generation input items.
pub type PatchGenerationInputIterItem<'a> = (&'a String, &'a Vec<PatchGenerationInputDep>);

impl PatchGenerationInput {
    /// Iterate over the patch generation input.
    pub fn iter(&self) -> impl Iterator<Item = PatchGenerationInputIterItem<'_>> {
        self.0.iter()
    }
}

/// Entry for [patch] section. It can be deserialized from:
/// ```text
/// {
///     "crates-io" = [
///         "foo",
///         ("bar", { git = "bar.com" })
///     ]
/// }
/// ```
///
/// The results will be:
/// - `PatchGenerationInputDep::FromFbsourceThirdParty("foo")`, which will patch
///   "foo" from registry "crates-io" using the entry from third-party
/// - `PatchGenerationInputDep::Dependency("bar", <Dep with git = "bar.com">)`,
///   which will patch "bar" from registry "crates-io" with `{ git = "bar.com" }`
#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum PatchGenerationInputDep {
    /// Copy the patch from fbsource/third-party/rust/Cargo.toml.
    FromFbsourceThirdParty(String),
    /// Set patch to this dependency definition.
    Dependency(String, Dependency),
}

/// Default values to put in generated files for project.
/// The attributes here are based on [::cargo_toml::Manifest] amd
/// [::cargo_toml::Package] plus some fields from
/// https://doc.rust-lang.org/cargo/reference/manifest.html.
#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfDefaults {
    /// Default values for "cargo-features" value of Cargo.toml.
    pub cargo_features: Vec<String>,
    /// Default values for [package] section of Cargo.toml.
    pub package: PackageDefaults,
    /// How to generate the [patch] section.
    #[serde(default = "PatchGeneration::empty")]
    pub patch_generation: PatchGeneration,
    /// Default additional entries for the [patch] section of Cargo.toml.
    ///
    /// Example:
    /// ```text
    /// [defaults.patch]
    /// "crates-io" = [
    ///   "addr2line",
    ///   ("bytecount", { git = "https://github.com/llogiq/bytecount", rev = "469eaf8395c99397cd64d059737a9054aa014088" }),
    /// ]
    /// ```
    ///
    /// This example copies the patch for `addr2line` from the third-party crates Cargo.toml
    /// and introduces a custom patch for `bytecount`.
    #[serde(default)]
    pub patch: PatchGenerationInput,
    /// Default value for [profile] section of Cargo.toml.
    pub profile: Profiles,
}

/// Default values for [package] section of Cargo.toml.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(missing_docs)]
pub struct PackageDefaults {
    pub version: String,
    pub authors: Vec<String>,
    pub edition: Edition,
    pub rust_version: Option<String>,
    pub description: Option<String>,
    pub documentation: Option<String>,
    /// Path to readme file relative to root of fbcode, it will be used to fill up
    /// [package.readme](https://doc.rust-lang.org/cargo/reference/manifest.html#the-readme-field)
    pub readme: Option<PathInFbcode>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    /// Path to license file relative to root of fbcode, it will be used to fill up
    /// [package.license-file](https://doc.rust-lang.org/cargo/reference/manifest.html#the-license-and-license-file-fields)
    pub license_file: Option<PathInFbcode>,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    /// Path to workspace relative to root of fbcode, it will be used to fill up
    /// [package.workspace](https://doc.rust-lang.org/cargo/reference/manifest.html#the-workspace-field)
    pub workspace: Option<PathInFbcode>,
    pub links: Option<String>,
    pub exclude: Vec<String>,
    pub include: Vec<String>,
    pub publish: Publish,
    pub metadata: Option<Value>,
}

impl Default for PackageDefaults {
    fn default() -> Self {
        Self {
            version: "0.0.0".to_owned(),
            authors: Vec::new(),
            edition: Edition::E2024,
            rust_version: None,
            description: None,
            documentation: None,
            readme: None,
            homepage: None,
            repository: None,
            license: None,
            license_file: None,
            keywords: Vec::new(),
            categories: Vec::new(),
            workspace: None,
            links: None,
            exclude: Vec::new(),
            include: Vec::new(),
            publish: Publish::default(),
            metadata: None,
        }
    }
}

fn process_dir(dir: PathBuf) -> BoxStream<'static, Result<PathBuf>> {
    async move {
        Ok(ReadDirStream::new(read_dir(dir).await?)
            .map_err(Error::from)
            .and_then(|entry| async move {
                let path = entry.path();
                let file_type = entry.file_type().await?;
                Ok(
                    if file_type.is_file()
                        && path.extension().and_then(|os| os.to_str()) == Some("toml")
                    {
                        stream::once(async move { Ok(path) }).boxed()
                    } else if file_type.is_dir() {
                        process_dir(path)
                    } else {
                        stream::empty().boxed()
                    },
                )
            })
            .try_flatten())
    }
    .try_flatten_stream()
    .boxed()
}

impl ProjectConf {
    /// Read the provided folder and deserialize each .toml file in it as
    /// TOML-encoded ProjectConf, then validate it and return AllProjects struct.
    pub async fn from_dir(dir: impl AsRef<Path>) -> Result<AllProjects> {
        let dir = dir.as_ref();
        let configs = process_dir(dir.to_owned())
            .and_then(|path| async move {
                let result: Result<Self> = try { from_str(&read_to_string(&path).await?)? };
                result.with_context(|| format!("While processing config file {}", path.display()))
            })
            .try_collect()
            .await
            .with_context(|| format!("While processing config dir {}", dir.display()))?;

        Ok(AllProjects {
            projects: validate_projects(configs)?,
        })
    }

    /// Return patterns for matching within the roots of the project.
    pub fn root_patterns(&self) -> Result<Vec<Pattern>, PatternError> {
        self.roots
            .iter()
            .map(|root| Pattern::new(&format!("{root}/**")))
            .collect()
    }

    fn covers_path(&self, path: &PathInFbcode) -> bool {
        let path: &Path = path.as_ref();
        for pattern in &self.exclude_globs {
            if pattern.matches_path(path) {
                return false;
            }
        }

        for pattern in &self.include_globs {
            if pattern.matches_path(path) {
                return true;
            }
        }

        for root in &self.roots {
            if path.starts_with(root) {
                return true;
            }
        }

        if let Some(public_dir) = self
            .oss_git_config
            .as_ref()
            .and_then(|c| c.public_cargo_dir.as_ref())
        {
            if path.starts_with(public_dir.as_ref()) {
                return true;
            }
        }

        false
    }
}

fn validate_projects(configs: Vec<ProjectConf>) -> Result<HashMap<String, ProjectConf>> {
    let mut all = HashMap::new();
    for conf in configs {
        let name = conf.name().to_owned();
        if all.insert(name.clone(), conf).is_some() {
            bail!(
                "The names of projects are not unique, one of the offenders is: {}",
                name
            );
        }
    }

    for conf in all.values() {
        for dep in conf.dependencies() {
            ensure!(
                all.contains_key(dep),
                "Dependency {} of project {} does not exists",
                dep,
                conf.name()
            );
        }

        for lock_path in &conf.cargo_locks {
            let lock_file = lock_path.join_to_path_in_fbcode("Cargo.lock");
            if !conf.covers_path(&lock_file) {
                bail!(
                    "cargo_lock path '{}' is not contained in project '{}' (within the include_globs).",
                    lock_path.as_ref().display(),
                    conf.name()
                );
            }
        }
    }

    Ok(all)
}

#[cfg(test)]
mod test {
    use assert_matches::assert_matches;
    use itertools::Itertools;
    use itertools::assert_equal;
    use maplit::hashmap;
    use serde_json::Value;
    use serde_json::from_value;
    use serde_json::json;

    use super::*;
    use crate::paths::TargetsPath;

    fn pc(json_value: Value) -> ProjectConf {
        from_value(json_value).unwrap()
    }

    fn assert_selected<'a>(
        selected: &SelectedProjects<'a>,
        names: impl IntoIterator<Item = &'a str>,
    ) {
        assert_equal(
            selected.projects().iter().map(|p| p.name()),
            names.into_iter().map(|s| -> &str { s }),
        )
    }

    #[test]
    fn select_all_test() {
        let pc = |name: &str| {
            pc(json!({
                "name": name,
                "include_globs": [],
                "oncall": "oncall_name",
            }))
        };

        assert_selected(
            &AllProjects {
                projects: validate_projects(vec![pc("proj1"), pc("proj3"), pc("proj2")]).unwrap(),
            }
            .select_all(),
            vec!["proj1", "proj2", "proj3"],
        )
    }

    #[test]
    fn select_based_on_paths_and_names_test() {
        let pc = |name: &str, inc: &[&str], deps: &[&str]| {
            pc(json!({
                "name": name,
                "include_globs": inc,
                "oncall": "oncall_name",
                "dependencies": deps,
            }))
        };
        let p = PathInFbcode::new_mock;
        let s = String::from;

        let all_proj = AllProjects {
            projects: validate_projects(vec![
                pc("proj1", &["a"], &[]),
                pc("proj2", &["b"], &["proj1"]),
                pc("proj3", &["c"], &["proj2"]),
                pc("proj4", &["b"], &[]),
            ])
            .unwrap(),
        };

        assert_selected(
            &all_proj
                .select_based_on_paths_and_names(&[p("a")], &[])
                .unwrap(),
            vec!["proj1", "proj2", "proj3"],
        );

        assert_selected(
            &all_proj
                .select_based_on_paths_and_names(&[p("b")], &[])
                .unwrap(),
            vec!["proj2", "proj3", "proj4"],
        );

        assert_selected(
            &all_proj
                .select_based_on_paths_and_names(&[p("c")], &[])
                .unwrap(),
            vec!["proj3"],
        );

        assert_selected(
            &all_proj
                .select_based_on_paths_and_names(&[p("a"), p("b")], &[])
                .unwrap(),
            vec!["proj1", "proj2", "proj3", "proj4"],
        );

        assert_selected(
            &all_proj
                .select_based_on_paths_and_names(&[], &[s("proj1")])
                .unwrap(),
            vec!["proj1"],
        );

        assert_selected(
            &all_proj
                .select_based_on_paths_and_names(&[], &[s("proj3")])
                .unwrap(),
            vec!["proj1", "proj2", "proj3"],
        );

        assert_selected(
            &all_proj
                .select_based_on_paths_and_names(&[p("b")], &[s("proj2")])
                .unwrap(),
            vec!["proj1", "proj2", "proj3", "proj4"],
        );
    }

    #[test]
    fn resolve_projects_for_paths_test() {
        let pc = |name: &str, roots: &[&str], inc: &[&str]| {
            pc(json!({
                "name": name,
                "roots": roots,
                "include_globs": inc,
                "oncall": "oncall_name",
            }))
        };
        let p = |s: &str| TargetsPath::new(PathInFbcode::new_mock(s)).unwrap();

        let all_proj = AllProjects {
            projects: validate_projects(vec![
                pc("proj1", &[], &["a/**"]),
                pc("proj2", &[], &["b/**"]),
                pc("proj3", &["c"], &[]),
            ])
            .unwrap(),
        };

        let pa = p("a/BUCK");
        let pb = p("b/BUCK.v2");
        let pc = p("c/TARGETS");
        let pd = p("d/TARGETS.v2");

        assert_eq!(
            all_proj
                .resolve_projects_for_paths([&pa, &pb, &pc, &pd])
                .into_iter()
                .map(|(k, v)| -> (&TargetsPath, &str) { (k, v.name()) })
                .collect::<HashMap<_, _>>(),
            hashmap! {
                &pa => "proj1",
                &pb => "proj2",
                &pc => "proj3",
            }
        );
    }

    #[test]
    fn covers_path_test() {
        let pc = |roots: &[&str], inc: &[&str], exc: &[&str]| {
            pc(json!({
                "name": "proj",
                "roots": roots,
                "include_globs": inc,
                "exclude_globs": exc,
                "oncall": "oncall_name",
            }))
        };
        let p = PathInFbcode::new_mock;

        assert!(pc(&[], &["a"], &[]).covers_path(&p("a")));
        assert!(pc(&[], &["a"], &["a/**"]).covers_path(&p("a")));
        assert!(!pc(&[], &["a"], &["a"]).covers_path(&p("a")));
        assert!(!pc(&[], &["a/**"], &[]).covers_path(&p("a")));

        assert!(!pc(&[], &[], &[]).covers_path(&p("a/b/c")));
        assert!(pc(&[], &["a/**"], &[]).covers_path(&p("a/b/c")));
        assert!(!pc(&[], &["a/**"], &["a/b/**"]).covers_path(&p("a/b/c")));
        assert!(pc(&[], &["a/**/c"], &[]).covers_path(&p("a/b/c")));
        assert!(!pc(&[], &["a/**/b"], &[]).covers_path(&p("a/b/c")));
        assert!(!pc(&[], &["a/**"], &["a/**/c"]).covers_path(&p("a/b/c")));
        assert!(pc(&[], &["a/**"], &["a/**/a", "a/**/b"]).covers_path(&p("a/b/c")));
        assert!(!pc(&[], &["a/**"], &["a/**/a", "a/**/c"]).covers_path(&p("a/b/c")));

        assert!(pc(&["a"], &[], &[]).covers_path(&p("a/b/c")));
        assert!(!pc(&["a"], &[], &["a/b/**"]).covers_path(&p("a/b/c")));
        assert!(!pc(&["a"], &[], &["a/**/c"]).covers_path(&p("a/b/c")));
        assert!(pc(&["a"], &[], &["a/**/a", "a/**/b"]).covers_path(&p("a/b/c")));
    }

    #[test]
    fn validate_projects_test() {
        let pc = |name: &str, deps: &[&str]| {
            pc(json!({
                "name": name,
                "include_globs": [],
                "oncall": "oncall_name",
                "dependencies": deps,
            }))
        };

        assert_matches!(
            validate_projects(vec![pc("proj1", &[]), pc("proj2", &[]), pc("proj2", &[])]),
            Err(err) => {
                assert_eq!(
                    err.to_string(),
                    "The names of projects are not unique, one of the offenders is: proj2"
                )
            }
        );

        assert_matches!(
            validate_projects(vec![pc("proj1", &[]), pc("proj2", &["proj1", "proj3"])]),
            Err(err) => {
                assert_eq!(
                    err.to_string(),
                    "Dependency proj3 of project proj2 does not exists"
                )
            }
        );

        assert_matches!(
            validate_projects(vec![pc("proj1", &[]), pc("proj2", &["proj1"])]),
            Ok(map) => {
                for (k, v) in &map {
                    assert_eq!(k, v.name());
                }
                assert_equal(map.keys().sorted(), &["proj1", "proj2"]);
            }
        );
    }
}
