// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use autocargo::config::AllProjects;
use autocargo::config::ProjectConf;
use autocargo::paths::FbcodeRoot;
use autocargo::paths::FbsourceRoot;
use autocargo::paths::PathInFbcode;
use autocargo::paths::process_input_paths;
use clap::Parser;

const DEFAULT_CONF: &str = "fbcode/common/rust/cargo_from_buck/project_configs";

const DEFAULT_UTD_MAP: &str = "tools/utd/migrated_nbtd_jobs/autocargo_verification.json";

#[derive(Parser, Debug)]
#[command(about = "Generates Cargo.toml files out of Buck build rules")]
pub struct AutocargoArgs {
    /// Use a custom config dir
    #[clap(long, short)]
    config: Option<PathBuf>,

    /// Use a custom UTD map file
    #[clap(long)]
    utd_map: Option<PathBuf>,

    /// Run buck commands in an isolation dir
    #[clap(long, short, alias = "use_isolation_dir")]
    pub use_isolation_dir: bool,

    /// Project name to regenerate, including dependencies
    #[clap(long = "project", short, value_name = "PROJECT")]
    pub projects: Vec<String>,

    /// Paths to be checked
    // These paths are paths in the repo, so must be valid UTF-8.
    pub paths: Vec<String>,
}

impl AutocargoArgs {
    pub async fn project_confs(&self, fbsource_root: &FbsourceRoot) -> Result<AllProjects> {
        let conf_path = self
            .config
            .clone()
            .unwrap_or_else(|| Path::join(fbsource_root.as_ref(), DEFAULT_CONF));
        ProjectConf::from_dir(conf_path).await
    }

    pub async fn process_input_paths(&self, fbcode_root: &FbcodeRoot) -> Result<Vec<PathInFbcode>> {
        process_input_paths(self.paths.iter().map(String::as_str), fbcode_root).await
    }

    pub fn utd_map(&self, fbsource_root: &FbsourceRoot) -> PathBuf {
        self.utd_map
            .clone()
            .unwrap_or_else(|| Path::join(fbsource_root.as_ref(), DEFAULT_UTD_MAP))
    }
}
