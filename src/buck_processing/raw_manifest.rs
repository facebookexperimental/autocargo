/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;

use cargo_toml::Edition;
use cargo_toml::FeatureSet;
use cargo_toml::Profiles;
use cargo_toml::Publish;
use cargo_toml::Value;
use cargo_toml::Workspace;
pub use cargo_util_schemas::manifest::StringOrBool;
use serde::Deserialize;
use serde_with::rust::default_on_null;
use serde_with::rust::double_option;
use thrift_compiler::GenContext;

use super::rules::BuckRuleParseOutput;
use crate::cargo_manifest::Product;
use crate::cargo_manifest::TargetKey;
use crate::config::PatchGeneration;
use crate::config::PatchGenerationInput;

/// Enum describing type of rule that the manifest describes.
#[derive(Debug, Deserialize, Copy, Clone, Eq, PartialEq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RawFbconfigRuleType {
    /// Binary
    RustBinary,
    /// Library
    RustLibrary,
    /// Unittest
    RustUnittest,
    /// Bindgen generated library
    RustBindgenLibrary,
    /// Unknown rule type
    #[serde(other)]
    Other,
}

/// Enum describing platform for which a given dependency is added.
#[derive(Debug, Deserialize, Copy, Clone, Eq, PartialEq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RawOsDepsPlatform {
    /// Linux
    Linux,
    /// Macos
    Macos,
    /// Windows
    Windows,
    /// Unknown platform
    #[serde(other)]
    Other,
}

/// Structure that represents buck manifest as parsed from buck output of
/// *-rust-manifest rule.
///
/// NOTE: on #[serde(flatten)] - This "inlines" the fields into the structure
/// that contains flattened attributes. It is used here to group related options
/// that hopefuly makes the code easier to read and handle in code.
///
/// NOTE: on using "pub" attributes rather than getters/setters - The
/// RawBuckManifest can be accessed outside of this module only via non-mutable
/// borrow, so there is no risk in messing up the content of manifest. Making the
/// attributes public will make testing easier and will enable deconstructing
/// &RawBuckManifest for easier handling in code.
#[derive(Debug, Deserialize)]
pub struct RawBuckManifest {
    /// Name which is unique within a single TARGETS file.
    pub name: String,
    /// Type that defines if a rule is a binary, library, test, etc.
    pub fbconfig_rule_type: RawFbconfigRuleType,
    /// Group of attributes configuring Rust/Cargo build options.
    #[serde(flatten)]
    pub rust_config: RawBuckManifestRustConfig,
    /// Group of attributes configuring sources of build.
    #[serde(flatten)]
    pub sources: RawBuckManifestSources,
    /// Group of attributes configuring dependencies of build.
    #[serde(flatten)]
    pub dependencies: RawBuckManifestDependencies,
    /// Autocargo extra configuration for rule.
    #[serde(default, deserialize_with = "default_on_null::deserialize")]
    pub autocargo: AutocargoField,
}

/// Group of attributes configuring Rust/Cargo build options.
#[derive(Debug, Deserialize)]
pub struct RawBuckManifestRustConfig {
    /// Features that are always enabled for this crate.
    #[serde(deserialize_with = "default_on_null::deserialize")]
    pub features: Vec<String>,
    /// Usually name of the crate is the same as name of rule, but this field
    /// lets you change it.
    #[serde(rename = "crate")]
    pub crate_: Option<String>,
    /// If a crate root (lib.rs for libraries and main.rs for binaries) is not
    /// automatically found then this field can be used to point to it.
    pub crate_root: Option<PathBuf>,
    /// For buck this decides if it should generate a rule for unittests. In
    /// practice this field turns the unit tests on or off for a crate.
    pub unittests: bool,
    /// Whether this crate is providing a procedural macro and should be treated
    /// differently.
    pub proc_macro: bool,
    /// Extra features for unittests.
    #[serde(deserialize_with = "default_on_null::deserialize")]
    pub test_features: Vec<String>,
    /// Edition of Rust that this crate uses.
    pub edition: Option<Edition>,
}

