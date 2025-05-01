// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;

use anyhow::Result;
use autocargo::config::AllProjects;
use autocargo::config::ProjectConf;
use autocargo::paths::FbcodeRoot;
use glob::Pattern;
use serde::Serialize;
use serde::Serializer;
use serde::ser::Error;
use serde::ser::SerializeMap;
use serde::ser::SerializeSeq;
use slog::Logger;
use slog::info;

/// Generate the "UTD map" - a mapping of all project include and exclude
/// globs suitable for [`autocargo_verification.td`].
///
/// [`autocargo_verification.td`]:
///     https://www.internalfb.com/code/fbsource/tools/utd/migrated_nbtd_jobs/autocargo_verification.td
pub(crate) async fn generate_utd_map(
    logger: &Logger,
    all_configs: &AllProjects,
    utd_map_path: &Path,
) -> Result<()> {
    let w = Vec::new();
    let mut serializer = serde_json::Serializer::pretty(w);

    // UTD's `python.json_loads` only accepts lists,
    // so we return a one item list.
    let mut seq = serializer.serialize_seq(Some(1))?;
    seq.serialize_element(&UtdMap {
        prefix: FbcodeRoot::dirname(),
        all_configs,
    })?;
    SerializeSeq::end(seq)?;

    let mut w = serializer.into_inner();
    w.write_all(b"\n")?;
    w.flush()?;

    if !tokio::fs::read(utd_map_path)
        .await
        .is_ok_and(|data| data == w)
    {
        info!(logger, "Updating UTD map at '{}'", utd_map_path.display());
        tokio::fs::write(utd_map_path, w).await?;
    }

    Ok(())
}

struct UtdMap<'a> {
    prefix: &'a str,
    all_configs: &'a AllProjects,
}

impl Serialize for UtdMap<'_> {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut map = ser.serialize_map(None)?;

        map.serialize_entry(
            "__comment__",
            &[
                "\x40generated", // TODO: Add signature.
                "@codegen-command: arc autocargo",
                "See https://fburl.com/autocargo",
            ],
        )?;

        map.serialize_entry(
            "project_configs",
            &ProjectConfigs {
                prefix: self.prefix,
                all_configs: self.all_configs,
            },
        )?;

        map.end()
    }
}

struct ProjectConfigs<'a> {
    prefix: &'a str,
    all_configs: &'a AllProjects,
}

impl Serialize for ProjectConfigs<'_> {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let projects = self.all_configs.select_all();
        let projects = projects.projects();
        let mut seq = ser.serialize_seq(Some(projects.len()))?;
        for project in projects {
            seq.serialize_element(&ProjectEntry {
                prefix: self.prefix,
                project,
            })?;
        }
        seq.end()
    }
}

struct ProjectEntry<'a> {
    prefix: &'a str,
    project: &'a ProjectConf,
}

impl Serialize for ProjectEntry<'_> {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut map = ser.serialize_map(None)?;

        map.serialize_entry("name", self.project.name())?;

        let mut include_globs = self.project.include_globs().clone();
        include_globs.extend(
            self.project.root_patterns().map_err(|e| {
                S::Error::custom(format!("Failed to construct root patterns: {}", e))
            })?,
        );
        map.serialize_entry(
            "include_globs",
            &ProjectGlobs {
                prefix: self.prefix,
                patterns: &include_globs,
            },
        )?;

        map.serialize_entry(
            "exclude_globs",
            &ProjectGlobs {
                prefix: self.prefix,
                patterns: self.project.exclude_globs(),
            },
        )?;

        map.end()
    }
}

struct ProjectGlobs<'a> {
    prefix: &'a str,
    patterns: &'a HashSet<Pattern>,
}

impl Serialize for ProjectGlobs<'_> {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let prefix = self.prefix;
        let mut patterns = self.patterns.iter().collect::<Vec<_>>();
        patterns.sort_unstable();

        ser.collect_seq(
            patterns
                .iter()
                .map(|pattern| ProjectGlob { prefix, pattern }),
        )
    }
}

struct ProjectGlob<'a> {
    prefix: &'a str,
    pattern: &'a Pattern,
}

impl Serialize for ProjectGlob<'_> {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(&format_args!("{}/{}", self.prefix, self.pattern.as_str()))
    }
}
