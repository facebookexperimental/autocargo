/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use ::glob::Pattern;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use cfg_if::cfg_if;
use futures::TryFutureExt;
use futures::TryStreamExt;
use futures::future::try_join3;
use futures::stream;
use futures::stream::FuturesUnordered;
use tokio::task::spawn_blocking;

use super::ProjectFiles;
use super::ProjectLoader;
use crate::config::ProjectConf;
use crate::paths::CargoTomlPath;
use crate::paths::FbcodeRoot;
use crate::paths::PathInFbcode;
use crate::paths::TargetsPath;

// This is to help with mocking calls to [::glob::glob], as described here:
// https://docs.rs/mockall/0.8.3/mockall/index.html#mocking-structs
cfg_if! {
    if #[cfg(test)] {
        use self::glob::MockGlob as Glob;
    } else {
        use self::glob::Glob;
    }
}

impl<'proj, 'a> ProjectLoader<'proj, 'a> {
    /// Given include/exclude globs search for covered paths per each of
    /// the selected project.
    pub(super) async fn project_files_load(&self) -> Result<Vec<ProjectFiles<'proj>>> {
        get_files_for_multiple_projects(
            Arc::new(Glob::default()),
            self.fbcode_root,
            self.configs.projects().iter().cloned(), // && -> & with cloned
        )
        .await
    }
}

async fn get_files_for_multiple_projects<'proj>(
    glob: Arc<Glob>,
    fbcode_root: &FbcodeRoot,
    configs: impl IntoIterator<Item = &'proj ProjectConf>,
) -> Result<Vec<ProjectFiles<'proj>>> {
    let mut result: Vec<_> = configs
        .into_iter()
        .map(|conf| {
            get_files_for_project(glob.clone(), fbcode_root, conf).and_then(
                move |(cargo, targets, additional)| async move {
                    Ok(ProjectFiles::new(conf, cargo, targets, additional))
                },
            )
        })
        .collect::<FuturesUnordered<_>>()
        .try_collect()
        .await?;

    result.sort_unstable_by_key(|proj_files| proj_files.conf().name());
    Ok(result)
}

async fn get_files_for_project(
    glob: Arc<Glob>,
    fbcode_root: &FbcodeRoot,
    conf: &ProjectConf,
) -> Result<(Vec<CargoTomlPath>, Vec<TargetsPath>, Vec<PathInFbcode>)> {
    let maybe_public_cargo_dir_pattern = maybe_public_cargo_dir_pattern(conf)?;
    let root_patterns = conf.root_patterns()?;

    let exclude_globs: Arc<Vec<_>> = Arc::new(conf.exclude_globs().iter().cloned().collect());

    let (cargo_set, targets_set, additional_set) = conf
        .include_globs()
        .iter()
        .chain(root_patterns.iter())
        .chain(maybe_public_cargo_dir_pattern.as_ref())
        .map(|include_pat| {
            let cargo_fut = get_files_helper(
                glob.clone(),
                fbcode_root.clone(),
                conf.name(),
                include_pat.clone(),
                CargoTomlPath::filename(),
                CargoTomlPath::new,
                exclude_globs.clone(),
            );

            let targets_fut = TargetsPath::filenames()
                .iter()
                .map(|filename| {
                    get_files_helper(
                        glob.clone(),
                        fbcode_root.clone(),
                        conf.name(),
                        include_pat.clone(),
                        filename,
                        TargetsPath::new,
                        exclude_globs.clone(),
                    )
                    .map_ok(|vec| stream::iter(vec.into_iter().map(Result::<_>::Ok)))
                })
                .collect::<FuturesUnordered<_>>()
                .try_flatten()
                .try_collect::<Vec<_>>();

            let additional_fut = PathInFbcode::all_additional_filenames()
                .iter()
                .map(|filename| {
                    get_files_helper(
                        glob.clone(),
                        fbcode_root.clone(),
                        conf.name(),
                        include_pat.clone(),
                        filename,
                        Ok,
                        exclude_globs.clone(),
                    )
                    .map_ok(|vec| stream::iter(vec.into_iter().map(Result::<_>::Ok)))
                })
                .collect::<FuturesUnordered<_>>()
                .try_flatten()
                .try_collect::<Vec<_>>();

            try_join3(cargo_fut, targets_fut, additional_fut)
        })
        .collect::<FuturesUnordered<_>>()
        .try_fold(
            (HashSet::new(), HashSet::new(), HashSet::new()),
            |(mut cargo_set, mut targets_set, mut additional_set),
             (cargo_vec, targets_vec, additional_vec)| async move {
                cargo_set.extend(cargo_vec);
                targets_set.extend(targets_vec);
                additional_set.extend(additional_vec);
                Ok((cargo_set, targets_set, additional_set))
            },
        )
        .await
        .with_context(|| format!("While glob-searching files for project {}", conf.name()))?;

    let cargo_vec: Vec<_> = cargo_set.into_iter().collect();
    let targets_vec: Vec<_> = targets_set.into_iter().collect();
    let additional_vec: Vec<_> = additional_set.into_iter().collect();

    Ok((cargo_vec, targets_vec, additional_vec))
}