/// Group of attributes configuring sources of build.
#[derive(Debug, Deserialize)]
pub struct RawBuckManifestSources {
    /// Evaluated sources (not as glob expressions) relative to the TARGETS file.
    #[serde(deserialize_with = "default_on_null::deserialize")]
    pub srcs: Vec<PathBuf>,
    /// Map where key are rules producing source files or paths to sources and
    /// values are paths where the key should be put.
    #[serde(deserialize_with = "default_on_null::deserialize")]
    pub mapped_srcs: HashMap<PathBuf, String>,
    /// Extra sources for unittests.
    #[serde(deserialize_with = "default_on_null::deserialize")]
    pub test_srcs: Vec<PathBuf>,
}

/// Group of attributes configuring dependencies of build.
#[derive(Debug, Deserialize)]
pub struct RawBuckManifestDependencies {
    /// List of either relative or absolute dependencies, in or out fbcode.
    #[serde(deserialize_with = "default_on_null::deserialize")]
    pub deps: Vec<BuckRuleParseOutput>,
    /// Depedencies that are renamed, so in Rust are reference by the mapped
    /// name.
    #[serde(deserialize_with = "default_on_null::deserialize")]
    pub named_deps: HashMap<String, BuckRuleParseOutput>,
    /// Dependencies per platform that are included in build only when building
    /// for that platform.
    #[serde(deserialize_with = "default_on_null::deserialize")]
    pub os_deps: Vec<(RawOsDepsPlatform, Vec<BuckRuleParseOutput>)>,
    /// List of tests that verify if this rule is correct. This includes the
    /// unittests rule.
    #[serde(deserialize_with = "default_on_null::deserialize")]
    pub tests: Vec<BuckRuleParseOutput>,
    /// Extra deps for unittests.
    #[serde(deserialize_with = "default_on_null::deserialize")]
    pub test_deps: Vec<BuckRuleParseOutput>,
    /// Extra named_deps for unittests.
    #[serde(deserialize_with = "default_on_null::deserialize")]
    pub test_named_deps: HashMap<String, BuckRuleParseOutput>,
    /// Extra platform deps for unittests.
    #[serde(deserialize_with = "default_on_null::deserialize")]
    pub test_os_deps: Vec<(RawOsDepsPlatform, Vec<BuckRuleParseOutput>)>,
}

/// Autocargo field used for fine-tuning autocargo generation per buck rule.
#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AutocargoField {
    /// Folder where the generated Cargo.toml file should be put, relative to the
    /// current TARGETS file.
    #[serde(default)]
    pub cargo_toml_dir: PathBuf,
    /// If true do not generate Cargo.toml for this rule and treat it as
    /// non-existing as a dependency.
    pub ignore_rule: bool,

    /// Configuration for the whole Cargo.toml file generated. When multiple Buck
    /// rules are put in a single Cargo.toml file only one of the rules might
    /// define cargo_toml_config field, the rest must use None here.
    pub cargo_toml_config: Option<AutocargoCargoTomlConfig>,
    /// Configuration for the library/binary/test/bench that is generated
    /// directly from the corresponding buck rule.
    pub cargo_target_config: AutocargoTargetConfig,
    /// Present only for thrift_library rules, contains thrift-specific configs.
    pub thrift: Option<AutocargoThrift>,
}

