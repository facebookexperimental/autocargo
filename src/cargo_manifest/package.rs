/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use cargo_toml::Edition;
use cargo_toml::Publish;
use cargo_toml::Value as CValue;
use cargo_util_schemas::manifest::StringOrBool;
use toml_edit::Table;

use super::toml_util::cargo_toml_to_toml_edit_item;
use super::toml_util::decorated_value;
use super::toml_util::edition_to_str;
use super::toml_util::maybe_add_to_table;
use super::toml_util::new_implicit_table;
use super::toml_util::ordered_array;
use super::toml_util::sorted_array;

/// Format package according to
/// https://doc.rust-lang.org/cargo/reference/manifest.html#the-package-section
#[derive(Debug)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub authors: Vec<String>,
    pub edition: Edition,
    pub rust_version: Option<String>,
    pub description: Option<String>,
    pub documentation: Option<String>,
    pub readme: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub license_file: Option<String>,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    pub workspace: Option<String>,
    pub build: Option<StringOrBool>,
    pub links: Option<String>,
    pub exclude: Vec<String>,
    pub include: Vec<String>,
    pub publish: Publish,
    pub metadata: Option<CValue>,
    pub default_run: Option<String>,
    pub autobins: bool,
    pub autoexamples: bool,
    pub autotests: bool,
    pub autobenches: bool,
}

impl Package {
    pub(super) fn to_toml(&self) -> Table {
        let Self {
            name,
            version,
            authors,
            edition,
            rust_version,
            description,
            documentation,
            readme,
            homepage,
            repository,
            license,
            license_file,
            keywords,
            categories,
            workspace,
            build,
            links,
            exclude,
            include,
            publish,
            metadata,
            default_run,
            autobins,
            autoexamples,
            autotests,
            autobenches,
        } = self;

        let mut table = new_implicit_table();
        {
            let table = &mut table;

            table["name"] = decorated_value(name.as_str());
            table["version"] = decorated_value(version.as_str());
            maybe_add_to_table(table, "authors", ordered_array(authors));
            table["edition"] = decorated_value(edition_to_str(edition));
            maybe_add_to_table(table, "rust-version", rust_version.as_deref());
            maybe_add_to_table(table, "description", description.as_deref());
            maybe_add_to_table(table, "documentation", documentation.as_deref());
            maybe_add_to_table(table, "readme", readme.as_deref());
            maybe_add_to_table(table, "homepage", homepage.as_deref());
            maybe_add_to_table(table, "repository", repository.as_deref());
            maybe_add_to_table(table, "license", license.as_deref());
            maybe_add_to_table(table, "license-file", license_file.as_deref());
            maybe_add_to_table(table, "keywords", sorted_array(keywords));
            maybe_add_to_table(table, "categories", sorted_array(categories));
            maybe_add_to_table(table, "workspace", workspace.as_deref());
            if let Some(build) = build {
                table["build"] = match build {
                    StringOrBool::String(value) => decorated_value(value.as_str()),
                    StringOrBool::Bool(value) => decorated_value(*value),
                };
            }
            maybe_add_to_table(table, "links", links.as_deref());
            maybe_add_to_table(table, "exclude", sorted_array(exclude));
            maybe_add_to_table(table, "include", sorted_array(include));
            if let Some(value) = match publish {
                Publish::Flag(true) => None,
                Publish::Flag(false) => Some(decorated_value(false)),
                Publish::Registry(regs) => sorted_array(regs).map(decorated_value),
            } {
                table["publish"] = value;
            }
            maybe_add_to_table(table, "default-run", default_run.as_deref());
            // For edition 2015 the autodiscovery is turned off if one
            // target is manually defined in Cargo.toml. To save ourselves
            // the trouble of checking it, lets just explicitly set
            // autodiscovery for 2015.
            if !autobins || edition == &Edition::E2015 {
                table["autobins"] = decorated_value(false);
            }
            if !autoexamples || edition == &Edition::E2015 {
                table["autoexamples"] = decorated_value(false);
            }
            if !autotests || edition == &Edition::E2015 {
                table["autotests"] = decorated_value(false);
            }
            if !autobenches || edition == &Edition::E2015 {
                table["autobenches"] = decorated_value(false);
            }
            if let Some(value) = metadata {
                table["metadata"] = cargo_toml_to_toml_edit_item(value);
            }
        }
        table
    }
}

