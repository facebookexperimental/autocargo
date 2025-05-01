/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::Output;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use anyhow::Result;
use futures::future;
use itertools::Itertools;
use tokio::fs::metadata;
use tokio::io::AsyncWriteExt as _;
use tokio::io::BufWriter;
use tokio::process::Command;

use super::ProjectLoader;
use crate::config::ProjectConf;
use crate::paths::CargoTomlPath;
use crate::paths::FbcodeRoot;
use crate::paths::FbsourceRoot;
use crate::paths::PathInFbcode;
use crate::paths::RUST_VENDOR_STR;
use crate::paths::TargetsPath;
use crate::util::command_runner::run_command;

impl<'proj, 'a> ProjectLoader<'proj, 'a> {
    /// Given all the include_globs from all selected projects calls
    /// 'eden prefetch' on them. If the command takes too much time this fact
    /// will be logged.
    /// 'eden prefetch' accepts patterns relative to root of fbsource.
    pub(super) async fn eden_prefetch(&self) -> Result<()> {
        let &Self {
            logger,
            fbsource_root,
            configs,
            ..
        } = self;

        if metadata(Path::join(fbsource_root.as_ref(), ".eden"))
            .await
            .is_err()
        {
            // If we fail to get metadata of fbsource/.eden then either
            // something is really bad with fbsource or this is not an eden
            // checkout, in both cases it is pointless to run eden prefetch, so
            // just return Ok here as prefetching is just an optimisation.
            return Ok(());
        }

        run_command(
            logger,
            "eden prefetch",
            Duration::from_secs(5),
            eden_prefetch_cmd(fbsource_root, configs.projects()),
        )
        .await?;

        Ok(())
    }
}

async fn eden_prefetch_cmd(
    fbsource_root: &FbsourceRoot,
    configs: &[&ProjectConf],
) -> Result<(Command, Output)> {
    let mut command = Command::new("eden");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .current_dir(fbsource_root)
        .arg("prefetch")
        .arg("--pattern-file=-");

    let mut child = command
        .spawn()
        .with_context(|| format!("Spawning command: {:?}", command.as_std()))?;

    let mut stdin = BufWriter::new(child.stdin.take().unwrap());

    let (_, output) = future::try_join(
        async move {
            let patterns = eden_prefetch_args(configs);
            for pat in patterns {
                let line = format!("{}\n", pat.display());
                stdin.write_all(line.as_bytes()).await?;
            }
            stdin.flush().await
        },
        child.wait_with_output(),
    )
    .await
    .with_context(|| format!("Executing command: {:?}", command.as_std()))?;

    Ok((command, output))
}

/// Given include globs of projects produce list of globs relative to root
/// of fbsource to be passed to 'eden prefetch'
fn eden_prefetch_args<'a>(
    configs: &[&'a ProjectConf],
) -> impl Iterator<Item = PathBuf> + 'a + use<'a> {
    let fbcode_root = Path::new(FbcodeRoot::dirname());
    let patterns: BTreeSet<PathBuf> = configs
        .iter()
        .flat_map(|conf| {
            conf.include_globs()
                .iter()
                .map(|pat| fbcode_root.join(pat.as_str()))
                .chain(
                    conf.roots()
                        .iter()
                        .map(|root| fbcode_root.join(root).join("**")),
                )
        })
        .collect();

    // If any configs want to generate a Cargo.lock then make sure to prefetch
    // third-party/rust/vendor. Since the cargo_lock path is required to live in
    // include_globs its Cargo.toml will already be fetched.
    let vendor_paths = if configs.iter().any(|cfg| !cfg.cargo_locks().is_empty()) {
        Some(Path::new(RUST_VENDOR_STR).join("**"))
    } else {
        None
    };

    itertools::chain(
        patterns.into_iter().flat_map(|pat| {
            PathInFbcode::all_additional_filenames()
                .iter()
                .map(|filename| pat.join(filename))
                .chain(Some(pat.join(CargoTomlPath::filename())))
                .chain(TargetsPath::filenames().iter().map(|name| pat.join(name)))
                .collect_vec()
        }),
        vendor_paths,
    )
}

#[cfg(test)]
mod test {
    use serde_json::from_value;
    use serde_json::json;

    use super::*;

    fn pc(roots: &[&str], inc: &[&str]) -> ProjectConf {
        from_value(json!({
            "name": "proj",
            "roots": roots,
            "include_globs": inc,
            "oncall": "oncall_name",
        }))
        .unwrap()
    }

    fn pc_with_lock(inc: &[&str], lock: &str) -> ProjectConf {
        from_value(json!({
            "name": "proj",
            "include_globs": inc,
            "oncall": "oncall_name",
            "cargo_locks": [lock],
        }))
        .unwrap()
    }

    #[test]
    fn eden_prefetch_args_test() {
        let vec_p = |ps: &[&str]| {
            ps.iter()
                .map(|s| Path::new(s).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            eden_prefetch_args(&[
                &pc(&[], &["a/b/**", "c"]),
                &pc(&[], &["d/**/e"]),
                &pc(&["f"], &[])
            ])
            .collect::<Vec<_>>(),
            vec_p(&[
                "fbcode/a/b/**/thrift_build.rs",
                "fbcode/a/b/**/thrift_lib.rs",
                "fbcode/a/b/**/Cargo.toml",
                "fbcode/a/b/**/TARGETS",
                "fbcode/a/b/**/BUCK",
                "fbcode/a/b/**/TARGETS.v2",
                "fbcode/a/b/**/BUCK.v2",
                "fbcode/c/thrift_build.rs",
                "fbcode/c/thrift_lib.rs",
                "fbcode/c/Cargo.toml",
                "fbcode/c/TARGETS",
                "fbcode/c/BUCK",
                "fbcode/c/TARGETS.v2",
                "fbcode/c/BUCK.v2",
                "fbcode/d/**/e/thrift_build.rs",
                "fbcode/d/**/e/thrift_lib.rs",
                "fbcode/d/**/e/Cargo.toml",
                "fbcode/d/**/e/TARGETS",
                "fbcode/d/**/e/BUCK",
                "fbcode/d/**/e/TARGETS.v2",
                "fbcode/d/**/e/BUCK.v2",
                "fbcode/f/**/thrift_build.rs",
                "fbcode/f/**/thrift_lib.rs",
                "fbcode/f/**/Cargo.toml",
                "fbcode/f/**/TARGETS",
                "fbcode/f/**/BUCK",
                "fbcode/f/**/TARGETS.v2",
                "fbcode/f/**/BUCK.v2",
            ])
        );
        assert_eq!(
            eden_prefetch_args(&[&pc_with_lock(&["a/**/b"], "a/some/random/path")])
                .collect::<Vec<_>>(),
            vec_p(&[
                "fbcode/a/**/b/thrift_build.rs",
                "fbcode/a/**/b/thrift_lib.rs",
                "fbcode/a/**/b/Cargo.toml",
                "fbcode/a/**/b/TARGETS",
                "fbcode/a/**/b/BUCK",
                "fbcode/a/**/b/TARGETS.v2",
                "fbcode/a/**/b/BUCK.v2",
                "third-party/rust/vendor/**",
            ])
        );
    }
}