/// Configuration for the whole Cargo.toml file generated. Based on
/// [::cargo_toml::Manifest] and extended with fields from
/// https://doc.rust-lang.org/cargo/reference/manifest.html.
/// See [AutocargoPackageConfig] for explanation on Option<Option<T>> fields>
#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AutocargoCargoTomlConfig {
    /// Some unstable features require being listed here.
    #[serde(rename = "cargo-features")]
    pub cargo_features: Option<Vec<String>>,
    /// Configuration based on [::cargo_toml::Package].
    pub package: AutocargoPackageConfig,
    /// Configuration based on [::cargo_toml::Workspace].
    pub workspace: Option<Workspace>,
    /// Those are some extra dependencies structured like Cargo dependencies
    /// (dependencies, dev-dependencies, build-dependencies and target
    /// dependencies), but the values are Buck rules. Autocargo will resolve the
    /// rules for you as Cargo dependencies. Thanks to this field you might add
    /// extra dependencies to your generated Cargo.toml file that are not
    /// included in Buck or even delete some of the dependencies that Buck has,
    /// but Cargo shouldn't. Note that this enables you to add build-dependencies
    /// which don't exist in Buck.
    ///
    /// Check examples in dependencies_override documentation.
    pub extra_buck_dependencies: RawExtraBuckDependencies,
    /// Those are the last transformations that are applied directly on
    /// end-result generated Cargo dependencies, just before they are formatted
    /// and printed. Those transformations allow removing or editing any
    /// attribute of a buck-generated dependency or to add a completely new
    /// dependency not mentioned in buck and not even mentioned in
    /// thrird-party/rust/Cargo.toml.
    ///
    /// Note: If you want to remove a buck-generated dependency use the
    /// extra_buck_dependencies. It might be even practical sometimes to remove a
    /// buck-generated dependency and then re-add it with this field like in the
    /// below example:
    ///
    /// # Examples
    ///
    /// ```text
    /// rust_library(
    ///     name = "foo",
    ///     srcs = glob(["src/**/*.rs"]),
    ///     autocargo = {
    ///         "cargo_toml_config": {
    ///             "extra_buck_dependencies": {
    ///                 "dependencies": [
    ///                     (None, "//foo/bar:biz"),
    ///                     ("foobar", "//foo/bar:fiz"),
    ///                 ],
    ///             },
    ///             "dependencies_override": {
    ///                 "dependencies": {
    ///                      "biz": { "version": "0.4.2" },
    ///                      "foobar": { "features": ["foo2"] },
    ///                 },
    ///             },
    ///         },
    ///     },
    ///     deps = ["//foo/bar:biz"],
    /// )
    /// ```
    ///
    /// Could be generated into a Cargo.toml file like this:
    ///
    /// ```text
    /// [package]
    /// name = "foo"
    /// version = "0.1.0"
    /// edition = "2021"
    ///
    /// [dependencies]
    /// biz = "0.4.2"
    /// foobar = { package = "fiz", "path" = "../bar/fiz", "features": ["foo2"] }
    /// ```
    ///
    /// In the above example the dependency "//foo/bar:biz" is removed by
    /// extra_buck_dependencies entry and then added by dependencies_override
    /// so it doesn't contain any "path" entries. Similar result could be
    /// achieved with an entry in dependencies_override alone: `"biz": { "path":
    /// None, "version": "0.4.2" }`.
    ///
    /// The "//foo/bar/fiz" dependency is added by extra_buck_dependencies with a
    /// "foobar" alias and then extended by dependencies_override to enable some
    /// features. Notice that the dependencies_override use the dependency/alias
    /// to refer to a dependency and not the package name or buck target.
    pub dependencies_override: DependenciesOverride,
    /// Features for the crate.
    pub features: Option<FeatureSet>,
    /// This field is to allow defining a lib section in Cargo.toml file when it
    /// is not generated from Buck already. If you are looking for a way to
    /// modify fields of an existing generated library section then use
    /// "autocargo.cargo_target_config" on appropriate buck library rule.
    pub lib: Option<Product>,
    /// This field is to allow defining extra bins in Cargo.toml file that are
    /// not generated from any buck rule.
    pub bin: Vec<Product>,
    /// This field is to allow defining extra tests in Cargo.toml file that are
    /// not generated from any buck rule.
    pub test: Vec<Product>,
    /// Currently there aren't any Buck rules that would be generated into a
    /// benchmark rule, since Buck doesn't run benchmarks. This field is to allow
    /// defining benchmarks in Cargo.toml file.
    pub bench: Vec<Product>,
    /// Currently there aren't any Buck rules that would be generated into an
    /// example rule. This field is to allow defining examples in Cargo.toml
    /// file.
    pub example: Vec<Product>,
    /// How to generate the [patch] section for the crate.
    pub patch_generation: Option<PatchGeneration>,
    /// Specify additional [patch] section entries for this crate.
    ///
    /// Example:
    /// ```text
    /// "crates-io": [
    ///   "addr2line",
    ///   ("bytecount", { "git": "https://github.com/llogiq/bytecount", rev: "469eaf8395c99397cd64d059737a9054aa014088" }),
    /// ]
    /// ```
    ///
    /// This example copies the patch for `addr2line` from the third-party crates Cargo.toml
    /// and introduces a custom patch for `bytecount`.
    #[serde(default)]
    pub patch: PatchGenerationInput,
    /// Profiles for the crate.
    pub profile: Option<Profiles>,
    /// Lint configuration, such as `[lints.rust]` sections.
    ///
    /// ```text
    /// "lints": {
    ///     "rust": {
    ///         "unexpected_cfgs": {
    ///             "check-cfg", ["cfg(fbcode_build)"],
    ///             "level": "warn",
    ///         },
    ///     },
    /// }
    /// ```
    #[serde(default)]
    pub lints: BTreeMap<String, Value>,
}

