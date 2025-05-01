// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

//! This binary is the main autocargo command that is responsible for processing
//! user input, parsing Buck files, and generating Cargo.toml files.

mod args;
mod generate_cargo_locks;
mod generate_utd_map;
mod handle_generation_results;
mod logger;

use anyhow::Result;
use autocargo::buck_processing::ProcessOutput;
use autocargo::buck_processing::process_targets;
use autocargo::cargo_generator::CargoGenerator;
use autocargo::paths::FbcodeRoot;
use autocargo::paths::FbsourceRoot;
use autocargo::project_loader::ProjectLoader;
use clap::Parser;
use slog::info;

use crate::args::AutocargoArgs;
use crate::handle_generation_results::handle_generation_results;
use crate::logger::logger;

#[tokio::main]
async fn main() -> Result<()> {
    let args = AutocargoArgs::parse();
    let logger = logger();

    info!(logger, "Using isolation dir: {:?}", args.use_isolation_dir);

    let fbsource_root = FbsourceRoot::new().await?;
    let fbcode_root = FbcodeRoot::from(fbsource_root.clone());
    info!(logger, "{:?}", fbsource_root);

    let all_configs = args.project_confs(&fbsource_root).await?;
    let paths = args.process_input_paths(&fbcode_root).await?;
    let paths_provided = !paths.is_empty();
    let selected_configs = if paths_provided || !args.projects.is_empty() {
        info!(
            logger,
            "Processing projects selected based on input paths and project names provided"
        );
        all_configs.select_based_on_paths_and_names(&paths, &args.projects)?
    } else {
        info!(
            logger,
            "Processing all projects since no input paths or project names were provided"
        );
        all_configs.select_all()
    };

    let utd_map_path = args.utd_map(&fbsource_root);

    let (project_files, projectless_files) = ProjectLoader {
        logger: &logger,
        fbsource_root: &fbsource_root,
        fbcode_root: &fbcode_root,
        configs: &selected_configs,
        input_paths: paths,
    }
    .load()
    .await?;

    {
        let max_proj_name_len = selected_configs
            .projects()
            .iter()
            .map(|conf| conf.name().len())
            .chain(Some(12)) // The minimal will be 12
            .max()
            .unwrap_or(0);
        for proj in &project_files {
            info!(
                logger,
                "{:>4$} found Cargo.toml {:>3} TARGETS {:>3} additional {:>3}",
                proj.conf().name(),
                proj.cargo().len(),
                proj.targets().len(),
                proj.additional().len(),
                max_proj_name_len,
            );
        }
        if paths_provided {
            info!(
                logger,
                "Files from input for which no project was found - \
                Cargo.toml {:>3} TARGETS {:>3} additional {:>3}",
                projectless_files.cargo().len(),
                projectless_files.targets().len(),
                projectless_files.additional().len()
            );
        }
    }

    let ProcessOutput {
        processed_manifests,
        unprocessed_paths,
    } = process_targets(
        &logger,
        &fbcode_root,
        args.use_isolation_dir,
        project_files.iter().flat_map(|p| p.targets().iter()),
    )
    .await?;

    info!(
        logger,
        "Processed {} manifests; {} paths were out of scope of selected \
        projects",
        processed_manifests.values().flatten().count(),
        unprocessed_paths.len(),
    );

    let generator = CargoGenerator::new(
        &logger,
        &fbsource_root,
        &all_configs,
        &project_files,
        &unprocessed_paths,
    )
    .await?;

    let generated =
        generator.generate_for_projects(&logger, &selected_configs, &processed_manifests)?;

    handle_generation_results(
        &logger,
        &fbcode_root,
        &generated,
        &project_files,
        &projectless_files,
    )
    .await?;

    generate_cargo_locks::generate_cargo_locks(&logger, &fbsource_root, &selected_configs).await?;

    generate_utd_map::generate_utd_map(&logger, &all_configs, &utd_map_path).await?;

    Ok(())
}
