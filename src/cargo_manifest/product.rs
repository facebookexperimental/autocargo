// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

use std::path::Path;

use cargo_toml::Edition;
use serde::Deserialize;
use toml_edit::Table;

use super::Package;
use super::toml_util::decorated_value;
use super::toml_util::edition_to_str;
use super::toml_util::maybe_add_to_table;
use super::toml_util::new_implicit_table;
use super::toml_util::sorted_array;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProductType {
    Lib,
    Bin,
    Example,
    Test,
    Bench,
}

/// Based on [cargo_toml::Product] and
/// https://doc.rust-lang.org/cargo/reference/cargo-targets.html
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Product {
    pub name: Option<String>,
    pub path: Option<String>,
    pub test: Option<bool>,
    pub doctest: Option<bool>,
    pub bench: Option<bool>,
    pub doc: Option<bool>,
    pub plugin: bool,
    pub proc_macro: bool,
    pub harness: bool,
    pub edition: Option<Edition>,
    pub crate_type: Vec<String>,
    pub required_features: Vec<String>,
}

impl Default for Product {
    fn default() -> Self {
        Self {
            name: None,
            path: None,
            test: None,
            doctest: None,
            bench: None,
            doc: None,
            plugin: false,
            proc_macro: false,
            harness: true,
            edition: None,
            crate_type: Vec::new(),
            required_features: Vec::new(),
        }
    }
}

impl Product {
    pub(super) fn to_toml(
        &self,
        maybe_package: Option<&Package>,
        product_type: ProductType,
    ) -> Table {
        let Self {
            name,
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
        } = self;

        let is_lib = product_type == ProductType::Lib;

        let table_entries = {
            let mut table_entries = Vec::new();
            if let Some(test) = test {
                if *test != default_test(product_type) {
                    table_entries.push(("test", decorated_value(*test)))
                }
            }
            if let Some(doctest) = doctest {
                if !doctest && is_lib {
                    table_entries.push(("doctest", decorated_value(*doctest)));
                }
            }
            if let Some(bench) = bench {
                if *bench != default_bench(product_type) {
                    table_entries.push(("bench", decorated_value(*bench)));
                }
            }
            if let Some(doc) = doc {
                if *doc != default_doc(product_type) {
                    table_entries.push(("doc", decorated_value(*doc)));
                }
            }
            if *plugin {
                table_entries.push(("plugin", decorated_value(*plugin)));
            }
            if *proc_macro && is_lib {
                table_entries.push(("proc-macro", decorated_value(*proc_macro)));
            }
            if !harness {
                table_entries.push(("harness", decorated_value(*harness)));
            }
            if let Some(edition) = edition {
                if maybe_package.map(|package| package.edition) != Some(*edition) {
                    table_entries.push(("edition", decorated_value(edition_to_str(edition))));
                }
            }
            if let Some(crate_type) = sorted_array(crate_type) {
                table_entries.push(("crate-type", decorated_value(crate_type)));
            }
            if let Some(required_features) = sorted_array(required_features) {
                table_entries.push(("required-features", decorated_value(required_features)));
            }
            table_entries
        };

        let CheckAutodiscoveryResult {
            can_be_autodiscovered,
            name_matches_package,
        } = check_autodiscovery_and_name(
            maybe_package,
            product_type,
            name,
            AllFieldsAreDefaults(table_entries.is_empty()),
        );

        let path_is_implicit = is_path_implicit(product_type, name, path, name_matches_package);

        let mut table = new_implicit_table();
        if can_be_autodiscovered && path_is_implicit {
            // Cargo's autodiscovery should handle this product
        } else {
            let table = &mut table;
            if !(is_lib && name_matches_package) {
                // The lib name would be inferred from package if it matched
                maybe_add_to_table(table, "name", name.as_deref());
            }
            if !path_is_implicit {
                maybe_add_to_table(table, "path", path.as_deref());
            }
            for (k, v) in table_entries {
                table[k] = v
            }
        }
        table
    }
}