/// Cargo package configuration, based on [::cargo_toml::Package] and extended by
/// https://doc.rust-lang.org/cargo/reference/manifest.html. The difference
/// between this structure and the original is that all the values are wrapped in
/// Option<T> or Option<Option<T>>.
/// - None - means the value is undefined and a default value should be used
/// - Some(None) or Some(Default::default) - means that the value should be
///   left undefined ignoring the defaults, e.g. authors: Some(vec![]) leaves
///   authors unspecified
/// - Some(Some(T)) or Some(T) - sets field to T
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
#[allow(missing_docs)]
pub struct AutocargoPackageConfig {
    /// If None use the name of the rule or the value of "crate" rule attribute.
    pub name: Option<String>,
    pub version: Option<String>,
    pub authors: Option<Vec<String>>,
    pub edition: Option<Edition>,
    #[serde(with = "double_option")]
    pub rust_version: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub description: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub documentation: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub readme: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub homepage: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub repository: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub license: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub license_file: Option<Option<String>>,
    pub keywords: Option<Vec<String>>,
    pub categories: Option<Vec<String>>,
    #[serde(with = "double_option")]
    pub workspace: Option<Option<String>>,
    pub build: Option<StringOrBool>,
    #[serde(with = "double_option")]
    pub links: Option<Option<String>>,
    pub exclude: Option<Vec<String>>,
    pub include: Option<Vec<String>>,
    pub publish: Option<Publish>,
    #[serde(with = "double_option")]
    pub metadata: Option<Option<Value>>,
    pub default_run: Option<String>,
    pub autobins: bool,
    pub autoexamples: bool,
    pub autotests: bool,
    pub autobenches: bool,
}

impl Default for AutocargoPackageConfig {
    fn default() -> Self {
        Self {
            name: None,
            version: None,
            authors: None,
            edition: None,
            rust_version: None,
            description: None,
            documentation: None,
            readme: None,
            homepage: None,
            repository: None,
            license: None,
            license_file: None,
            keywords: None,
            categories: None,
            workspace: None,
            build: None,
            links: None,
            exclude: None,
            include: None,
            publish: None,
            metadata: None,
            default_run: None,
            autobins: true,
            autoexamples: true,
            autotests: true,
            autobenches: true,
        }
    }
}

