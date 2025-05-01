/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::collections::BTreeMap;
use std::ops::Deref;

use anyhow::Context;
use anyhow::Error;
use anyhow::anyhow;
use cargo_toml::Target;
use serde::Deserialize;
use serde::Deserializer;
use serde::de;
use toml_edit::Key;

/// Like `cargo_toml::TargetDepsSet` (which is just `BTreeMap<String, Target>`),
/// but with keys that are valid single TOML table keys.
pub type KeyedTargetDepsSet = BTreeMap<TargetKey, Target>;

#[derive(Debug, Clone, Hash, Ord, PartialOrd, PartialEq, Eq)]
pub struct TargetKey(Key);

impl TryFrom<&str> for TargetKey {
    type Error = Error;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        let mut keys = Key::parse(s).context("Failed target key parsing")?;

        let key = keys
            .pop()
            .ok_or_else(|| anyhow!("Expected exactly one target key, found none"))?;

        if keys.is_empty() {
            Ok(TargetKey(key))
        } else {
            Err(anyhow!("Expected exactly one target key, found more"))
        }
    }
}

impl<'de> Deserialize<'de> for TargetKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        TargetKey::try_from(s.as_str()).map_err(de::Error::custom)
    }
}

impl Deref for TargetKey {
    type Target = Key;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod test {
    use assert_matches::assert_matches;

    use super::*;

    #[test]
    fn target_key_test_invalid_keys() {
        #[track_caller]
        fn t_err(s: &str, err: &str) {
            assert_matches!(
                TargetKey::try_from(s),
                Err(e) if e.to_string() == err,
                "TargetKey::try_from({s:?})",
            );
        }

        t_err("cfg(target_os = \"linux\")", "Failed target key parsing");
        t_err("cfg(target_os = 'linux')", "Failed target key parsing");
        t_err("cfg(windows)", "Failed target key parsing");
        t_err(
            "'cfg(windows)'.dependencies",
            "Expected exactly one target key, found more",
        );
        t_err(
            r#"'cfg(target_os = "linux")'.dependencies"#,
            "Expected exactly one target key, found more",
        );
    }
}