/// Create a pattern from public_cargo_dir if it is present in the project.
fn maybe_public_cargo_dir_pattern(conf: &ProjectConf) -> Result<Option<Pattern>> {
    conf.oss_git_config()
        .as_ref()
        .and_then(|oss_git_config| oss_git_config.public_cargo_dir.as_ref())
        .map(|public_cargo_dir| -> Result<_> {
            Ok(Pattern::new(
                public_cargo_dir
                    .as_ref()
                    .join("**")
                    .to_str()
                    .ok_or_else(|| anyhow!("Failed to concatenate '**'"))?,
            )?)
        })
        .transpose()
        .with_context(|| {
            format!(
                "While creating pattern from public_cargo_dir for project: {}",
                conf.name()
            )
        })
}

/// [::glob::glob] is a non-async function that does filesystem lookups. This
/// function runs this glob searching inside a [::tokio::task::spawn_blocking]
/// making the whole operation asynchronous. The downside is that spawn_blocking
/// requires a `FnOnce + Send + 'static` so all the necessary input has to be
/// moved into it.
async fn get_files_helper<T: Send + 'static>(
    glob: Arc<Glob>,
    fbcode_root: FbcodeRoot,
    proj_name: &str,
    include_pat: Pattern,
    file_name: &str,
    path_converter: impl Fn(PathInFbcode) -> Result<T> + Send + 'static,
    exclude_globs: Arc<Vec<Pattern>>,
) -> Result<Vec<T>> {
    let fut = spawn_blocking({
        let file_name = file_name.to_owned();
        move || -> Result<Vec<T>> {
            let include_pat = AsRef::<Path>::as_ref(&fbcode_root)
                .join(include_pat.as_str())
                .join(file_name);
            let paths = glob.glob(
                include_pat
                    .to_str()
                    .ok_or_else(|| anyhow!("Failed to convert {:?} to string", include_pat))?,
            )?;

            paths
                .into_iter()
                .map(|path| PathInFbcode::from_absolute(&fbcode_root, path?))
                .filter_map(|path| {
                    let path = match path {
                        Ok(path) => path,
                        Err(err) => return Some(Err(err)),
                    };

                    for pattern in exclude_globs.iter() {
                        if pattern.matches_path(path.as_ref()) {
                            return None;
                        }
                    }

                    Some(path_converter(path))
                })
                .collect()
        }
    });

    fut.await
        .with_context(|| format!("Failed to join on glob-searching task for project {proj_name}"))?
        .with_context(|| format!("While glob-searching {file_name} files for project {proj_name}"))
}

/// This module provides the Glob structure that is used as a replacement for
/// [::glob::glob] function, so that calling glob searching can be mocked in
/// test runs via [::mockall] crate.
mod glob {
    use std::path::PathBuf;

    use anyhow::Result;
    use glob::glob;
    use mockall::automock;

    #[derive(Default)]
    pub struct Glob {}