/// Those are some extra dependencies structured like Cargo dependencies.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RawExtraBuckDependencies {
    /// Notice that this field is flattened. It gives you ability to override
    /// dependencies, dev-dependencies and build-dependencies.
    #[serde(flatten)]
    pub deps: RawBuckTargetDependencies,
    /// For overriding target dependencies. Since the key is an arbitrary string
    /// you can both override RawOsDepsPlatform targets and create new ones.
    pub target: BTreeMap<TargetKey, RawBuckTargetDependencies>,
}

/// Structure for overriding dependencies, dev-dependencies and
/// build-dependencies.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct RawBuckTargetDependencies {
    pub dependencies: HashSet<RawBuckDependencyOverride>,
    pub dev_dependencies: HashSet<RawBuckDependencyOverride>,
    pub build_dependencies: HashSet<RawBuckDependencyOverride>,
}

/// This structure can have three representations in Buck's autocargo field:
/// - "//foo/bar:biz" - adds this target as a dependency
/// - ("fiz", "//foo/bar:biz") - adds this target as a named dependency
/// - (None, "//foo/bar:biz") - removes this target from dependencies
#[derive(Debug, Deserialize, Eq, PartialEq, Hash)]
#[serde(untagged)]
pub enum RawBuckDependencyOverride {
    Dep(BuckRuleParseOutput),
    NamedOrRemovedDep(Option<String>, BuckRuleParseOutput),
}

/// Those are overrides that will be applied to Cargo dependencies after all
/// buck-related generation is done.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DependenciesOverride {
    /// Notice that this field is flattened. It gives you ability to override
    /// dependencies, dev-dependencies and build-dependencies.
    #[serde(flatten)]
    pub deps: TargetDependenciesOverride,
    /// For overriding target dependencies. Since the key is an arbitrary string you
    /// can both override RawOsDepsPlatform targets and create new targets.
    pub target: BTreeMap<TargetKey, TargetDependenciesOverride>,
}

/// Structure for overriding dependencies, dev-dependencies and
/// build-dependencies.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
#[allow(missing_docs)]
pub struct TargetDependenciesOverride {
    pub dependencies: BTreeMap<String, CargoDependencyOverride>,
    pub dev_dependencies: BTreeMap<String, CargoDependencyOverride>,
    pub build_dependencies: BTreeMap<String, CargoDependencyOverride>,
}

/// This structure does to dependencies what AutocargoPackageConfig does to
/// package fields. It is based on the [cargo_toml::DependencyDetail] struct,
/// but each field is wrapped in an extra Option, so that e.g.
/// - `version = None` will leave the version unchanged
/// - `version = Some(None)` will remove the version information from dependency
/// - `version = Some(Some(foo))` will set version to foo
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
#[allow(missing_docs)]
pub struct CargoDependencyOverride {
    #[serde(with = "double_option")]
    pub version: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub registry: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub registry_index: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub path: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub git: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub branch: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub tag: Option<Option<String>>,
    #[serde(with = "double_option")]
    pub rev: Option<Option<String>>,
    pub features: Option<Vec<String>>,
    pub optional: Option<bool>,
    pub default_features: Option<bool>,
    #[serde(with = "double_option")]
    pub package: Option<Option<String>>,
}

/// Configuration for the library/binary/test/bench that is generated directly
/// from the corresponding buck rule. Follows the same approach to optional
/// values as [AutocargoPackageConfig]
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(missing_docs)]
pub struct AutocargoTargetConfig {
    pub name: Option<String>,
    pub path: Option<String>,
    #[serde(with = "double_option")]
    pub test: Option<Option<bool>>,
    #[serde(with = "double_option")]
    pub doctest: Option<Option<bool>>,
    pub bench: Option<bool>,
    pub doc: Option<bool>,
    pub plugin: bool,
    pub proc_macro: Option<bool>,
    pub harness: bool,
    #[serde(with = "double_option")]
    pub edition: Option<Option<Edition>>,
    pub crate_type: Vec<String>,
    pub required_features: Vec<String>,
}

