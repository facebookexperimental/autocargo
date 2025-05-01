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
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use futures::FutureExt;
use futures::TryStreamExt;
use futures::future::LocalBoxFuture;
use futures::stream::FuturesUnordered;
use itertools::multipeek;
use serde_json::from_slice;
use slog::Logger;
use thrift_compiler::GenContext;
use tokio::fs::read;
use tokio::fs::read_to_string;

use super::commands::buck_build_cratemaps_cmd;
use super::commands::buck_build_manifests_cmd;
use super::commands::buck_query_manifests_cmd;
use super::raw_manifest::AutocargoThrift;
use super::raw_manifest::RawBuckManifest;
use super::rules::BuckManifestRule;
use super::rules::FbcodeBuckRule;
use super::rules::ThriftCratemapRule;
use crate::paths::FbcodeRoot;
use crate::paths::TargetsPath;
use crate::util::command_runner::MockableCommandRunner;

/// Structure responsible for querying, building and parsing rust manifests using
/// buck.
pub struct BuckManifestLoader<'input> {
    logger: &'input Logger,
    fbcode_root: &'input FbcodeRoot,
    use_isolation_dir: bool,
    rules: Vec<BuckManifestRule>,
    cmd_runner: MockableCommandRunner,
}

impl<'input> BuckManifestLoader<'input> {
    /// Given list of TARGETS paths create the loader by querying Buck for rust
    /// manifest in those TARGETS files.
    pub fn from_targets_paths<'fut>(
        logger: &'input Logger,
        fbcode_root: &'input FbcodeRoot,
        use_isolation_dir: bool,
        targets: impl IntoIterator<Item = &'fut TargetsPath> + 'fut,
        cmd_runner: MockableCommandRunner,
    ) -> LocalBoxFuture<'fut, Result<Self>>
    where
        'input: 'fut,
    {
        async move {
            let dbg_name = "buck query manifests";
            let mut targets = multipeek(targets);
            if targets.peek().is_none() {
                return Ok(Self {
                    logger,
                    fbcode_root,
                    use_isolation_dir,
                    rules: Vec::new(),
                    cmd_runner,
                });
            }

            let output = cmd_runner
                .run(
                    logger,
                    dbg_name,
                    Duration::from_secs(5),
                    buck_query_manifests_cmd(fbcode_root, use_isolation_dir, targets).boxed_local(),
                )
                .await?;

            ensure!(output.status.success(), "Failed to run '{}'", dbg_name);

            let rules = from_slice::<Vec<BuckManifestRule>>(&output.stdout)
                .with_context(|| format!("Failed to parse output of '{dbg_name}'"))?;

            Ok(Self {
                logger,
                fbcode_root,
                use_isolation_dir,
                rules,
                cmd_runner,
            })
        }
        .boxed_local()
    }

    /// Given list of buck rules (presumably coming from dependencies of known
    /// manifests) create the loader by querying Buck for rust manifest for those
    /// manifests. Querying is necessary since we don't know which of those rules
    /// are Rust rules and which are not (like C++ or Python) that do not have
    /// manifests. Since querying for a fully quallified rule is considered an
    /// error if it doesn't exist we query all rules in targets containing
    /// provided rules and then filter the result.
    pub fn from_rust_buck_rules<'fut>(
        logger: &'input Logger,
        fbcode_root: &'input FbcodeRoot,
        use_isolation_dir: bool,
        input_rules: impl IntoIterator<Item = &'fut FbcodeBuckRule>,
        cmd_runner: MockableCommandRunner,
    ) -> LocalBoxFuture<'fut, Result<Self>>
    where
        'input: 'fut,
    {
        let input_rules = input_rules
            .into_iter()
            .map(BuckManifestRule::from)
            .collect::<HashSet<_>>();

        async move {
            let targets = input_rules
                .iter()
                .map(|rule| &rule.as_ref().path)
                .collect::<HashSet<_>>();

            let mut loader = Self::from_targets_paths(
                logger,
                fbcode_root,
                use_isolation_dir,
                targets,
                cmd_runner,
            )
            .await?;
            loader.rules.retain(|rule| input_rules.contains(rule));
            Ok(loader)
        }
        .boxed_local()
    }

    /// Builds rust manifests using buck (they are already validated to exist
    /// thanks to buck query call when creating this structure) and then reads
    /// and parses the results into RawBuckManifest.
    pub async fn load(self) -> Result<HashMap<FbcodeBuckRule, RawBuckManifest>> {
        self.build()
            .await?
            .into_iter()
            .map(|(rule, out_path)| async move {
                let try_parsed: Result<RawBuckManifest> =
                    try { from_slice(&read(&out_path).await?)? };
                let parsed = try_parsed
                    .with_context(|| format!("While reading file {}", out_path.display()))?;
                let rule = FbcodeBuckRule::try_from(rule)
                    .context("While parsing output of buck build manifests command")?;
                ensure!(
                    rule.name == parsed.name,
                    "Name of the rule ({:?}) is not the same as declared in manifest: {:#?}",
                    rule,
                    parsed
                );
                Ok((rule, parsed))
            })
            .collect::<FuturesUnordered<_>>()
            .try_collect()
            .await
    }

    async fn build(self) -> Result<HashMap<BuckManifestRule, PathBuf>> {
        let Self {
            logger,
            fbcode_root,
            use_isolation_dir,
            rules,
            cmd_runner,
        } = self;
        let dbg_name = "buck build manifest rules";

        if rules.is_empty() {
            return Ok(HashMap::new());
        }

        let output = cmd_runner
            .run(
                logger,
                dbg_name,
                Duration::from_secs(5),
                buck_build_manifests_cmd(fbcode_root, use_isolation_dir, &rules).boxed_local(),
            )
            .await?;

        ensure!(output.status.success(), "Failed to run '{}'", dbg_name);

        from_slice::<HashMap<BuckManifestRule, PathBuf>>(&output.stdout)
            .with_context(|| format!("Failed to parse output of '{dbg_name}'"))
    }
}

