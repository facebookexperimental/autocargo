/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::collections::HashSet;

use super::ProjectLoader;
use super::ProjectlessFiles;
use crate::paths::CargoTomlPath;
use crate::paths::PathInFbcode;
use crate::paths::TargetsPath;

#[allow(clippy::needless_lifetimes)]
impl<'proj, 'a> ProjectLoader<'proj, 'a> {
    /// Compute list of files that were given by user as an input, but are not
    /// covered by any of the projects.
    pub(super) fn projectless_files<'input>(
        self,
        all_cargo: impl IntoIterator<Item = &'input CargoTomlPath>,
        all_targets: impl IntoIterator<Item = &'input TargetsPath>,
        all_additional: impl IntoIterator<Item = &'input PathInFbcode>,
    ) -> ProjectlessFiles {
        projectless_files(self.input_paths, all_cargo, all_targets, all_additional)
    }
}

fn projectless_files<'input>(
    input_paths: Vec<PathInFbcode>,
    all_cargo: impl IntoIterator<Item = &'input CargoTomlPath>,
    all_targets: impl IntoIterator<Item = &'input TargetsPath>,
    all_additional: impl IntoIterator<Item = &'input PathInFbcode>,
) -> ProjectlessFiles {
    let mut input_cargo = HashSet::new();
    let mut input_targets = HashSet::new();
    let mut input_additional = HashSet::new();
    for path in input_paths {
        if path.as_ref().ends_with(CargoTomlPath::filename()) {
            input_cargo.insert(CargoTomlPath::new(path).unwrap());
        } else if TargetsPath::matches_path(path.as_ref()) {
            input_targets.insert(TargetsPath::new(path).unwrap());
        } else if PathInFbcode::all_additional_filenames()
            .iter()
            .any(|filename| path.as_ref().ends_with(filename))
        {
            input_additional.insert(path);
        }
    }

    for p in all_cargo {
        input_cargo.remove(p);
    }
    for p in all_targets {
        input_targets.remove(p);
    }
    for p in all_additional {
        input_additional.remove(p);
    }

    ProjectlessFiles::new(
        input_cargo.into_iter().collect(),
        input_targets.into_iter().collect(),
        input_additional.into_iter().collect(),
    )
}

#[cfg(test)]
mod test {
    use anyhow::Result;

    use super::*;

    fn vec_fb(ps: &[&str]) -> Vec<PathInFbcode> {
        ps.iter().map(PathInFbcode::new_mock).collect()
    }

    fn vec_cargo(ps: &[&str]) -> Vec<CargoTomlPath> {
        vec_fb(ps)
            .into_iter()
            .map(CargoTomlPath::new)
            .collect::<Result<Vec<_>>>()
            .unwrap()
    }

    fn vec_targets(ps: &[&str]) -> Vec<TargetsPath> {
        vec_fb(ps)
            .into_iter()
            .map(TargetsPath::new)
            .collect::<Result<Vec<_>>>()
            .unwrap()
    }

    #[test]
    fn projectless_files_test() {
        assert_eq!(
            projectless_files(
                vec_fb(&[
                    "e/Cargo.toml",
                    "e/TARGETS",
                    "a",
                    "b/c/Cargo.toml",
                    "b/d/Cargo.toml",
                    "b/TARGETS",
                    "a/BUCK",
                    "c/BUCK",
                    "f/thrift_lib.rs",
                    "f/thrift_build.rs",
                    "g/thrift_lib.rs",
                    "g/thrift_build.rs",
                ]),
                &vec_cargo(&["b/c/Cargo.toml", "g/Cargo.toml"]),
                &vec_targets(&["e/TARGETS", "c/BUCK"]),
                &vec_fb(&["g/thrift_build.rs", "g/thrift_lib.rs"])
            ),
            ProjectlessFiles::new(
                vec_cargo(&["b/d/Cargo.toml", "e/Cargo.toml"]),
                vec_targets(&["a/BUCK", "b/TARGETS"]),
                vec_fb(&["f/thrift_build.rs", "f/thrift_lib.rs"])
            )
        )
    }
}