impl Default for AutocargoTargetConfig {
    fn default() -> Self {
        Self {
            name: None,
            path: None,
            test: None,
            doctest: None,
            bench: None,
            doc: None,
            plugin: false,
            proc_macro: None,
            harness: true,
            edition: None,
            crate_type: Vec::new(),
            required_features: Vec::new(),
        }
    }
}

/// Thrift-specific configs that should be passed to thrift compiler.
#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct AutocargoThrift {
    /// Base path for thrift files.
    pub base_path: String,
    /// The type of the crate being generated for.
    pub gen_context: GenContext,
    /// Options for the thrift compiler.
    pub options: AutocargoThriftOptions,
    /// Map of thrift source files to list of services. The list is irrelevant
    /// for autocargo.
    pub thrift_srcs: HashMap<String, Vec<String>>,
    /// Target name without any `-clients` or `-services` suffix. The name for
    /// the `-dep-map` target is constructed from this.
    pub unsuffixed_name: String,
}

/// Options for the thrift compiler.
#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct AutocargoThriftOptions {
    /// Path to where the cratemap was generated by Buck. This value shouldn't
    /// be ever used since Buck's distributed cache will fill it up with values
    /// computed on Sandcastle hosts making this path totally useless. Instead
    /// autocargo will call Buck for each thrift rule to generate the cratemap.
    ///
    /// Extracting it to a private field will remove it from "more_options"
    /// section while preventing code accessing its value.
    cratemap: String,
    /// The crate name through which the thrift generated types can be used e.g.
    /// `use foo__types;`
    pub types_crate: String,
    /// Crate name for the clients crate, if any. There is no clients crate if
    /// the Thrift library contains no services.
    pub clients_crate: Option<String>,
    /// Crate name for the services crate, if any.
    pub services_crate: Option<String>,
    /// Extra Rust srcs included into the types crate.
    /// Of the format "path/to/first.rs:path/to/second.rs:somewhere/third.rs"
    pub types_include_srcs: Option<String>,
    /// Extra Rust srcs copied into the types crate.
    /// Of the format "path/to/first.rs:path/to/second.rs:somewhere/third.rs"
    pub types_extra_srcs: Option<String>,
    /// Extra Rust srcs included into the clients crate.
    pub clients_include_srcs: Option<String>,
    /// Extra Rust srcs included into the services crate.
    pub services_include_srcs: Option<String>,
    /// Rest of options.
    #[serde(flatten)]
    pub more_options: BTreeMap<String, Option<String>>,
}

#[cfg(test)]
impl RawBuckManifest {
    pub fn empty_test() -> RawBuckManifest {
        Self {
            name: String::new(),
            fbconfig_rule_type: RawFbconfigRuleType::RustBinary,
            rust_config: RawBuckManifestRustConfig {
                features: Vec::new(),
                crate_: None,
                crate_root: None,
                unittests: true,
                proc_macro: false,
                test_features: Vec::new(),
                edition: None,
            },
            sources: RawBuckManifestSources {
                srcs: Vec::new(),
                mapped_srcs: HashMap::new(),
                test_srcs: Vec::new(),
            },
            dependencies: RawBuckManifestDependencies {
                deps: Vec::new(),
                named_deps: HashMap::new(),
                os_deps: Vec::new(),
                tests: Vec::new(),
                test_deps: Vec::new(),
                test_named_deps: HashMap::new(),
                test_os_deps: Vec::new(),
            },
            autocargo: AutocargoField::default(),
        }
    }
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use assert_matches::assert_matches;
    use maplit::btreemap;
    use maplit::hashmap;
    use serde_json::from_str;
    use serde_json::from_value;
    use serde_json::json;
    use thrift_compiler::GenContext;