    #[automock]
    impl Glob {
        #[allow(dead_code)]
        pub fn glob(&self, pattern: &str) -> Result<Box<dyn Iterator<Item = Result<PathBuf>>>> {
            Ok(Box::new(glob(pattern)?.map(|p| Ok(p?))))
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    use anyhow::Error;
    use anyhow::bail;
    use assert_matches::assert_matches;
    use maplit::hashmap;
    use serde_json::from_value;
    use serde_json::json;

    use super::*;

    type GlobRet = Result<Vec<Result<&'static str, &'static str>>, &'static str>;

    fn glob_mock(mocked_values: HashMap<&'static str, GlobRet>) -> Glob {
        let mut glob_mock = Glob::default();
        let times_max = mocked_values.keys().count();
        let times_min = if mocked_values.values().any(|v| v.is_err()) {
            // If there is an error the execution might short-circuit and glob
            // will be called only once
            1
        } else {
            times_max
        };

        glob_mock
            .expect_glob()
            .times(times_min..=times_max)
            .returning({
                let mocked_values = Mutex::new(mocked_values);
                move |pattern| {
                    let mut mocked_values = mocked_values.lock().unwrap();
                    let mocked_return = mocked_values.remove(pattern);
                    match mocked_return {
                        Some(ret) => ret
                            .map(|ok| -> Box<dyn Iterator<Item = _>> {
                                Box::new(ok.into_iter().map(|s| {
                                    s.map(|s_ok| Path::new(s_ok).to_owned()).map_err(Error::msg)
                                }))
                            })
                            .map_err(Error::msg),
                        None => bail!(
                            "Pattern {:?} not found or already taken in mocked values {:?}",
                            pattern,
                            mocked_values
                        ),
                    }
                }
            });
        glob_mock
    }

    fn vec_cargo(ps: &[&str]) -> Vec<CargoTomlPath> {
        ps.iter()
            .map(|p| CargoTomlPath::new(PathInFbcode::new_mock(p)).unwrap())
            .collect()
    }

    fn vec_targets(ps: &[&str]) -> Vec<TargetsPath> {
        ps.iter()
            .map(|p| TargetsPath::new(PathInFbcode::new_mock(p)).unwrap())
            .collect()
    }

    fn vec_additional(ps: &[&str]) -> Vec<PathInFbcode> {
        ps.iter().map(PathInFbcode::new_mock).collect()
    }

    #[tokio::test]
    async fn get_files_for_multiple_projects_test() {
        if cfg!(windows) {
            return; // Broken on Windows
        }

        let pc = |name: &str, inc: &[&str], exc: &[&str]| -> ProjectConf {
            from_value(json!({
                "name": name,
                "include_globs": inc,
                "exclude_globs": exc,
                "oncall": "oncall_name",
            }))
            .unwrap()
        };

        let configs = vec![
            pc("proj2", &["b/c/**"], &[]),
            pc("proj1", &["b/c/d/**"], &["b/c/d/f/**"]),
        ];

        let pfs = get_files_for_multiple_projects(
            Arc::new(glob_mock(hashmap! {
                "/a/b/c/**/Cargo.toml" => Ok(vec![Ok("/a/b/c/Cargo.toml")]),
                "/a/b/c/**/BUCK" => Ok(vec![]),
                "/a/b/c/**/TARGETS" => Ok(vec![]),
                "/a/b/c/**/BUCK.v2" => Ok(vec![]),
                "/a/b/c/**/TARGETS.v2" => Ok(vec![]),
                "/a/b/c/**/thrift_lib.rs" => Ok(vec![]),
                "/a/b/c/**/thrift_build.rs" => Ok(vec![]),
                "/a/b/c/d/**/Cargo.toml" => Ok(vec![
                    Ok("/a/b/c/d/e/Cargo.toml"),
                    Ok("/a/b/c/d/f/Cargo.toml"),
                    Ok("/a/b/c/d/g/Cargo.toml"),
                ]),
                "/a/b/c/d/**/BUCK" => Ok(vec![Ok("/a/b/c/d/BUCK")]),
                "/a/b/c/d/**/TARGETS" => Ok(vec![Ok("/a/b/c/d/TARGETS")]),
                "/a/b/c/d/**/BUCK.v2" => Ok(vec![Ok("/a/b/c/d/BUCK.v2")]),
                "/a/b/c/d/**/TARGETS.v2" => Ok(vec![Ok("/a/b/c/d/TARGETS.v2")]),
                "/a/b/c/d/**/thrift_lib.rs" => Ok(vec![Ok("/a/b/c/d/thrift_lib.rs")]),
                "/a/b/c/d/**/thrift_build.rs" => Ok(vec![Ok("/a/b/c/d/thrift_build.rs")]),
            })),
            &FbcodeRoot::new_mock("/a"),
            &configs,
        )
        .await
        .unwrap();

        let expected = [
            (
                "proj1",
                vec_cargo(&["b/c/d/e/Cargo.toml", "b/c/d/g/Cargo.toml"]),
                vec_targets(&["b/c/d/TARGETS"]),
                vec_additional(&["b/c/d/thrift_build.rs", "b/c/d/thrift_lib.rs"]),
            ),
            (
                "proj2",
                vec_cargo(&["b/c/Cargo.toml"]),
                vec_targets(&[]),
                vec_additional(&[]),
            ),
        ];

        assert_eq!(
            pfs.iter()
                .map(|pf| (
                    pf.conf().name().as_str(),
                    pf.cargo(),
                    pf.targets(),
                    pf.additional(),
                ))
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|(n, c, t, a)| (*n, c, t, a))
                .collect::<Vec<_>>(),
        );
    }