/// Structure responsible for building thrift cratemaps using buck.
pub struct ThriftCratemapLoader<'input> {
    logger: &'input Logger,
    fbcode_root: &'input FbcodeRoot,
    use_isolation_dir: bool,
    rules: Vec<ThriftCratemapRule>,
    cmd_runner: MockableCommandRunner,
}

impl<'input> ThriftCratemapLoader<'input> {
    /// Checks which RawBuckManifest contain a thrift section and prepares the
    /// loader for querying cratemaps for those rules.
    pub fn from_rules_and_raw<'a>(
        logger: &'input Logger,
        fbcode_root: &'input FbcodeRoot,
        use_isolation_dir: bool,
        rules_and_raw: impl IntoIterator<Item = (&'a FbcodeBuckRule, &'a RawBuckManifest)>,
        cmd_runner: MockableCommandRunner,
    ) -> Self {
        Self {
            logger,
            fbcode_root,
            use_isolation_dir,
            rules: rules_and_raw
                .into_iter()
                .filter_map(|(rule, raw)| {
                    if let Some(AutocargoThrift {
                        gen_context: GenContext::Types,
                        ..
                    }) = raw.autocargo.thrift
                    {
                        Some(ThriftCratemapRule::from_library_rule(rule.clone()))
                    } else {
                        None
                    }
                })
                .collect(),
            cmd_runner,
        }
    }

    /// Builds cratemaps using buck and returns map from rule to their
    /// corresponding cratemap content.
    pub async fn load(self) -> Result<HashMap<FbcodeBuckRule, String>> {
        self.build()
            .await?
            .into_iter()
            .map(|(rule, out_path)| async move {
                let rule = rule.to_library_rule();
                let content = read_to_string(&out_path)
                    .await
                    .with_context(|| format!("While reading file {}", out_path.display()))?;
                Ok((rule, content))
            })
            .collect::<FuturesUnordered<_>>()
            .try_collect()
            .await
    }

    async fn build(self) -> Result<HashMap<ThriftCratemapRule, PathBuf>> {
        let Self {
            logger,
            fbcode_root,
            use_isolation_dir,
            rules,
            cmd_runner,
        } = self;
        let dbg_name = "buck build thrift cratemaps";

        if rules.is_empty() {
            return Ok(HashMap::new());
        }

        let output = cmd_runner
            .run(
                logger,
                dbg_name,
                Duration::from_secs(5),
                buck_build_cratemaps_cmd(fbcode_root, use_isolation_dir, &rules).boxed_local(),
            )
            .await?;

        ensure!(output.status.success(), "Failed to run '{}'", dbg_name);

        from_slice::<HashMap<ThriftCratemapRule, PathBuf>>(&output.stdout)
            .with_context(|| format!("Failed to parse output of '{dbg_name}'"))
    }
}