    use super::*;
    use crate::buck_processing::rules::BuckRule;
    use crate::buck_processing::rules::RuleName;

    #[test]
    fn fbconfig_rule_type_test() {
        let parse = |value: &str| -> Result<RawFbconfigRuleType, _> { from_value(json!(value)) };

        assert_matches!(parse("rust_binary"), Ok(RawFbconfigRuleType::RustBinary));
        assert_matches!(parse("rust_library"), Ok(RawFbconfigRuleType::RustLibrary));
        assert_matches!(
            parse("rust_unittest"),
            Ok(RawFbconfigRuleType::RustUnittest)
        );
        assert_matches!(
            parse("rust_bindgen_library"),
            Ok(RawFbconfigRuleType::RustBindgenLibrary)
        );
        assert_matches!(parse("rust_unknown"), Ok(RawFbconfigRuleType::Other));
    }

    #[test]
    fn os_deps_platform_test() {
        let parse = |value: &str| -> Result<RawOsDepsPlatform, _> { from_value(json!(value)) };

        assert_matches!(parse("linux"), Ok(RawOsDepsPlatform::Linux));
        assert_matches!(parse("macos"), Ok(RawOsDepsPlatform::Macos));
        assert_matches!(parse("windows"), Ok(RawOsDepsPlatform::Windows));
        assert_matches!(parse("solaris"), Ok(RawOsDepsPlatform::Other));
    }

    #[test]
    fn raw_buck_manifest_test() {
        assert_matches!(
            from_str::<RawBuckManifest>(include_str!("../../buck_generated/autocargo_rust_manifest.json")),
            Ok(rule) => {
                assert_eq!(rule.name, "autocargo");
                assert_eq!(rule.fbconfig_rule_type, RawFbconfigRuleType::RustBinary);
                assert!(rule.rust_config.unittests);
                assert!(!rule.rust_config.proc_macro);
                assert!(rule.dependencies.deps.contains(
                    &BuckRuleParseOutput::RuleName(RuleName {
                        name: "autocargo_lib".to_owned(),
                        subtarget: None,
                    })
                ));
            }
        );

        assert_matches!(
            from_str::<RawBuckManifest>(include_str!("../../buck_generated/autocargo_lib_rust_manifest.json")),
            Ok(rule) => {
                assert_eq!(rule.name, "autocargo_lib");
                assert_eq!(rule.fbconfig_rule_type, RawFbconfigRuleType::RustLibrary);
                assert!(rule.rust_config.unittests);
                assert!(!rule.rust_config.proc_macro);
                assert_eq!(rule.rust_config.crate_, Some("autocargo".to_owned()));
                assert!(rule.sources.srcs.contains(&Path::new("src/buck_processing/manifest.rs").to_owned()));
                assert!(rule.dependencies.test_deps.contains(
                    &BuckRuleParseOutput::FullyQualified(BuckRule::new_mock(
                        "fbsource",
                        "third-party/rust",
                        "assert_matches",
                    ))
                ));
            }
        );
    }

