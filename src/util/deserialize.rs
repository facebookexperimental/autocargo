/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::collections::HashSet;

use glob::Pattern;
use serde::de::Deserialize;
use serde::de::Deserializer;
use serde::de::Error;

pub fn deserialize_globs<'de, D>(deserializer: D) -> Result<HashSet<Pattern>, D::Error>
where
    D: Deserializer<'de>,
{
    let input: Vec<String> = Deserialize::deserialize(deserializer)?;
    input
        .into_iter()
        .map(|s| {
            if is_target_like(&s) {
                Err(Error::custom(format!(
                    "expected path glob but `{s}` looks like a buck target"
                )))
            } else {
                Pattern::new(&s).map_err(Error::custom)
            }
        })
        .collect()
}

fn is_target_like(s: &str) -> bool {
    if let Some((_head, tail)) = s.rsplit_once('/') {
        if tail == "..." || tail.contains(':') {
            return true;
        }
    }
    if s.contains("//") {
        return true;
    }
    false
}

#[cfg(test)]
mod test {
    use maplit::hashset;
    use serde::Deserialize;
    use serde_json::from_value;
    use serde_json::json;

    use super::*;

    #[derive(Debug, Eq, PartialEq, Deserialize)]
    struct TestData {
        #[serde(deserialize_with = "deserialize_globs")]
        globs: HashSet<Pattern>,
    }

    #[test]
    fn invalid_globs() {
        let json = json!({
            "globs": [
                "**/file1",
                "in**valid_dir1/*",
                "/dir2/dir3/file3",
            ]
        });
        assert!(from_value::<TestData>(json).unwrap_err().is_data());
    }

    #[test]
    fn valid_globs() {
        let json = json!({
            "globs": [
                "**/file1",
                "dir1/*",
                "/dir2/dir3/file3",
            ]
        });
        assert_eq!(
            from_value::<TestData>(json).unwrap().globs,
            hashset![
                Pattern::new("**/file1").unwrap(),
                Pattern::new("dir1/*").unwrap(),
                Pattern::new("/dir2/dir3/file3").unwrap(),
            ]
        );
    }

    #[test]
    fn target_like_globs() {
        assert!(!is_target_like("foo/bar"));
        assert!(is_target_like("foo/bar:"));
        assert!(is_target_like("foo//bar/"));
        assert!(is_target_like("foo/..."));
    }
}
