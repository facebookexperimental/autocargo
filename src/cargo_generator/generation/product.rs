// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::Result;
use anyhow::anyhow;
use itertools::Itertools;
use maplit::hashset;
use pathdiff::diff_paths;

use crate::buck_processing::AutocargoTargetConfig;
use crate::buck_processing::FbconfigRuleType;
use crate::buck_processing::RawBuckManifest;
use crate::cargo_manifest::Product;
use crate::paths::CargoTomlPath;
use crate::paths::TargetsPath;

static RUST_KEYWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    hashset! {
        "abstract", "alignof", "as",       "become",   "box",
        "break",    "const",   "continue", "crate",    "do",
        "else",     "enum",    "extern",   "false",    "final",
        "fn",       "for",     "if",       "impl",     "in",
        "let",      "loop",    "macro",    "match",    "mod",
        "move",     "mut",     "offsetof", "override", "priv",
        "proc",     "pub",     "pure",     "ref",      "return",
        "Self",     "self",    "sizeof",   "static",    "struct",
        "super",    "trait",   "true",     "type",     "typeof",
        "unsafe",   "unsized", "use",      "virtual",  "where",
        "while",    "yield",

        // Other problematic names
        "core",
        "std",
    }
});

/// Generate name for lib/bin/test product. If the name is a rust keyword adds a
/// "_" suffix. The name is not escaped, so that it can be used to generate
/// package name or when escaped (replace "-" with "_") can be used as a
/// target/product name.
pub fn generate_product_name(raw: &RawBuckManifest) -> String {
    if let Some(name) = raw.autocargo.cargo_target_config.name.clone() {
        name
    } else {
        let name = raw
            .rust_config
            .crate_
            .clone()
            .unwrap_or_else(|| raw.name.clone());
        if RUST_KEYWORDS.contains(name.as_str()) {
            name + "_"
        } else {
            name
        }
    }
}

pub fn generate_product(
    fbconfig_rule_type: FbconfigRuleType,
    raw: &RawBuckManifest,
    targets_path: &TargetsPath,
    cargo_toml_path: &CargoTomlPath,
) -> Result<Product> {
    let AutocargoTargetConfig {
        name: _,
        path,
        test,
        doctest,
        bench,
        doc,
        plugin,
        proc_macro,
        harness,
        edition,
        crate_type,
        required_features,
    } = &raw.autocargo.cargo_target_config;

    let name = generate_product_name(raw).replace('-', "_");

    Ok(Product {
        path: Some(
            path.clone()
                .map(Ok)
                .or_else(|| {
                    if raw.autocargo.thrift.is_some() {
                        // This is possible only for [lib]. We will put a generated
                        // thrift_lib.rs file next to the Cargo.toml file.
                        Some(Ok("thrift_lib.rs".to_owned()))
                    } else {
                        raw.rust_config
                            .crate_root
                            .as_ref()
                            .map(|p| relative_crate_root(p, targets_path, cargo_toml_path))
                    }
                })
                .unwrap_or_else(|| {
                    generate_crate_root(
                        fbconfig_rule_type,
                        raw,
                        &name,
                        targets_path,
                        cargo_toml_path,
                    )
                })?,
        ),
        name: Some(name),
        test: test.unwrap_or(
            if !raw.rust_config.unittests || raw.rust_config.proc_macro {
                Some(false)
            } else {
                None
            },
        ),
        doctest: doctest.unwrap_or(
            if !raw.rust_config.unittests || raw.rust_config.proc_macro {
                Some(false)
            } else {
                None
            },
        ),
        bench: *bench,
        doc: *doc,
        plugin: *plugin,
        proc_macro: proc_macro.unwrap_or(raw.rust_config.proc_macro),
        harness: *harness,
        edition: edition.unwrap_or(raw.rust_config.edition),
        crate_type: crate_type.clone(),
        required_features: required_features.clone(),
    })
}

/// Looks for the root path of crate following the logic from rust_common.bzl.
fn generate_crate_root(
    fbconfig_rule_type: FbconfigRuleType,
    raw: &RawBuckManifest,
    name: &str,
    targets_path: &TargetsPath,
    cargo_toml_path: &CargoTomlPath,
) -> Result<String> {
    let mut srcs: Vec<PathBuf> = vec![];

    // mapped_srcs can only be used for Cargo if destination .rs is already present for cargo usage
    if raw.sources.srcs.is_empty() && !raw.sources.mapped_srcs.is_empty() {
        for path in raw.sources.mapped_srcs.values() {
            let mut p = PathBuf::from("src");
            p.push(PathBuf::from(path));
            srcs.push(p);
        }
    } else {
        srcs.clone_from(&raw.sources.srcs);
    };

    // Skips test_srcs as they are mostly for buck to
    // include fixtures etc. to build rule. The original rule and the
    // corresponding test rule have the same crate_root
    let srcs: Vec<_> = {
        let srcs_per_depth = srcs
            .iter()
            .map(|path| (path.components().count(), path))
            .into_group_map();
        srcs_per_depth
            .into_iter()
            .sorted_by(|(k1, _), (k2, _)| k1.cmp(k2))
            .flat_map(|(_, v)| v.into_iter().sorted())
            .collect()
    };

    let candidate_crate_name = name.to_owned() + ".rs";
    let candidates = {
        let main = "main.rs";
        let lib = "lib.rs";
        match fbconfig_rule_type {
            FbconfigRuleType::RustBinary => vec![main, &candidate_crate_name],
            FbconfigRuleType::RustLibrary => vec![lib, &candidate_crate_name],
            FbconfigRuleType::RustUnittest => vec![main, lib, &candidate_crate_name],
        }
    };

    let crate_root = candidates
        .iter()
        .find_map(|candidate| srcs.iter().find(|path| path.ends_with(candidate)))
        .ok_or_else(|| {
            anyhow!(
                "Unable to find any of {:?} in {:?} while searching for crate root",
                candidates,
                srcs
            )
        })?;

    relative_crate_root(crate_root, targets_path, cargo_toml_path)
}