    #[test]
    fn autocargo_field_test() {
        assert_matches!(
            from_value::<AutocargoField>(json!({
                "cargo_toml_dir": "file/path",
                "cargo_toml_config": {
                    "package": {
                        "readme": "some",
                        "publish": ["val1", "val2"],
                    },
                    "features": {"default": ["some_crate"]},
                },
                "thrift": {
                    "base_path": "foo/bar",
                    "gen_context": "lib",
                    "options": {
                        "types_crate": "some__types",
                        "cratemap": "some_value",
                        "opt": "val",
                    },
                    "thrift_srcs": {
                        "src_foo": [],
                    },
                    "unsuffixed_name": "thing-rust",
                }
            })),
            Ok(field) => {
                assert_eq!(field.cargo_toml_dir, Path::new("file/path").to_owned());
                let cargo_toml_config = field.cargo_toml_config.as_ref().unwrap();
                assert_eq!(
                    cargo_toml_config.features,
                    Some(btreemap! {
                        "default".to_owned() => vec!["some_crate".to_owned()],
                    })
                );
                assert_eq!(
                    cargo_toml_config.package.publish,
                    Some(Publish::Registry(
                        vec!["val1".to_owned(), "val2".to_owned()],
                    ))
                );
                assert_eq!(
                    cargo_toml_config.package.readme,
                    Some(Some("some".to_owned()))
                );
                assert_eq!(cargo_toml_config.package.build, None);
                assert_eq!(field.cargo_target_config.name, None);
                assert_eq!(field.thrift, Some(AutocargoThrift {
                    base_path: "foo/bar".to_owned(),
                    gen_context: GenContext::Types,
                    options: AutocargoThriftOptions {
                        cratemap: "some_value".to_owned(),
                        types_crate: "some__types".to_owned(),
                        clients_crate: None,
                        services_crate: None,
                        types_include_srcs: None,
                        types_extra_srcs: None,
                        clients_include_srcs: None,
                        services_include_srcs: None,
                        more_options: btreemap! {
                            "opt".to_owned() => Some("val".to_owned())
                        },
                    },
                    thrift_srcs: hashmap! {
                        "src_foo".to_owned() => Vec::new()
                    },
                    unsuffixed_name: "thing-rust".to_owned(),
                }));
            }
        );
    }

    #[test]
    fn autocargo_field_test_types_include_srcs() {
        assert_matches!(
            from_value::<AutocargoField>(json!({
                "cargo_toml_dir": "file/path",
                "cargo_toml_config": {
                    "package": {
                        "readme": "some",
                        "publish": ["val1", "val2"],
                    },
                    "features": {"default": ["some_crate"]},
                },
                "thrift": {
                    "base_path": "foo/bar",
                    "gen_context": "types",
                    "options": {
                        "cratemap": "some_value",
                        "types_crate": "some__types",
                        "types_include_srcs": "path_a:path_b",
                        "types_extra_srcs": "path_c:path_d",
                        "opt": "val",
                    },
                    "thrift_srcs": {
                        "src_foo": [],
                    },
                    "unsuffixed_name": "thing-rust",
                }
            })),
            Ok(field) => {
                assert_eq!(field.cargo_toml_dir, Path::new("file/path").to_owned());
                let cargo_toml_config = field.cargo_toml_config.as_ref().unwrap();
                assert_eq!(
                    cargo_toml_config.features,
                    Some(btreemap! {
                        "default".to_owned() => vec!["some_crate".to_owned()],
                    })
                );
                assert_eq!(
                    cargo_toml_config.package.publish,
                    Some(Publish::Registry(
                        vec!["val1".to_owned(), "val2".to_owned()],
                    ))
                );
                assert_eq!(
                    cargo_toml_config.package.readme,
                    Some(Some("some".to_owned()))
                );
                assert_eq!(cargo_toml_config.package.build, None);
                assert_eq!(field.cargo_target_config.name, None);
                assert_eq!(field.thrift, Some(AutocargoThrift {
                    base_path: "foo/bar".to_owned(),
                    gen_context: GenContext::Types,
                    options: AutocargoThriftOptions {
                        cratemap: "some_value".to_owned(),
                        types_crate: "some__types".to_owned(),
                        clients_crate: None,
                        services_crate: None,
                        types_include_srcs: Some("path_a:path_b".to_owned()),
                        types_extra_srcs: Some("path_c:path_d".to_owned()),
                        clients_include_srcs: None,
                        services_include_srcs: None,
                        more_options: btreemap! {
                            "opt".to_owned() => Some("val".to_owned())
                        },
                    },
                    thrift_srcs: hashmap! {
                        "src_foo".to_owned() => Vec::new()
                    },
                    unsuffixed_name: "thing-rust".to_owned(),
                }));
            }
        );
    }
}
