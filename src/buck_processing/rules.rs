/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

//! This module is for deserializing and handling of Buck rules, both for
//! interaction with Buck commands and for deserializing Buck rust manifests.

use std::fmt;
use std::fmt::Display;
use std::path::PathBuf;

use anyhow::Error;
use anyhow::Result;
use anyhow::anyhow;
use derive_more::AsRef;
use derive_more::From;
use derive_more::TryInto;
use getset::Getters;

use crate::paths::TargetsPath;

/// The rust manifest buck rule is created by appending the following suffix to
/// the name of the rust binary/library/unittest that this manifest is
/// describing.
static RUST_MANIFEST_SFX: &str = "-rust-manifest";
/// The thrift cratemap buck rule is created by appending the following suffix to
/// the name of the rust library created via thrift.
static THRIFT_CRATEMAP_SFX: &str = "-dep-map";

/// Structure describing a fully qualified build target. For build targets
/// inside fbcode repo use [FbcodeBuckRule].
/// See https://buck.build/concept/build_target.html for more information on the
/// format of buck targets.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Getters)]
#[getset(get = "pub")]
pub struct BuckRule {
    repo: String,
    path: PathBuf,
    name: String,
}

impl BuckRule {
    #[cfg(test)]
    pub fn new_mock(
        repo: impl Into<String>,
        path: impl Into<PathBuf>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            repo: repo.into(),
            path: path.into(),
            name: name.into(),
        }
    }
}

/// Structure describing a fully qualified build target in fbcode repo.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FbcodeBuckRule {
    pub path: TargetsPath,
    pub name: String,
}

impl Display for FbcodeBuckRule {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(
            formatter,
            "fbcode//{}:{}",
            self.path.as_dir().as_ref().display(),
            self.name,
        )
    }
}

/// Target in the current module, like :foobar or :foobar[doc].
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RuleName {
    pub name: String,
    pub subtarget: Option<String>,
}

/// Enum used for deserializing string as a buck rule.
/// See https://buck.build/concept/build_target.html for more information on the
/// format of buck targets.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, TryInto)]
pub enum BuckRuleParseOutput {
    /// Example rule: fbsource//third-party/rust:foo
    FullyQualified(BuckRule),
    /// Example rule: //common/rust/foo:bar
    FullyQualifiedInFbcode(FbcodeBuckRule),
    /// Example rule: :foobar
    RuleName(RuleName),
}

/// This newtype is used to distinguish between buck rules of unknown type (like
/// rust rules, but also c++, python or thrift etc.) and rules that presumably
/// point to rust manifest rules.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, AsRef, From)]
pub struct BuckManifestRule(FbcodeBuckRule);

impl From<&FbcodeBuckRule> for BuckManifestRule {
    /// Given a FbcodeBuckRule create a rust manifest rule assuming the given
    /// rule is a rust rule.
    ///
    /// NOTE: this rule might not exist in Buck, since it is created from
    /// FbcodeBuckRule at the point when we don't know if FbcodeBuckRule is a
    /// rust rule or not. The verification and filtering is done later by "buck
    /// query" in the BuckManifestLoader.
    fn from(rule: &FbcodeBuckRule) -> Self {
        Self(FbcodeBuckRule {
            path: rule.path.clone(),
            name: format!("{}{}", rule.name, RUST_MANIFEST_SFX),
        })
    }
}

impl TryFrom<BuckManifestRule> for FbcodeBuckRule {
    type Error = Error;

    fn try_from(rule: BuckManifestRule) -> Result<Self, Self::Error> {
        Ok(Self {
            name: rule
                .0
                .name
                .strip_suffix(RUST_MANIFEST_SFX)
                .ok_or_else(|| {
                    anyhow!(
                        "BuckManifestRule ({:?}) name did not end in {}",
                        rule,
                        RUST_MANIFEST_SFX,
                    )
                })?
                .to_owned(),
            path: rule.0.path,
        })
    }
}

/// This type is used to distinguish between buck rules of unknown type (like
/// rust rules, but also c++, python or thrift etc.) and rules that point to
/// thrift cratemap rules.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ThriftCratemapRule {
    path: TargetsPath,
    name_of_library: String,
}

impl ThriftCratemapRule {
    /// Given a FbcodeBuckRule create a thrift cratemap rule assuming the given
    /// rule is a rust thrift rule.
    ///
    /// NOTE: As opposed to BuckManifestRule we know this rule must exist,
    /// because the code creates it only at the point when the rust library
    /// is verified to be generated by Thrift.
    pub fn from_library_rule(rule: FbcodeBuckRule) -> Self {
        assert!(
            rule.name.ends_with("-rust"),
            "unexpected thrift library {rule}",
        );
        ThriftCratemapRule {
            path: rule.path,
            name_of_library: rule.name,
        }
    }

