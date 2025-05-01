// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use autocargo::config::SelectedProjects;
use autocargo::future_soft_timeout;
use autocargo::paths::FbcodeRoot;
use autocargo::paths::FbsourceRoot;
use autocargo::paths::PathInFbcode;
use autocargo::paths::RUST_VENDOR_STR;
use cargo::core::shell::Shell;
use cargo::util::ConfigValue;
use cargo::util::GlobalContext as Config;
use cargo::util::context::Definition;
use futures::TryStreamExt;
use futures::future;
use futures::stream::FuturesOrdered;
use maplit::hashmap;
use slog::Logger;
use slog::info;
use slog::warn;
use tokio::task::spawn_blocking;

/// Generate a Cargo.lock for each directory specified in the ProjectConf's
/// cargo_locks field.
pub(crate) async fn generate_cargo_locks(
    logger: &Logger,
    fbsource: &FbsourceRoot,
    selected_projects: &SelectedProjects<'_>,
) -> Result<()> {
    let homedir = cargo::util::context::homedir(fbsource.as_ref()).context(
        "Couldn't find your home directory. This probably means that $HOME was not set.",
    )?;

    selected_projects
        .projects()
        .iter()
        .flat_map(|x| x.cargo_locks())
        .map(future::ok)
        .collect::<FuturesOrdered<_>>()
        // Run serially because cargo holds a lock on the package cache,
        // and warns on concurrent access.
        .try_for_each(|path| {
            info!(
                logger,
                "Running 'generate_cargo_lock' for '{}'",
                path.as_ref().display(),
            );
            let homedir = homedir.clone();
            async move {
                future_soft_timeout(
                    spawn_blocking({
                        let path = path.clone();
                        let fbsource = fbsource.clone();
                        move || generate_cargo_lock(&fbsource, &homedir, &path)
                    }),
                    Duration::from_secs(10),
                    |duration| {
                        warn!(
                            logger,
                            "'generate_cargo_lock' for '{}' running for more than {:.1?}",
                            path.as_ref().display(),
                            duration
                        )
                    },
                    |duration| {
                        warn!(
                            logger,
                            "'generate_cargo_lock' for '{}' finished after {:.1?}",
                            path.as_ref().display(),
                            duration
                        )
                    },
                )
                .await
                .context("Failed spawn_blocking")?
                .with_context(|| {
                    format!(
                        "While running 'generate_cargo_lock' for '{}'",
                        path.as_ref().display(),
                    )
                })
            }
        })
        .await
}

/// Do `cargo generate-lockfile` on the given path. This uses the internal cargo
/// crate to do the work rather than calling out to an external cargo binary.
///
/// We don't require .cargo/config.toml to be set up in the target directory -
/// instead we force a virtual config to point directly at
/// third-party/rust/vendor. Note that this could eventually become a problem if
/// a project requires some custom values (such as needing to override some
/// other fbcode project) since cargo doesn't provide a way to "merge" configs
/// or set individual values.
fn generate_cargo_lock(fbsource: &FbsourceRoot, homedir: &Path, path: &PathInFbcode) -> Result<()> {
    let fbsource: &Path = fbsource.as_ref();
    let target_dir = fbsource.join(FbcodeRoot::dirname()).join(path.as_ref());
    let path = target_dir.join("Cargo.toml");
    let shell = Shell::new();
    let mut cfg = Config::new(shell, target_dir.clone(), homedir.to_path_buf());
    let rustc = fbsource.join("xplat/rust/toolchain/current/basic/bin/rustc");

    // Set up the config to point at third-party/rust/vendor
    let tpr = fbsource.join(RUST_VENDOR_STR);
    const DEFN: Definition = Definition::Cli(None);
    cfg.set_values(hashmap! {
        "build".to_owned() => ConfigValue::Table(hashmap!{
            "rustc".to_owned() => ConfigValue::String(rustc.to_str().unwrap().to_owned(), DEFN),
        }, DEFN),
        "net".to_owned() => ConfigValue::Table(hashmap!{
            "offline".to_owned() => ConfigValue::Boolean(true, DEFN),
        }, DEFN),
        "source".to_owned() => ConfigValue::Table(hashmap!{
            "crates-io".to_owned() => ConfigValue::Table(hashmap!{
                "replace-with".to_owned() => ConfigValue::String("vendored-sources".to_owned(), DEFN),
            }, DEFN),
            "vendored-sources".to_owned() => ConfigValue::Table(hashmap!{
                "directory".to_owned() => ConfigValue::String(
                    tpr.to_str().context("vendor path is not UTF-8")?.to_owned(), DEFN),
            }, DEFN),
        }, DEFN),
    })?;

    cfg.configure(
        /* verbose: */ 0,
        /* quiet: */ false,
        /* color: */ None,
        /* frozen: */ false,
        /* locked: */ false,
        /* offline: */ true,
        /* target_dir: */ &Some(target_dir),
        /* unstable_flags: */ &[],
        /* cli_config: */ &[],
    )?;

    let ws = cargo::core::Workspace::new(&path, &cfg)?;
    cargo::ops::generate_lockfile(&ws)?;

    Ok(())
}