fn relative_crate_root(
    crate_root: impl AsRef<Path>,
    targets_path: &TargetsPath,
    cargo_toml_path: &CargoTomlPath,
) -> Result<String> {
    let crate_root = targets_path.as_dir().join_to_path_in_fbcode(crate_root);

    diff_paths(crate_root.as_ref(), cargo_toml_path.as_dir().as_ref())
        .and_then(|path| path.to_str().map(|s| s.to_owned()))
        .ok_or_else(|| {
            anyhow!(
                "Failed to make a relative path from {:?} to {:?} while searching for crate root",
                crate_root,
                cargo_toml_path.as_dir()
            )
        })
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::paths::PathInFbcode;

    #[test]
    fn generate_crate_root_test() {
        if cfg!(windows) {
            return; // Broken on Windows
        }

        let targets_path = TargetsPath::new(PathInFbcode::new_mock("foo/bar/TARGETS")).unwrap();
        let cargo_path = CargoTomlPath::new(PathInFbcode::new_mock("foo/bar/Cargo.toml")).unwrap();
        let r = |ps: &[&str]| {
            let mut raw = RawBuckManifest::empty_test();
            raw.sources.srcs = ps
                .iter()
                .map(|p| Path::new(p).to_owned())
                .collect::<Vec<_>>();
            raw
        };
        let r#gen =
            |ty, raw| generate_crate_root(ty, raw, "foo", &targets_path, &cargo_path).unwrap();

        let raw = r(&["foo/test.rs", "foo/bar/main.rs"]);
        assert_eq!(r#gen(FbconfigRuleType::RustBinary, &raw), "foo/bar/main.rs");
        let raw = r(&["foo/foo.rs", "foo/bar/main2.rs"]);
        assert_eq!(r#gen(FbconfigRuleType::RustBinary, &raw), "foo/foo.rs");
        let raw = r(&["foo/test.rs", "foo/bar/lib.rs"]);
        assert_eq!(r#gen(FbconfigRuleType::RustLibrary, &raw), "foo/bar/lib.rs");
        let raw = r(&["foo/foo.rs", "foo/bar/main2.rs"]);
        assert_eq!(r#gen(FbconfigRuleType::RustLibrary, &raw), "foo/foo.rs");
        let raw = r(&["foo/test.rs", "foo/bar/main.rs"]);
        assert_eq!(
            r#gen(FbconfigRuleType::RustUnittest, &raw),
            "foo/bar/main.rs"
        );
        let raw = r(&["foo/test.rs", "foo/bar/lib.rs"]);
        assert_eq!(
            r#gen(FbconfigRuleType::RustUnittest, &raw),
            "foo/bar/lib.rs"
        );
        let raw = r(&["foo/foo.rs", "foo/bar/main2.rs"]);
        assert_eq!(r#gen(FbconfigRuleType::RustUnittest, &raw), "foo/foo.rs");
    }

    #[test]
    fn relative_crate_root_test() {
        if cfg!(windows) {
            return; // Broken on Windows
        }

        let path = "src/main.rs";
        let targets_path = TargetsPath::new(PathInFbcode::new_mock("foo/bar/TARGETS")).unwrap();
        let cargo_path = |s: &str| CargoTomlPath::new(PathInFbcode::new_mock(s)).unwrap();

        assert_eq!(
            relative_crate_root(path, &targets_path, &cargo_path("foo/bar/Cargo.toml")).unwrap(),
            "src/main.rs"
        );

        assert_eq!(
            relative_crate_root(path, &targets_path, &cargo_path("foo/Cargo.toml")).unwrap(),
            "bar/src/main.rs"
        );
        assert_eq!(
            relative_crate_root(path, &targets_path, &cargo_path("foo/bar/biz/Cargo.toml"))
                .unwrap(),
            "../src/main.rs"
        );
        assert_eq!(
            relative_crate_root(path, &targets_path, &cargo_path("foo/bar2/biz/Cargo.toml"))
                .unwrap(),
            "../../bar/src/main.rs"
        );
    }
}