    /// The target for the dep map, ending in `-rust-dep-map`.
    pub fn fbcode_buck_rule(&self) -> FbcodeBuckRule {
        FbcodeBuckRule {
            path: self.path.clone(),
            name: self.name_of_library.clone() + THRIFT_CRATEMAP_SFX,
        }
    }

    /// The target for the Rust library this dep map is for, ending in `-rust`.
    pub fn to_library_rule(&self) -> FbcodeBuckRule {
        FbcodeBuckRule {
            path: self.path.clone(),
            name: self.name_of_library.clone(),
        }
    }
}

mod parsing {
    use std::path::Path;
    use std::str::FromStr;
    use std::sync::LazyLock;

    use anyhow::Error;
    use regex::Regex;
    use serde::Deserialize;
    use serde::Deserializer;
    use serde::de;

    use super::*;

    // Based on https://buck.build/concept/build_target.html
    static BUCK_FULLY_QUALIFIED_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^([A-Za-z0-9._-]+)//([A-Za-z0-9/._-]*):([A-Za-z0-9_/.=,@~+-]+)$").unwrap()
    });
    static BUCK_FULLY_QUALIFIED_IN_FBCODE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:fbcode)?//([A-Za-z0-9/._-]*):([A-Za-z0-9_/.=,@~+-]+)$").unwrap()
    });
    static BUCK_RULE_NAME_REGEX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^:([A-Za-z0-9_/.=,@~+-]+)(?:\[([a-z_]+)\])?$").unwrap());

    impl FromStr for BuckRuleParseOutput {
        type Err = Error;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            if let Some(captures) = BUCK_FULLY_QUALIFIED_IN_FBCODE_REGEX.captures(s) {
                Ok(BuckRuleParseOutput::FullyQualifiedInFbcode(
                    FbcodeBuckRule {
                        path: TargetsPath::from_buck_rule(&captures[1]),
                        name: captures[2].to_owned(),
                    },
                ))
            } else if let Some(captures) = BUCK_FULLY_QUALIFIED_REGEX.captures(s) {
                Ok(BuckRuleParseOutput::FullyQualified(BuckRule {
                    repo: captures[1].to_owned(),
                    path: Path::new(&captures[2]).to_owned(),
                    name: captures[3].to_owned(),
                }))
            } else if let Some(captures) = BUCK_RULE_NAME_REGEX.captures(s) {
                Ok(BuckRuleParseOutput::RuleName(RuleName {
                    name: captures[1].to_owned(),
                    subtarget: captures.get(2).map(|capture| capture.as_str().to_owned()),
                }))
            } else {
                Err(anyhow!("Failed to parse '{}' as buck rule", s))
            }
        }
    }

    impl<'de> Deserialize<'de> for BuckRuleParseOutput {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let s = String::deserialize(deserializer)?;
            FromStr::from_str(&s).map_err(de::Error::custom)
        }
    }

    impl<'de> Deserialize<'de> for BuckManifestRule {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let rule = BuckRuleParseOutput::deserialize(deserializer)?;
            let result: Result<_> = try {
                let rule = BuckManifestRule(FbcodeBuckRule::try_from(rule).map_err(Error::msg)?);
                if !rule.0.name.ends_with(RUST_MANIFEST_SFX) {
                    Err(anyhow!(
                        "Rust manifest build rule ({:?}) name must end with suffix {}",
                        rule,
                        RUST_MANIFEST_SFX
                    ))?;
                }
                rule
            };
            result.map_err(de::Error::custom)
        }
    }

    impl<'de> Deserialize<'de> for ThriftCratemapRule {
        fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
        where
            D: Deserializer<'de>,
        {
            let rule = BuckRuleParseOutput::deserialize(deserializer)?;
            let mut rule = FbcodeBuckRule::try_from(rule).map_err(de::Error::custom)?;
            if let Some(library_rule_name) = rule.name.strip_suffix(THRIFT_CRATEMAP_SFX) {
                rule.name = library_rule_name.to_owned();
                Ok(ThriftCratemapRule::from_library_rule(rule))
            } else {
                Err(de::Error::custom(format_args!(
                    "Thrift cratemap build rule ({rule}) name must end with suffix {THRIFT_CRATEMAP_SFX}",
                )))
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::fmt::Debug;
    use std::path::Path;

    use assert_matches::assert_matches;
    use serde::Deserialize;
    use serde_json::from_value;
    use serde_json::json;

    use super::*;
    use crate::paths::PathInFbcode;

    #[test]
    fn buck_rule_parse_output_test() {
        assert_matches!(
            from_value::<Vec<BuckRuleParseOutput>>(json!([
                "fbsource//third-party/rust:foo",
                "fbcode//common/rust/foo:bar",
                "//common/rust/biz:baz",
                ":foobar",
                ":foobar[doc]",
            ])),
            Ok(out) => {
                assert_eq!(
                    out,
                    vec![
                        BuckRuleParseOutput::FullyQualified(BuckRule {
                            repo: "fbsource".to_owned(),
                            path: Path::new("third-party/rust").to_owned(),
                            name: "foo".to_owned(),
                        }),
                        BuckRuleParseOutput::FullyQualifiedInFbcode(FbcodeBuckRule {
                            path: TargetsPath::new(PathInFbcode::new_mock("common/rust/foo/TARGETS")).unwrap(),
                            name: "bar".to_owned(),
                        }),
                        BuckRuleParseOutput::FullyQualifiedInFbcode(FbcodeBuckRule {
                            path: TargetsPath::new(PathInFbcode::new_mock("common/rust/biz/TARGETS")).unwrap(),
                            name: "baz".to_owned(),
                        }),
                        BuckRuleParseOutput::RuleName(RuleName {
                            name: "foobar".to_owned(),
                            subtarget: None,
                        }),
                        BuckRuleParseOutput::RuleName(RuleName {
                            name: "foobar".to_owned(),
                            subtarget: Some("doc".to_owned()),
                        }),
                    ]
                )
            }
        );

        assert_matches!(
            ":foobar".parse::<BuckRuleParseOutput>(),
            Ok(out) => {
                assert_eq!(
                    out,
                    BuckRuleParseOutput::RuleName(RuleName {
                        name: "foobar".to_owned(),
                        subtarget: None,
                    }),
                )
            }
        );

        assert_matches!(
            "invalid/rule:name".parse::<BuckRuleParseOutput>(),
            Err(err) => {
                assert_eq!(
                    &format!("{err}"),
                    "Failed to parse 'invalid/rule:name' as buck rule",
                )
            }
        );
    }

    fn rule_test_deserializing<T>(
        from_rule: impl Fn(FbcodeBuckRule) -> T,
        struct_name: &'static str,
        suffix: &'static str,
        dbg_err: &'static str,
    ) where
        T: for<'de> Deserialize<'de> + Debug + PartialEq,
    {
        assert_matches!(
            from_value::<T>(json!(format!("//common/rust/biz:baz{suffix}"))),
            Ok(rule) => {
                assert_eq!(
                    rule,
                    from_rule(FbcodeBuckRule {
                        path: TargetsPath::new(PathInFbcode::new_mock("common/rust/biz/TARGETS")).unwrap(),
                        name: format!("baz{suffix}"),
                    })
                )
            }
        );

        assert_matches!(
            from_value::<T>(json!("//common/rust/biz:baz")),
            Err(err) => {
                assert_eq!(
                    format!("{err}"),
                    format!(
                        "{dbg_err} build rule ({struct_name}(FbcodeBuckRule {{ \
                                path: TargetsPath {{ \
                                    dir: PathInFbcode(\"common/rust/biz\") \
                                }}, \
                                name: \"baz\" \
                        }})) name must end with suffix {suffix}"
                    )
                )
            }
        );

        assert_matches!(
            from_value::<T>(json!("fbsource//third-party/rust:foo")),
            Err(err) => {
                assert_eq!(
                    &format!("{err}"),
                    "Only FullyQualifiedInFbcode can be converted to FbcodeBuckRule",
                )
            }
        );

        assert_matches!(
            from_value::<T>(json!(":foobar")),
            Err(err) => {
                assert_eq!(
                    &format!("{err}"),
                    "Only FullyQualifiedInFbcode can be converted to FbcodeBuckRule",
                )
            }
        );

        assert_matches!(
            from_value::<T>(json!("invalid/rule:name")),
            Err(err) => {
                assert_eq!(
                    &format!("{err}"),
                    "Failed to parse 'invalid/rule:name' as buck rule",
                )
            }
        );
    }

    #[test]
    fn buck_manifest_rule_test_deserializing() {
        if cfg!(windows) {
            return; // Broken on Windows
        }

        rule_test_deserializing(
            BuckManifestRule,
            "BuckManifestRule",
            RUST_MANIFEST_SFX,
            "Rust manifest",
        );
    }
}
