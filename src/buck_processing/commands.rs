// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use std::process::Output;
use std::process::Stdio;

use anyhow::Context as _;
use anyhow::Result;
use futures::future;
use tokio::io::AsyncWriteExt as _;
use tokio::io::BufWriter;
use tokio::process::Command;

use super::rules::BuckManifestRule;
use super::rules::FbcodeBuckRule;
use super::rules::ThriftCratemapRule;
use crate::paths::FbcodeRoot;
use crate::paths::TargetsPath;

const BUCK_CMD: &str = "buck2";

const BUCK_ATTRIBUTION_ARGS: &[&str] = &["--oncall=autocargo", "--client-metadata=id=autocargo"];

// For autocargo purposes, the mode file used doesn't matter.
// The manifest rules are unaffected by the configs in standard
// mode files. However, because we might be starting a new buckd,
// we want to use standard mode files so whatever runs after us
// is less likely to need a new buckd.
const BUCK_MODE_ARGS: &[&str] = if cfg!(target_os = "macos") {
    &["@fbcode//mode/mac-arm64"]
} else if cfg!(target_os = "windows") {
    &["@fbcode//mode/win"]
} else {
    &[]
};

const BUCK_ISOLATION_ARGS: &[&str] = &["--isolation-dir=autocargo"];

/// Command for running buck build of *-rust-manifest files.
pub async fn buck_build_manifests_cmd<'a>(
    fbcode_root: &FbcodeRoot,
    use_isolation_dir: bool,
    rules: impl IntoIterator<Item = &'a BuckManifestRule>,
) -> Result<(Command, Output)> {
    buck_build_cmd(
        fbcode_root,
        use_isolation_dir,
        rules.into_iter().map(|rule| rule.as_ref().clone()),
    )
    .await
}

/// Command for running buck build of *-rust-dep-map files.
pub async fn buck_build_cratemaps_cmd<'a>(
    fbcode_root: &FbcodeRoot,
    use_isolation_dir: bool,
    rules: impl IntoIterator<Item = &'a ThriftCratemapRule>,
) -> Result<(Command, Output)> {
    buck_build_cmd(
        fbcode_root,
        use_isolation_dir,
        rules.into_iter().map(|rule| rule.fbcode_buck_rule()),
    )
    .await
}

// [Note: Why do we pass `--isolation-dir=autocargo` here?]
// --------------------------------------------------------
// Running a target like fbcode//hphp/hack/scripts/facebook:test_hh_cargo will
// mean the buck commands run in this program are recursive invocations. In such
// a situation, the `--isolation-dir` flag ensures the invocation is isolated
// from the parent inocation. Without this flag, this recursive invocation
// scenario will soon become a hard error. See
// https://fb.workplace.com/groups/buck2eng/permalink/3044383762525770/ for
// details.

async fn buck_build_cmd(
    fbcode_root: &FbcodeRoot,
    use_isolation_dir: bool,
    rules: impl IntoIterator<Item = FbcodeBuckRule>,
) -> Result<(Command, Output)> {
    let mut command = Command::new(BUCK_CMD);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::inherit());
    command.current_dir(fbcode_root);
    if use_isolation_dir {
        // See [Note: Why do we pass `--isolation-dir=autocargo` here?]
        command.args(BUCK_ISOLATION_ARGS);
    }
    command.arg("build");
    command.args(BUCK_ATTRIBUTION_ARGS);
    command.args(BUCK_MODE_ARGS);
    command.args(["--show-full-json-output", "@-"]);

    let mut child = command
        .spawn()
        .with_context(|| format!("Spawning command: {:?}", command.as_std()))?;

    let mut stdin = BufWriter::new(child.stdin.take().unwrap());

    let (_, output) = future::try_join(
        async move {
            for rule in rules {
                let line = format!("{rule}\n");
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

/// Command for running buck query in search of *-rust-manifest files.
pub async fn buck_query_manifests_cmd<'a>(
    fbcode_root: &FbcodeRoot,
    use_isolation_dir: bool,
    targets_paths: impl IntoIterator<Item = &'a TargetsPath>,
) -> Result<(Command, Output)> {
    let mut command = Command::new(BUCK_CMD);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::inherit());
    command.current_dir(fbcode_root);
    if use_isolation_dir {
        // See [Note: Why do we pass `--isolation-dir=autocargo` here?]
        command.args(BUCK_ISOLATION_ARGS);
    }
    command.arg("uquery");
    command.args(BUCK_ATTRIBUTION_ARGS);
    command.args(BUCK_MODE_ARGS);
    command.args([
        "--output-format=json",
        "attrfilter('labels', 'rust_manifest', kind('^(genrule|write_file)$', %Ss))",
        "@-",
    ]);

    let mut child = command
        .spawn()
        .with_context(|| format!("Spawning command: {:?}", command.as_std()))?;

    let mut stdin = BufWriter::new(child.stdin.take().unwrap());

    let (_, output) = future::try_join(
        async move {
            for path in targets_paths {
                let line = format!("fbcode//{}:\n", path.as_dir().as_ref().display());
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
