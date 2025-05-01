// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use cargo_toml::Dependency;
use cargo_toml::DependencyDetail;
use cargo_toml::DepsSet;
use cargo_toml::Target;
use toml_edit::InlineTable;
use toml_edit::Item;
use toml_edit::Table;

use super::KeyedTargetDepsSet;
use super::toml_util::cargo_toml_to_toml_edit_value;
use super::toml_util::decorated_value;
use super::toml_util::maybe_add_to_inline_table;
use super::toml_util::new_implicit_table;
use super::toml_util::sorted_array;

/// Formats dependencies with accordance to
/// https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html
pub fn deps_set_to_toml(deps: &DepsSet) -> Table {
    let mut table = new_implicit_table();

    for (alias, dep) in deps {
        let item = match dep {
            Dependency::Simple(v) => decorated_value(v.as_str()),
            Dependency::Detailed(_) => {
                if let Some(DependencyDetail {
                    version,
                    registry,
                    registry_index,
                    path,
                    inherited: _,
                    git,
                    branch,
                    tag,
                    rev,
                    features,
                    optional,
                    default_features,
                    package,
                    unstable,
                }) = dep.detail()
                {
                    let mut dep_table = InlineTable::default();
                    {
                        let dep_table = &mut dep_table;
                        maybe_add_to_inline_table(dep_table, "package", package.as_deref());
                        maybe_add_to_inline_table(dep_table, "version", version.as_deref());
                        maybe_add_to_inline_table(dep_table, "registry", registry.as_deref());
                        maybe_add_to_inline_table(
                            dep_table,
                            "registry-index",
                            registry_index.as_deref(),
                        );
                        maybe_add_to_inline_table(dep_table, "path", path.as_deref());
                        maybe_add_to_inline_table(dep_table, "git", git.as_deref());
                        maybe_add_to_inline_table(dep_table, "branch", branch.as_deref());
                        maybe_add_to_inline_table(dep_table, "tag", tag.as_deref());
                        maybe_add_to_inline_table(dep_table, "rev", rev.as_deref());
                        maybe_add_to_inline_table(dep_table, "features", sorted_array(features));
                        maybe_add_to_inline_table(
                            dep_table,
                            "optional",
                            if *optional { Some(true) } else { None },
                        );
                        maybe_add_to_inline_table(
                            dep_table,
                            "default-features",
                            if *default_features { None } else { Some(false) },
                        );
                        for (k, v) in unstable {
                            dep_table.get_or_insert(k, cargo_toml_to_toml_edit_value(v));
                        }
                    }
                    dep_table.fmt();
                    decorated_value(dep_table)
                } else {
                    // This should never happen.
                    continue;
                }
            }
            Dependency::Inherited(_) => unimplemented!(
                "dependency `{alias}` uses inherited dependency syntax whic his not supported"
            ),
        };

        table[alias] = item;
    }

    table
}

/// Formats target dependencies with accordance to
/// https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#platform-specific-dependencies
pub fn target_deps_set_to_toml(target_deps: &KeyedTargetDepsSet) -> Table {
    let mut table = new_implicit_table();

    for (target_name, target) in target_deps {
        let target = target_to_toml(target);
        if !target.is_empty() {
            table.insert_formatted(target_name, Item::Table(target));
        }
    }

    table
}

fn target_to_toml(target: &Target) -> Table {
    let Target {
        dependencies,
        dev_dependencies,
        build_dependencies,
    } = target;

    let mut table = new_implicit_table();
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

    table
}

#[cfg(test)]
mod test {
    use std::collections::BTreeMap;

    use maplit::btreemap;

    use super::*;
    use crate::cargo_manifest::TargetKey;

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
    fn deps_set_to_toml_test_empty() {
        assert!(deps_set_to_toml(&DepsSet::new()).is_empty());
    }

    #[test]
    fn deps_set_to_toml_test() {
        assert_eq!(
            deps_set_to_toml(&btreemap! {
                s("foo") => Dependency::Simple(s("1")),
                s("bar") => Dependency::Detailed(Box::default()),
                s("biz") => Dependency::Detailed(Box::new(DependencyDetail {
                    version: Some(s("version")),
                    registry: Some(s("registry")),
                    registry_index: Some(s("registry_index")),
                    path: Some(s("path")),
                    inherited: false,
                    git: Some(s("git")),
                    branch: Some(s("branch")),
                    tag: Some(s("tag")),
                    rev: Some(s("rev")),
                    features: vec_s(&["foo", "bar"]),
                    optional: true,
                    default_features: false,
                    package: Some(s("package")),
                    unstable: BTreeMap::new(),
                }))
            })
            .to_string(),
            r#"bar = {}
biz = { package = "package", version = "version", registry = "registry", registry-index = "registry_index", path = "path", git = "git", branch = "branch", tag = "tag", rev = "rev", features = ["bar", "foo"], optional = true, default-features = false }
foo = "1"
"#
        );
    }

    #[test]
    fn target_deps_set_to_toml_test_empty() {
        assert!(target_deps_set_to_toml(&KeyedTargetDepsSet::new()).is_empty());
    }

    #[test]
    fn target_deps_set_to_toml_test() {
        let table = target_deps_set_to_toml(&btreemap! {
            tk(r#"'cfg(target_os = "linux")'"#) => Target {
                dependencies: btreemap! { s("foo") => Dependency::Simple(s("1")) },
                dev_dependencies: DepsSet::new(),
                build_dependencies: DepsSet::new(),
            },
            tk("unix") => Target {
                dependencies: btreemap! { s("bar") => Dependency::Simple(s("2")) },
                dev_dependencies: btreemap! { s("biz") => Dependency::Simple(s("3")) },
                build_dependencies: DepsSet::new(),
            }
        });
        assert_eq!(
            toml_edit::Document::from(table).to_string(),
            r#"['cfg(target_os = "linux")'.dependencies]
foo = "1"

[unix.dependencies]
bar = "2"

[unix.dev-dependencies]
biz = "3"
"#
        );
    }

    #[test]
    fn target_to_toml_test_empty() {
        assert!(
            target_to_toml(&Target {
                dependencies: DepsSet::new(),
                dev_dependencies: DepsSet::new(),
                build_dependencies: DepsSet::new(),
            })
            .is_empty()
        );
    }

    #[test]
    fn target_to_toml_test() {
        let table = target_to_toml(&Target {
            dependencies: btreemap! { s("foo") => Dependency::Simple(s("1")) },
            dev_dependencies: btreemap! { s("bar") => Dependency::Simple(s("2")) },
            build_dependencies: btreemap! { s("biz") => Dependency::Simple(s("3")) },
        });
        assert_eq!(
            toml_edit::Document::from(table).to_string(),
            r#"[dependencies]
foo = "1"

[dev-dependencies]
bar = "2"

[build-dependencies]
biz = "3"
"#,
        );
    }
}
