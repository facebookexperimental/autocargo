/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::collections::BTreeMap;

use cargo_toml::DepsSet;
use cargo_toml::Edition;
use cargo_toml::FeatureSet;
use cargo_toml::PatchSet;
use cargo_toml::Profiles;
use cargo_toml::Resolver;
use cargo_toml::Value;
use cargo_toml::Workspace;
use itertools::Itertools;
use toml_edit::ArrayOfTables;
use toml_edit::DocumentMut;
use toml_edit::Item;

use super::KeyedTargetDepsSet;
use super::Package;
use super::Product;
use super::dependencies::deps_set_to_toml;
use super::dependencies::target_deps_set_to_toml;
use super::product::ProductType;
use super::profiles::profiles_to_toml;
use super::toml_util::cargo_toml_to_toml_edit_value;
use super::toml_util::decorated_value;
use super::toml_util::maybe_add_to_table;
use super::toml_util::new_implicit_table;
use super::toml_util::sorted_array;
use super::toml_util::sorted_array_maybe_multiline;

/// Formatted with accordance to
/// https://doc.rust-lang.org/cargo/reference/manifest.html
#[derive(Debug, Default)]
pub struct Manifest {
    /// This string will be prepended to the output of to_toml_string
    pub prefix_comment: Option<String>,

    pub cargo_features: Vec<String>,
    pub package: Option<Package>,

    pub lib: Option<Product>,
    pub bin: Vec<Product>,
    pub example: Vec<Product>,
    pub test: Vec<Product>,
    pub bench: Vec<Product>,

    pub dependencies: DepsSet,
    pub dev_dependencies: DepsSet,
    pub build_dependencies: DepsSet,
    pub target: KeyedTargetDepsSet,

    pub features: FeatureSet,
    pub patch: PatchSet,
    pub profile: Profiles,
    pub workspace: Option<Workspace>,
    pub lints: BTreeMap<String, Value>,
}

impl Manifest {
    pub fn to_toml_string(&self) -> String {
        self.prefix_comment.clone().unwrap_or_default() + self.to_toml().to_string().trim_start()
    }

    fn to_toml(&self) -> DocumentMut {
        let Self {
            prefix_comment: _,
            cargo_features,
            package,
            lib,
            bin,
            example,
            test,
            bench,
            dependencies,
            dev_dependencies,
            build_dependencies,
            target,
            features,
            patch,
            profile,
            workspace,
            lints,
        } = self;

        let mut document = DocumentMut::new();
        let table = document.as_table_mut();

        maybe_add_to_table(
            table,
            "cargo-features",
            sorted_array_maybe_multiline(cargo_features),
        );
        if let Some(package) = package {
            table["package"] = Item::Table(package.to_toml());
        }
        if let Some(lib) = lib {
            let product_table = lib.to_toml(package.as_ref(), ProductType::Lib);
            if !product_table.is_empty() {
                table["lib"] = Item::Table(product_table);
            }
        }
        for (key, product_type, products) in &[
            ("bin", ProductType::Bin, bin),
            ("example", ProductType::Example, example),
            ("test", ProductType::Test, test),
            ("bench", ProductType::Bench, bench),
        ] {
            let array = products
                .iter()
                .sorted_by_key(|p| p.name.as_ref())
                .filter_map(|p| {
                    let product_table = p.to_toml(package.as_ref(), *product_type);
                    if product_table.is_empty() {
                        None
                    } else {
                        Some(product_table)
                    }
                })
                .fold(ArrayOfTables::new(), |mut array, v| {
                    array.push(v);
                    array
                });

            if !array.is_empty() {
                table[key] = Item::ArrayOfTables(array);
            }
        }

        let dependencies = deps_set_to_toml(dependencies);
        if !dependencies.is_empty() {
            table["dependencies"] = Item::Table(dependencies);
        }
        let dev_dependencies = deps_set_to_toml(dev_dependencies);
        if !dev_dependencies.is_empty() {
            table["dev-dependencies"] = Item::Table(dev_dependencies);
        }
        let build_dependencies = deps_set_to_toml(build_dependencies);
        if !build_dependencies.is_empty() {
            table["build-dependencies"] = Item::Table(build_dependencies);
        }
        let target = target_deps_set_to_toml(target);
        if !target.is_empty() {
            table["target"] = Item::Table(target);
        }

        let mut features_table = new_implicit_table();
        {
            let features_table = &mut features_table;
            for (k, vs) in features {
                maybe_add_to_table(
                    features_table,
                    k,
                    Some(sorted_array(vs).unwrap_or_default()),
                );
            }
        }
        if !features_table.is_empty() {
            table["features"] = Item::Table(features_table);
        }

        let mut patch_table = new_implicit_table();
        {
            let patch_table = &mut patch_table;
            for (k, vs) in patch {
                patch_table[k] = Item::Table(deps_set_to_toml(vs));
            }
        }
        if !patch_table.is_empty() {
            table["patch"] = Item::Table(patch_table);
        }

        let profile = profiles_to_toml(profile);
        if !profile.is_empty() {
            table["profile"] = Item::Table(profile);
        }

        if let Some(Workspace {
            members,
            default_members,
            package: _,
            exclude,
            metadata: _,
            resolver,
            dependencies: _,
            lints: _,
        }) = workspace
        {
            let mut workspace_table = new_implicit_table();
            {
                let workspace_table = &mut workspace_table;
                maybe_add_to_table(
                    workspace_table,
                    "members",
                    sorted_array_maybe_multiline(members),
                );
                maybe_add_to_table(
                    workspace_table,
                    "default-members",
                    sorted_array_maybe_multiline(default_members),
                );
                maybe_add_to_table(
                    workspace_table,
                    "exclude",
                    sorted_array_maybe_multiline(exclude),
                );
                if let Some(resolver) = resolver {
                    let is_default_resolver = if let Some(package) = package {
                        let default_resolver = match package.edition {
                            Edition::E2015 | Edition::E2018 => Resolver::V1,
                            Edition::E2021 | Edition::E2024 => Resolver::V2,
                            _ => Resolver::V2, // Edition is marked as non-exhaustive.
                        };
                        *resolver == default_resolver
                    } else {
                        false
                    };
                    if !is_default_resolver {
                        workspace_table["resolver"] = decorated_value(match resolver {
                            Resolver::V1 => "1",
                            Resolver::V2 => "2",
                            Resolver::V3 => "3",
                        });
                    }
                }
            }
            table["workspace"] = Item::Table(workspace_table);
        }

        if !lints.is_empty() {
            table["lints"] = Item::Table(
                lints
                    .iter()
                    .map(|(k, v)| (k, cargo_toml_to_toml_edit_value(v)))
                    .collect(),
            );
        }

        document
    }
}

