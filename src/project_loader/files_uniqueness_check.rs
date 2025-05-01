// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use std::collections::BTreeMap;
use std::fmt::Debug;

use anyhow::Result;
use anyhow::bail;

use super::ProjectFiles;
use crate::config::ProjectConf;
use crate::paths::CargoTomlPath;
use crate::paths::PathInFbcode;
use crate::paths::TargetsPath;

/// Verifies that the files covered by projects are unique, so that none of them
/// are covered by two projects. This function also returns condensed list of
/// all files for further processing.
pub(super) fn files_uniqueness_check<'a>(
    project_files_list: &'a [ProjectFiles<'_>],
) -> Result<(
    impl Iterator<Item = &'a CargoTomlPath>,
    impl Iterator<Item = &'a TargetsPath>,
    impl Iterator<Item = &'a PathInFbcode>,
)> {
    let all_cargo = check_uniqueness(
        project_files_list
            .iter()
            .map(|project_files| (*project_files.conf(), project_files.cargo().as_slice())),
    )?;
    let all_targets = check_uniqueness(
        project_files_list
            .iter()
            .map(|project_files| (*project_files.conf(), project_files.targets().as_slice())),
    )?;
    let all_additional = check_uniqueness(
        project_files_list
            .iter()
            .map(|project_files| (*project_files.conf(), project_files.additional().as_slice())),
    )?;

    Ok((
        all_cargo.into_keys(),
        all_targets.into_keys(),
        all_additional.into_keys(),
    ))
}

fn check_uniqueness<'a, T: Eq + Ord + Debug>(
    files: impl IntoIterator<Item = (&'a ProjectConf, &'a [T])>,
) -> Result<BTreeMap<&'a T, &'a str>> {
    let mut all_files = BTreeMap::new();
    for (conf, fs) in files {
        for f in fs {
            if let Some(old_name) = all_files.insert(f, conf.name().as_str()) {
                bail!(
                    "File {:?} is covered by both {} and {} projects",
                    f,
                    old_name,
                    conf.name()
                );
            }
        }
    }
    Ok(all_files)
}

#[cfg(test)]
mod test {
    use assert_matches::assert_matches;
    use maplit::btreemap;
    use serde_json::from_value;
    use serde_json::json;

    use super::*;
    use crate::paths::PathInFbcode;

    fn pc(name: &str) -> ProjectConf {
        from_value(json!({
            "name": name,
            "include_globs": [],
            "oncall": "oncall_name",
        }))
        .unwrap()
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

    // useful for type erasure from array -> slice
    fn s<'a>(s: &'a [&'a str]) -> &'a [&'a str] {
        s
    }

    #[test]
    fn files_uniqueness_check_test() {
        let proj1 = pc("proj1");
        let proj2 = pc("proj2");

        let pfs = &mut [
            ProjectFiles::new(
                &proj1,
                vec_cargo(&["a/Cargo.toml", "b/Cargo.toml"]),
                vec_targets(&["a/TARGETS"]),
                vec_additional(&["a/thrift_lib.rs", "b/thrift_build.rs"]),
            ),
            ProjectFiles::new(
                &proj2,
                vec_cargo(&["c/Cargo.toml"]),
                vec_targets(&["c/TARGETS"]),
                vec_additional(&[]),
            ),
        ];
        let (cargo, targets, additional) = files_uniqueness_check(pfs).unwrap();

        assert_eq!(
            cargo.cloned().collect::<Vec<_>>(),
            vec_cargo(&["a/Cargo.toml", "b/Cargo.toml", "c/Cargo.toml"])
        );
        assert_eq!(
            targets.cloned().collect::<Vec<_>>(),
            vec_targets(&["a/TARGETS", "c/TARGETS"])
        );
        assert_eq!(
            additional.cloned().collect::<Vec<_>>(),
            vec_additional(&["a/thrift_lib.rs", "b/thrift_build.rs"])
        );

        *pfs[1].targets_mut() = vec_targets(&["a/TARGETS"]);
        let err = match files_uniqueness_check(pfs) {
            Ok(_) => panic!("Unexpected Ok result, it should fail"),
            Err(err) => err,
        };

        assert_eq!(
            format!("{err:?}"),
            "File TargetsPath { \
                dir: PathInFbcode(\"a\") \
            } is covered by both proj1 and proj2 projects"
        );
    }

    #[test]
    fn check_uniqueness_test() {
        assert_eq!(
            check_uniqueness(vec![
                (&pc("proj1"), s(&["file3", "file1"])),
                (&pc("proj2"), s(&["file2"])),
            ])
            .unwrap(),
            btreemap! {
                &"file1" => "proj1",
                &"file2" => "proj2",
                &"file3" => "proj1",
            }
        );

        assert_matches!(
            check_uniqueness(
                vec![
                    (&pc("proj1"), s(&["file2", "file1"])),
                    (&pc("proj2"), s(&["file2"])),
                ]
            ),
            Err(err) => {
                assert_eq!(
                    format!("{err:?}"),
                    "File \"file2\" is covered by both proj1 and proj2 projects"
                );
            }
        )
    }
}