#[cfg(test)]
pub fn empty_package() -> Package {
    let s = |s: &str| s.to_owned();
    Package {
        name: s(""),
        version: s(""),
        authors: vec![],
        edition: Edition::E2021,
        rust_version: None,
        description: None,
        documentation: None,
        readme: None,
        homepage: None,
        repository: None,
        license: None,
        license_file: None,
        keywords: vec![],
        categories: vec![],
        workspace: None,
        build: None,
        links: None,
        exclude: vec![],
        include: vec![],
        publish: Publish::Flag(true),
        metadata: None,
        default_run: None,
        autobins: true,
        autoexamples: true,
        autotests: true,
        autobenches: true,
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn s(s: &str) -> String {
        s.to_owned()
    }

    fn vec_s(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn package_toml_test_empty() {
        assert_eq!(
            empty_package().to_toml().to_string(),
            r#"name = ""
version = ""
edition = "2021"
"#
        );
    }

    #[test]
    fn package_toml_test() {
        use cargo_toml::Value as CValue;

        let package = Package {
            name: s("foo"),
            version: s("bar"),
            authors: vec_s(&["foo", "bar", "biz"]),
            edition: Edition::E2015,
            rust_version: Some(s("1.75")),
            description: Some(s(
                r#"Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod
tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam,
quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo
consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse
cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non
proident, sunt in culpa qui officia deserunt mollit anim id est laborum.
"#,
            )),
            documentation: Some(s("https://foo.bar/documentation")),
            readme: Some(s("foo/bar/biz/README.md")),
            homepage: Some(s("https://foo.bar/homepage")),
            repository: Some(s("https://foo.bar/repository")),
            license: Some(s("GPL")),
            license_file: Some(s("foo/bar/biz/GPL.md")),
            keywords: vec_s(&["keyword_foo", "keyword_bar", "keyword_biz"]),
            categories: vec_s(&["category_foo", "category_bar", "category_biz"]),
            workspace: Some(s("../../foo/workspace")),
            build: Some(StringOrBool::Bool(false)),
            links: Some(s("foobarlinks")),
            exclude: vec_s(&["exclude/foo", "**/and/bar"]),
            include: vec_s(&["include/foo", "**/or/bar"]),
            publish: Publish::Registry(vec_s(&["foo.registry.com", "bar.registry.com"])),
            metadata: Some(CValue::Table(
                vec![(
                    s("stuff"),
                    CValue::Array(vec![
                        CValue::Table(
                            vec![
                                (
                                    s("foo"),
                                    CValue::Table(
                                        vec![
                                            (s("fiz"), CValue::Float(3.18)),
                                            (
                                                s("faz"),
                                                CValue::Array(vec![CValue::Array(vec![
                                                    CValue::Table(
                                                        vec![
                                                            (s("lorem"), CValue::Integer(42)),
                                                            (s("ipsum"), CValue::Integer(7)),
                                                        ]
                                                        .into_iter()
                                                        .collect(),
                                                    ),
                                                    CValue::Table(
                                                        vec![(
                                                            s("ipsum"),
                                                            CValue::String(s("dolor sit amet")),
                                                        )]
                                                        .into_iter()
                                                        .collect(),
                                                    ),
                                                ])]),
                                            ),
                                        ]
                                        .into_iter()
                                        .collect(),
                                    ),
                                ),
                                (
                                    s("bar"),
                                    CValue::Datetime("2021-02-16T12:12:12.12Z".parse().unwrap()),
                                ),
                            ]
                            .into_iter()
                            .collect(),
                        ),
                        CValue::Table(
                            vec![(s("biz"), CValue::Boolean(true))]
                                .into_iter()
                                .collect(),
                        ),
                    ]),
                )]
                .into_iter()
                .collect(),
            )),
            default_run: Some(s("foobar.exe")),
            autobins: false,
            autoexamples: false,
            autotests: false,
            autobenches: false,
        };
        let table = package.to_toml();
        assert_eq!(
            toml_edit::DocumentMut::from(table).to_string(),
            r#"name = "foo"
version = "bar"
authors = ["foo", "bar", "biz"]
edition = "2015"
rust-version = "1.75"
description = """
Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod
tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam,
quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo
consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse
cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non
proident, sunt in culpa qui officia deserunt mollit anim id est laborum.
"""
documentation = "https://foo.bar/documentation"
readme = "foo/bar/biz/README.md"
homepage = "https://foo.bar/homepage"
repository = "https://foo.bar/repository"
license = "GPL"
license-file = "foo/bar/biz/GPL.md"
keywords = ["keyword_bar", "keyword_biz", "keyword_foo"]
categories = ["category_bar", "category_biz", "category_foo"]
workspace = "../../foo/workspace"
build = false
links = "foobarlinks"
exclude = ["**/and/bar", "exclude/foo"]
include = ["**/or/bar", "include/foo"]
publish = ["bar.registry.com", "foo.registry.com"]
default-run = "foobar.exe"
autobins = false
autoexamples = false
autotests = false
autobenches = false

[metadata]

[[metadata.stuff]]
bar = 2021-02-16T12:12:12.12Z

[metadata.stuff.foo]
faz = [[{ ipsum = 7, lorem = 42 }, { ipsum = "dolor sit amet" }]]
fiz = 3.18

[[metadata.stuff]]
biz = true
"#
        );
    }
}