    #[tokio::test]
    async fn get_files_for_project_test() {
        if cfg!(windows) {
            return; // Broken on Windows
        }

        let pc = |inc: &[&str], exc: &[&str]| -> ProjectConf {
            from_value(json!({
                "name": "proj",
                "include_globs": inc,
                "exclude_globs": exc,
                "oncall": "oncall_name",
            }))
            .unwrap()
        };

        let sorted_files = |(mut cargo, mut targets, mut additional): (
            Vec<CargoTomlPath>,
            Vec<TargetsPath>,
            Vec<PathInFbcode>,
        )| {
            cargo.sort();
            targets.sort();
            additional.sort();
            (cargo, targets, additional)
        };

        let glob_values = hashmap! {
            "/a/b/c/d/**/Cargo.toml" => Ok(vec![Ok("/a/b/c/d/f/Cargo.toml")]),
            "/a/b/c/d/**/BUCK" => Ok(vec![Ok("/a/b/c/d/BUCK")]),
            "/a/b/c/d/**/TARGETS" => Ok(vec![Ok("/a/b/c/d/TARGETS")]),
            "/a/b/c/d/**/BUCK.v2" => Ok(vec![Ok("/a/b/c/d/BUCK.v2")]),
            "/a/b/c/d/**/TARGETS.v2" => Ok(vec![Ok("/a/b/c/d/TARGETS.v2")]),
            "/a/b/c/d/**/thrift_lib.rs" => Ok(vec![Ok("/a/b/c/d/thrift_lib.rs")]),
            "/a/b/c/d/**/thrift_build.rs" => Ok(vec![Ok("/a/b/c/d/f/thrift_build.rs")]),
        };
        let fbcode_root = FbcodeRoot::new_mock("/a/b");

        assert_eq!(
            sorted_files(
                get_files_for_project(
                    Arc::new(glob_mock(glob_values.clone())),
                    &fbcode_root,
                    &pc(&["c/d/**"], &[])
                )
                .await
                .unwrap()
            ),
            (
                vec_cargo(&["c/d/f/Cargo.toml"]),
                vec_targets(&["c/d/TARGETS"]),
                vec_additional(&["c/d/f/thrift_build.rs", "c/d/thrift_lib.rs"]),
            )
        );

        assert_eq!(
            sorted_files(
                get_files_for_project(
                    Arc::new(glob_mock(glob_values.clone())),
                    &fbcode_root,
                    &pc(&["c/d/**"], &["c/d/f/**"])
                )
                .await
                .unwrap()
            ),
            (
                vec_cargo(&[]),
                vec_targets(&["c/d/TARGETS"]),
                vec_additional(&["c/d/thrift_lib.rs"])
            )
        );

        assert_eq!(
            sorted_files(
                get_files_for_project(
                    Arc::new(glob_mock(glob_values)),
                    &fbcode_root,
                    &pc(&["c/d/**"], &["c/d/**"])
                )
                .await
                .unwrap()
            ),
            (vec_cargo(&[]), vec_targets(&[]), vec_additional(&[]))
        );

        assert_matches!(
            get_files_for_project(
                Arc::new(glob_mock(hashmap! {
                    "/a/b/c/d/**/Cargo.toml" => Err("Cargo glob error"),
                    "/a/b/c/d/**/BUCK" => Ok(vec![]),
                    "/a/b/c/d/**/TARGETS" => Ok(vec![Ok("/a/b/c/d/TARGETS")]),
                    "/a/b/c/d/**/BUCK.v2" => Ok(vec![]),
                    "/a/b/c/d/**/TARGETS.v2" => Ok(vec![]),
                    "/a/b/c/d/**/thrift_lib.rs" => Ok(vec![]),
                    "/a/b/c/d/**/thrift_build.rs" => Ok(vec![]),
                })),
                &fbcode_root,
                &pc(&["c/d/**"], &["c/d/**"])
            ).await,
            Err(err) => {
                assert_eq!(
                    format!("{err:?}"),
                    "While glob-searching files for project \
                    proj\n\nCaused by:\n    0: \
                    While glob-searching Cargo.toml files for project \
                    proj\n    1: Cargo glob error",
                )
            }
        );
    }