impl fmt::Debug for BuckManifestLoader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuckManifestLoader")
            .field("logger", &"Logger".to_owned())
            .field("fbcode_root", &self.fbcode_root)
            .field("rules", &self.rules)
            .field("cmd_runner", &"MockableCommandRunner".to_owned())
            .finish()
    }
}

impl fmt::Debug for ThriftCratemapLoader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThriftCratemapLoader")
            .field("logger", &"Logger".to_owned())
            .field("fbcode_root", &self.fbcode_root)
            .field("rules", &self.rules)
            .field("cmd_runner", &"MockableCommandRunner".to_owned())
            .finish()
    }
}

#[cfg(test)]
mod test {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;
    use std::path::Path;
    use std::process::ExitStatus;
    use std::process::Output;

    use assert_matches::assert_matches;
    use itertools::Itertools;
    use maplit::hashmap;
    use serde_json::json;
    use serde_json::to_vec;
    use slog::o;

    use super::*;
    use crate::buck_processing::test_utils::TmpManifests;
    use crate::paths::PathInFbcode;

    #[tokio::test]
    async fn buck_maniest_loader_test_from_targets_paths() {
        let logger = Logger::root(slog::Discard, o!());
        let fbcode_root = FbcodeRoot::new_mock("/foo/bar");

        let tp = |path: &str| TargetsPath::new(PathInFbcode::new_mock(path)).unwrap();

        assert_matches!(
            BuckManifestLoader::from_targets_paths(
                &logger,
                &fbcode_root,
                false, // use_isolation_dir
                &Vec::<TargetsPath>::new(),
                MockableCommandRunner::default(),
            ).await,
            Ok(loader) => {
                assert_eq!(loader.rules, vec![]);
            }
        );

        let cmd_runner = {
            let mut cmd_runner = MockableCommandRunner::default();
            cmd_runner.expect_run().return_once(|_, _, _, _| {
                Ok(Output {
                    status: ExitStatus::from_raw(0),
                    stderr: vec![],
                    stdout: to_vec(&json!(["//fiz:biz-rust-manifest"])).unwrap(),
                })
            });
            cmd_runner
        };

        assert_matches!(
            BuckManifestLoader::from_targets_paths(
                &logger,
                &fbcode_root,
                false, // use_isolation_dir
                &vec![tp("unimportant/TARGETS")],
                cmd_runner,
            ).await,
            Ok(loader) => {
                assert_eq!(loader.rules, vec![BuckManifestRule::from(&FbcodeBuckRule {
                    path: tp("fiz/TARGETS"),
                    name: "biz".to_owned(),
                })]);
            }
        );
    }

    #[tokio::test]
    async fn buck_maniest_loader_test_from_rust_buck_rules() {
        let logger = Logger::root(slog::Discard, o!());
        let fbcode_root = FbcodeRoot::new_mock("/foo/bar");

        let tp = |path: &str| TargetsPath::new(PathInFbcode::new_mock(path)).unwrap();

        let cmd_runner = {
            let mut cmd_runner = MockableCommandRunner::default();
            cmd_runner.expect_run().return_once(|_, _, _, _| {
                Ok(Output {
                    status: ExitStatus::from_raw(0),
                    stderr: vec![],
                    stdout: to_vec(&json!([
                        "//fiz:biz-rust-manifest",
                        "//fiz:biz2-rust-manifest"
                    ]))
                    .unwrap(),
                })
            });
            cmd_runner
        };

        assert_matches!(
            BuckManifestLoader::from_rust_buck_rules(
                &logger,
                &fbcode_root,
                false, // use_isolation_dir
                &vec![FbcodeBuckRule {
                    path: tp("fiz/TARGETS"),
                    name: "biz2".to_owned()
                }],
                cmd_runner,
            ).await,
            Ok(loader) => {
                assert_eq!(loader.rules, vec![BuckManifestRule::from(&FbcodeBuckRule {
                    path: tp("fiz/TARGETS"),
                    name: "biz2".to_owned(),
                })]);
            }
        );
    }