#[cfg(test)]
mod test {
    use cargo_toml::Dependency;
    use cargo_toml::Profile;
    use cargo_toml::Target;
    use maplit::btreemap;

    use super::*;
    use crate::cargo_manifest::TargetKey;
    use crate::cargo_manifest::package::empty_package;
    use crate::cargo_manifest::product::Product;
    use crate::cargo_manifest::profiles::empty_profile;

    fn s(s: &str) -> String {
        s.to_owned()
    }

    fn tk(s: &str) -> TargetKey {
        TargetKey::try_from(s).unwrap()
    }

    fn vec_s(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn manifest_toml_test_empty() {
        assert_eq!(&Manifest::default().to_toml_string(), "");
    }

    #[test]
    fn manifest_toml_test_cargo_features() {
        assert_eq!(
            &Manifest {
                cargo_features: vec_s(&["feature3", "feature1", "zfeature"]),
                ..Manifest::default()
            }
            .to_toml_string(),
            r#"cargo-features = ["feature1", "feature3", "zfeature"]
"#
        );
    }

    #[test]
    fn manifest_toml_test_package() {
        assert_eq!(
            &Manifest {
                package: Some(empty_package()),
                ..Manifest::default()
            }
            .to_toml_string(),
            r#"[package]
name = ""
version = ""
edition = "2021"
"#
        );
    }

    #[test]
    fn manifest_toml_test_product() {
        assert_eq!(
            &Manifest {
                lib: Some(Product {
                    name: Some(s("foo-lib")),
                    ..Product::default()
                }),
                bin: vec![
                    Product {
                        name: Some(s("foo-bin")),
                        ..Product::default()
                    },
                    Product {
                        name: Some(s("bar-bin")),
                        ..Product::default()
                    },
                ],
                example: vec![Product {
                    name: Some(s("foo-example")),
                    ..Product::default()
                },],
                test: vec![Product {
                    name: Some(s("foo-test")),
                    ..Product::default()
                },],
                bench: vec![Product {
                    name: Some(s("foo-bench")),
                    ..Product::default()
                },],
                ..Manifest::default()
            }
            .to_toml_string(),
            r#"[lib]
name = "foo-lib"

[[bin]]
name = "bar-bin"

[[bin]]
name = "foo-bin"

[[example]]
name = "foo-example"

[[test]]
name = "foo-test"

[[bench]]
name = "foo-bench"
"#
        );
    }

    #[test]
    fn manifest_toml_test_dependencies() {
        assert_eq!(
            &Manifest {
                dependencies: btreemap! { s("foo") => Dependency::Simple(s("1")) },
                dev_dependencies: btreemap! { s("bar") => Dependency::Simple(s("2")) },
                build_dependencies: btreemap! { s("biz") => Dependency::Simple(s("3")) },
                target: btreemap! {
                    tk("unix") => Target {
                        dependencies: btreemap! { s("fiz") => Dependency::Simple(s("4")) },
                        dev_dependencies: DepsSet::new(),
                        build_dependencies: DepsSet::new(),
                    }
                },
                ..Manifest::default()
            }
            .to_toml_string(),
            r#"[dependencies]
foo = "1"

[dev-dependencies]
bar = "2"

[build-dependencies]
biz = "3"

[target.unix.dependencies]
fiz = "4"
"#
        );
    }

    #[test]
    fn manifest_toml_test_other() {
        assert_eq!(
            &Manifest {
                features: btreemap! { s("foo") => vec_s(&["foo", "bar"]), s("bar") => vec![] },
                patch: btreemap! { s("foo") => btreemap! { s("bar") => Dependency::Simple(s("42")) } },
                profile: Profiles {
                    dev: Some(Profile { incremental: Some(true), ..empty_profile()}),
                    ..Profiles::default()
                },
                workspace: Some(Workspace {
                    members: vec_s(&["foo", "bar"]),
                    default_members: Vec::new(),
                    package: None,
                    exclude: Vec::new(),
                    metadata: None,
                    resolver: None,
                    dependencies: DepsSet::new(),
                    lints: BTreeMap::new(),
                }),
                ..Manifest::default()
            }
            .to_toml_string(),
            r#"[features]
bar = []
foo = ["bar", "foo"]

[patch.foo]
bar = "42"

[profile.dev]
incremental = true

[workspace]
members = ["bar", "foo"]
"#
        );
    }
}