fn default_test(product_type: ProductType) -> bool {
    match product_type {
        ProductType::Lib | ProductType::Bin | ProductType::Test => true,
        ProductType::Example | ProductType::Bench => false,
    }
}

fn default_bench(product_type: ProductType) -> bool {
    match product_type {
        ProductType::Lib | ProductType::Bin | ProductType::Bench => true,
        ProductType::Example | ProductType::Test => false,
    }
}

fn default_doc(product_type: ProductType) -> bool {
    match product_type {
        ProductType::Lib | ProductType::Bin => true,
        ProductType::Example | ProductType::Test | ProductType::Bench => false,
    }
}

#[derive(Debug, Eq, PartialEq)]
struct CheckAutodiscoveryResult {
    can_be_autodiscovered: bool,
    name_matches_package: bool,
}
struct AllFieldsAreDefaults(bool);

/// Cargo is using autodiscovery to automatically find and configure products
/// that match certain criteria. See the following link for more:
/// https://doc.rust-lang.org/cargo/reference/cargo-targets.html#target-auto-discovery
fn check_autodiscovery_and_name(
    maybe_package: Option<&Package>,
    product_type: ProductType,
    name: &Option<String>,
    AllFieldsAreDefaults(all_fields_are_defaults): AllFieldsAreDefaults,
) -> CheckAutodiscoveryResult {
    if let Some(package) = maybe_package {
        let name_matches_package = match product_type {
            ProductType::Lib | ProductType::Bin => &Some(package.name.replace('-', "_")) == name,
            _ => false,
        };

        let autodiscovery = match product_type {
            ProductType::Lib => name_matches_package,
            ProductType::Bin => package.autobins,
            ProductType::Example => package.autoexamples,
            ProductType::Test => package.autotests,
            ProductType::Bench => package.autobenches,
        };

        CheckAutodiscoveryResult {
            can_be_autodiscovered: autodiscovery && all_fields_are_defaults,
            name_matches_package,
        }
    } else {
        CheckAutodiscoveryResult {
            can_be_autodiscovered: false,
            name_matches_package: false,
        }
    }
}