    #[tokio::test]
    async fn buck_maniest_loader_test_build() {
        let tp = |path: &str| TargetsPath::new(PathInFbcode::new_mock(path)).unwrap();

        let make_rule = || {
            BuckManifestRule::from(&FbcodeBuckRule {
                path: tp("fiz/TARGETS"),
                name: "biz".to_owned(),
            })
        };
        assert_matches!(
            BuckManifestLoader {
                logger: &Logger::root(slog::Discard, o!()),
                fbcode_root: &FbcodeRoot::new_mock("/foo/bar"),
                use_isolation_dir: false,
                rules: vec![make_rule()],
                cmd_runner: {
                    let mut cmd_runner = MockableCommandRunner::default();
                    cmd_runner.expect_run().return_once(|_, _, _, _| {
                        Ok(Output {
                            status: ExitStatus::from_raw(0),
                            stderr: vec![],
                            stdout: to_vec(&json!({
                                "//fiz:biz-rust-manifest": "/foo/bar/output/manifest.json",
                            })).unwrap(),
                        })
                    });
                    cmd_runner
                }
            }.build().await,
            Ok(map) => {
                assert_eq!(
                    map,
                    hashmap! {
                        make_rule() => Path::new("/foo/bar/output/manifest.json").to_owned()
                    }
                )
            }
        );
    }

    #[tokio::test]
    async fn buck_maniest_loader_test_load() {
        let tp = |path: &str| TargetsPath::new(PathInFbcode::new_mock(path)).unwrap();

        let TmpManifests {
            autocargo_file,
            autocargo_lib_file,
            ..
        } = TmpManifests::new();
        assert_matches!(
            BuckManifestLoader {
                logger: &Logger::root(slog::Discard, o!()),
                fbcode_root: &FbcodeRoot::new_mock("/foo/bar"),
                use_isolation_dir: false,
                rules: vec![
                    BuckManifestRule::from(&FbcodeBuckRule {
                        path: tp("fiz/TARGETS"),
                        name: "biz".to_owned(),
                    }),
                ],
                cmd_runner: {
                    let mut cmd_runner = MockableCommandRunner::default();
                    cmd_runner.expect_run().return_once({
                        let p1 = autocargo_file.path().to_owned();
                        let p2 = autocargo_lib_file.path().to_owned();
                        move |_, _, _, _| {
                            Ok(Output {
                                status: ExitStatus::from_raw(0),
                                stderr: vec![],
                                stdout: to_vec(&json!({
                                    "//fiz:autocargo-rust-manifest": p1,
                                    "//fiz:autocargo_lib-rust-manifest": p2,
                                })).unwrap(),
                            })
                        }
                    });
                    cmd_runner
                }
            }.load().await,
            Ok(map) => {
                assert_eq!(
                    map.into_iter()
                        .sorted_by(|(k1, _), (k2, _)| Ord::cmp(k1, k2))
                        .map(|(k, v)| (k, v.name))
                        .collect::<Vec<_>>(),
                    vec![
                        (
                            FbcodeBuckRule {
                                path: tp("fiz/TARGETS"),
                                name: "autocargo".to_owned(),
                            },
                            "autocargo".to_owned(),
                        ),
                        (
                            FbcodeBuckRule {
                                path: tp("fiz/TARGETS"),
                                name: "autocargo_lib".to_owned(),
                            },
                            "autocargo_lib".to_owned(),
                        ),
                    ]
                );
            }
        );
    }
}