    #[derive(Clone, Debug)]
    struct TestGetFilesHelper {
        test_run: u64,
        glob_expected_input: &'static str,
        glob_mocked_return: GlobRet,
        fbcode_root: &'static str,
        proj_name: &'static str,
        include_pat: &'static str,
        file_name: &'static str,
        exclude_globs: &'static [&'static str],
    }

    impl TestGetFilesHelper {
        async fn run(&mut self) -> Result<Vec<String>> {
            let Self {
                glob_expected_input,
                glob_mocked_return,
                fbcode_root,
                proj_name,
                include_pat,
                file_name,
                exclude_globs,
                ..
            } = self.clone();

            // Bumping test_run which is useful for distinguishing between test runs
            self.test_run += 1;

            get_files_helper(
                Arc::new(glob_mock(
                    hashmap! { glob_expected_input => glob_mocked_return },
                )),
                FbcodeRoot::new_mock(fbcode_root),
                proj_name,
                Pattern::new(include_pat).unwrap(),
                file_name,
                |p_in_fb| Ok(p_in_fb.as_ref().to_str().unwrap().to_owned()),
                Arc::new(
                    exclude_globs
                        .iter()
                        .map(|p| Pattern::new(p).unwrap())
                        .collect(),
                ),
            )
            .await
        }
    }

    #[tokio::test]
    async fn get_files_helper_test() {
        if cfg!(windows) {
            return; // Broken on Windows
        }

        let mut test = TestGetFilesHelper {
            test_run: 0, // Setting to 0, so the first run will report as 1
            glob_expected_input: "/a/b/c/**/file.test",
            glob_mocked_return: Ok(vec![Ok("/a/b/c/file.test"), Ok("/a/b/c/d/file.test")]),
            fbcode_root: "/a/b",
            proj_name: "proj_name",
            include_pat: "c/**",
            file_name: "file.test",
            exclude_globs: &[],
        };

        let ss = |ss: &[&str]| -> Vec<_> { ss.iter().map(|s| String::from(*s)).collect() };

        assert_eq!(
            test.run().await.unwrap(),
            ss(&["c/file.test", "c/d/file.test"]),
            "While running test: {:#?}",
            test
        );

        test.exclude_globs = &["c/d/**"];
        assert_eq!(
            test.run().await.unwrap(),
            ss(&["c/file.test"]),
            "While running test: {:#?}",
            test
        );

        test.exclude_globs = &["*"];
        assert_eq!(
            test.run().await.unwrap(),
            ss(&[]),
            "While running test: {:#?}",
            test
        );

        let check_err = |mut test: TestGetFilesHelper, err_msg| async move {
            assert_matches!(
                test.run().await,
                Err(err) => {
                    assert_eq!(
                        format!("{err:?}"),
                        err_msg,
                        "While running test: {:#?}",
                        test
                    );
                },
                "While running test: {:#?}",
                test
            );
            test
        };

        test.glob_mocked_return = Ok(vec![Ok("/a/b/c/file.test"), Err("Glob iter error")]);
        test.exclude_globs = &[];
        let mut test = check_err(
            test,
            "While glob-searching file.test files for project \
            proj_name\n\nCaused by:\n    Glob iter error",
        )
        .await;

        test.glob_mocked_return = Err("Glob error");
        test.glob_expected_input = "/a/b/c/**/file2.rs";
        test.file_name = "file2.rs";
        test.proj_name = "proj_name2";
        check_err(
            test,
            "While glob-searching file2.rs files for project \
            proj_name2\n\nCaused by:\n    Glob error",
        )
        .await;
    }
}