/// If path of your product matches this layout:
/// https://doc.rust-lang.org/cargo/guide/project-layout.html
/// then it can be inferred by Cargo.
fn is_path_implicit(
    product_type: ProductType,
    name: &Option<String>,
    path: &Option<String>,
    name_matches_package: bool,
) -> bool {
    if let (Some(name), Some(path)) = (name, path) {
        let path = Path::new(path);
        let check_path = |name: &String, path: &Path, prefix: &str| -> bool {
            if let Ok(path) = path.strip_prefix(prefix) {
                path == Path::new(&(name.to_owned() + ".rs"))
                    || path == Path::new(name).join("main.rs")
            } else {
                false
            }
        };

        match product_type {
            ProductType::Lib => path == Path::new("src/lib.rs"),
            ProductType::Bin => {
                (path == Path::new("src/main.rs") && name_matches_package)
                    || check_path(name, path, "src/bin")
            }
            ProductType::Example => check_path(name, path, "examples"),
            ProductType::Test => check_path(name, path, "tests"),
            ProductType::Bench => check_path(name, path, "benches"),
        }
    } else {
        path.is_none()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::cargo_manifest::package::empty_package;

    fn s(s: &str) -> String {
        s.to_owned()
    }

    fn vec_s(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| (*s).to_owned()).collect()
    }

    fn test_product() -> Product {
        Product {
            name: None,
            path: None,
            test: Some(false),
            doctest: Some(false),
            bench: Some(false),
            doc: Some(false),
            plugin: true,
            proc_macro: true,
            harness: false,
            edition: Some(Edition::E2015),
            crate_type: vec_s(&["staticlib"]),
            required_features: vec_s(&["bar/biz"]),
        }
    }

    #[test]
    fn product_to_toml_test_empty() {
        for product_type in &[
            ProductType::Lib,
            ProductType::Bin,
            ProductType::Example,
            ProductType::Test,
            ProductType::Bench,
        ] {
            assert!(
                Product::default().to_toml(None, *product_type).is_empty(),
                "For {product_type:?}"
            );
            assert!(
                Product::default()
                    .to_toml(Some(&empty_package()), *product_type)
                    .is_empty(),
                "For {product_type:?}"
            );
        }
    }

    #[test]
    fn product_to_toml_test_lib() {
        assert_eq!(
            Product {
                name: Some(s("foo")),
                path: Some(s("src/lib.rs")),
                ..test_product()
            }
            .to_toml(None, ProductType::Lib)
            .to_string(),
            r#"name = "foo"
test = false
doctest = false
bench = false
doc = false
plugin = true
proc-macro = true
harness = false
edition = "2015"
crate-type = ["staticlib"]
required-features = ["bar/biz"]
"#,
        );
        assert_eq!(
            Product {
                name: Some(s("foo")),
                path: Some(s("src/foo.rs")),
                ..test_product()
            }
            .to_toml(
                Some(&Package {
                    name: s("foo"),
                    edition: Edition::E2015,
                    ..empty_package()
                }),
                ProductType::Lib
            )
            .to_string(),
            r#"path = "src/foo.rs"
test = false
doctest = false
bench = false
doc = false
plugin = true
proc-macro = true
harness = false
crate-type = ["staticlib"]
required-features = ["bar/biz"]
"#,
        );
    }

    #[test]
    fn product_to_toml_test_bin() {
        assert_eq!(
            Product {
                name: Some(s("foo")),
                path: Some(s("src/main.rs")),
                proc_macro: true,
                ..Product::default()
            }
            .to_toml(None, ProductType::Bin)
            .to_string(),
            r#"name = "foo"
path = "src/main.rs"
"#,
        );
        assert_eq!(
            Product {
                name: Some(s("foo")),
                path: Some(s("src/main.rs")),
                edition: Some(Edition::E2021),
                ..Product::default()
            }
            .to_toml(
                Some(&Package {
                    name: s("foo"),
                    edition: Edition::E2015,
                    ..empty_package()
                }),
                ProductType::Bin
            )
            .to_string(),
            r#"name = "foo"
edition = "2021"
"#,
        );
        assert_eq!(
            Product {
                name: Some(s("foo")),
                path: Some(s("src/main.rs")),
                ..Product::default()
            }
            .to_toml(
                Some(&Package {
                    name: s("foo"),
                    ..empty_package()
                }),
                ProductType::Bin
            )
            .to_string(),
            "",
        );
    }

    #[test]
    fn product_to_toml_test_other() {
        assert_eq!(
            Product {
                name: Some(s("foo")),
                path: Some(s("examples/foo.rs")),
                ..Product::default()
            }
            .to_toml(Some(&empty_package()), ProductType::Example)
            .to_string(),
            "",
        );
        assert_eq!(
            Product {
                name: Some(s("foo")),
                path: Some(s("tests/foo.rs")),
                ..Product::default()
            }
            .to_toml(
                Some(&Package {
                    autotests: false,
                    ..empty_package()
                }),
                ProductType::Test
            )
            .to_string(),
            r#"name = "foo"
"#,
        );
        assert_eq!(
            Product {
                name: Some(s("foo")),
                path: Some(s("benches/foo2.rs")),
                ..Product::default()
            }
            .to_toml(Some(&empty_package()), ProductType::Bench)
            .to_string(),
            r#"name = "foo"
path = "benches/foo2.rs"
"#,
        );
    }

    #[test]
    fn check_autodiscovery_and_name_test_no_package() {
        assert_eq!(
            check_autodiscovery_and_name(
                None,
                ProductType::Lib,
                &Some(s("foo")),
                AllFieldsAreDefaults(true),
            ),
            CheckAutodiscoveryResult {
                can_be_autodiscovered: false,
                name_matches_package: false,
            }
        );
    }

    #[test]
    fn check_autodiscovery_and_name_test_lib() {
        for (num, (pname, name, defaults, can_be_autodiscovered, name_matches_package)) in vec![
            ("foo-bar", "foo_bar", true, true, true),
            ("foo", "foo_bar", true, false, false),
            ("foo-bar", "foo_bar", false, false, true),
            ("foo", "foo_bar", false, false, false),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                check_autodiscovery_and_name(
                    Some(&Package {
                        name: s(pname),
                        ..empty_package()
                    }),
                    ProductType::Lib,
                    &Some(s(name)),
                    AllFieldsAreDefaults(defaults),
                ),
                CheckAutodiscoveryResult {
                    can_be_autodiscovered,
                    name_matches_package,
                },
                "Test case #{}",
                num
            );
        }
    }

    #[test]
    fn check_autodiscovery_and_name_test_bin() {
        for (num, (pname, autobins, name, defaults, can_be_autodiscovered, name_matches_package)) in
            vec![
                ("foo-bar", true, "foo_bar", true, true, true),
                ("foo", true, "foo_bar", true, true, false),
                ("foo-bar", false, "foo_bar", true, false, true),
                ("foo-bar", true, "foo_bar", false, false, true),
                ("foo-bar", false, "foo_bar", false, false, true),
                ("foo", false, "foo_bar", false, false, false),
            ]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                check_autodiscovery_and_name(
                    Some(&Package {
                        name: s(pname),
                        autobins,
                        ..empty_package()
                    }),
                    ProductType::Bin,
                    &Some(s(name)),
                    AllFieldsAreDefaults(defaults),
                ),
                CheckAutodiscoveryResult {
                    can_be_autodiscovered,
                    name_matches_package,
                },
                "Test case #{}",
                num
            );
        }
    }

    #[test]
    fn check_autodiscovery_and_name_test_other() {
        let pkg_auto = Package {
            name: s("foo"),
            autoexamples: true,
            autotests: true,
            autobenches: true,
            ..empty_package()
        };
        let pkg_no_auto = Package {
            name: s("foo"),
            autoexamples: false,
            autotests: false,
            autobenches: false,
            ..empty_package()
        };

        for product_type in &[ProductType::Example, ProductType::Test, ProductType::Bench] {
            for (num, (pkg, defaults, can_be_autodiscovered)) in vec![
                (&pkg_auto, true, true),
                (&pkg_auto, false, false),
                (&pkg_no_auto, true, false),
                (&pkg_no_auto, false, false),
            ]
            .into_iter()
            .enumerate()
            {
                assert_eq!(
                    check_autodiscovery_and_name(
                        Some(pkg),
                        *product_type,
                        &Some(s("foo")),
                        AllFieldsAreDefaults(defaults),
                    ),
                    CheckAutodiscoveryResult {
                        can_be_autodiscovered,
                        name_matches_package: false,
                    },
                    "Test case #{} for {:?}",
                    num,
                    product_type
                );
            }
        }
    }

    #[test]
    fn is_path_implicit_test() {
        assert!(is_path_implicit(
            ProductType::Lib,
            &Some(s("foo")),
            &Some(s("src/lib.rs")),
            false
        ));
        assert!(!is_path_implicit(
            ProductType::Lib,
            &Some(s("foo")),
            &Some(s("lib.rs")),
            false
        ));

        assert!(is_path_implicit(
            ProductType::Bin,
            &Some(s("foo")),
            &Some(s("src/main.rs")),
            true
        ));
        assert!(!is_path_implicit(
            ProductType::Bin,
            &Some(s("foo")),
            &Some(s("src/main.rs")),
            false
        ));
        assert!(is_path_implicit(
            ProductType::Bin,
            &Some(s("foo")),
            &Some(s("src/bin/foo.rs")),
            false
        ));
        assert!(!is_path_implicit(
            ProductType::Bin,
            &Some(s("foo")),
            &Some(s("src/bin/bar.rs")),
            false
        ));

        for (pty, path) in &[
            (ProductType::Example, "examples/foo.rs"),
            (ProductType::Test, "tests/foo.rs"),
            (ProductType::Bench, "benches/foo.rs"),
        ] {
            assert!(is_path_implicit(
                *pty,
                &Some(s("foo")),
                &Some(s(path)),
                false
            ));
            assert!(!is_path_implicit(
                *pty,
                &Some(s("bar")),
                &Some(s(path)),
                false
            ));
        }
    }
}
