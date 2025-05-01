// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

//! Provides Path-wrappers that make it easier to distinguish between different
//! types of paths in code (like paths to Cargo.toml files, paths to TARGETS,
//! root of repository).

use std::collections::HashSet;
use std::env::current_dir;
use std::fmt;
use std::fmt::Display;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::ensure;
use derive_more::AsRef;
use futures::TryStreamExt;
use futures::stream::FuturesUnordered;
use serde::Deserialize;
use tokio::fs::canonicalize;
use tokio::fs::read_to_string;

/// Parses provided paths, and makes them relative to root of fbcode.
pub async fn process_input_paths<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    fbcode_root: &FbcodeRoot,
) -> Result<Vec<PathInFbcode>> {
    paths
        .into_iter()
        .map(|p| async move {
            canonicalize(p)
                .await
                .with_context(|| format!("Failed to canonicalize path '{p}'"))
        })
        .collect::<FuturesUnordered<_>>()
        .try_collect::<HashSet<_>>()
        .await
        .with_context(|| {
            format!(
                "While canonicalizing input paths, current working dir is: {:?}",
                current_dir()
            )
        })?
        .into_iter()
        .map(|p| PathInFbcode::from_absolute(fbcode_root, p))
        .collect::<Result<Vec<_>, _>>()
}

/// Wrapper for PathBuf that holds absolute path to root of fbsource.
#[derive(Debug, Clone, AsRef)]
#[as_ref(forward)]
pub struct FbsourceRoot(PathBuf);
/// Wrapper for PathBuf that holds absolute path to root of fbcode.
#[derive(Debug, Clone, AsRef)]
#[as_ref(forward)]
pub struct FbcodeRoot(PathBuf);

impl FbsourceRoot {
    /// Looks for root of fbsource starting with current working directory up.
    pub async fn new() -> Result<Self> {
        let mut path = canonicalize(current_dir().context("While getting CWD")?)
            .await
            .context("While canonicalizing CWD")?;
        let cwd = path.to_string_lossy().into_owned();

        while path.parent().is_some() {
            if let Ok(content) = read_to_string(&path.join(".projectid")).await {
                if content.trim() == "fbsource" {
                    return Ok(Self(path));
                }
            }
            path.pop();
        }

        Err(anyhow!(
            "Couldn't find fbsource root while traversing {}",
            cwd
        ))
    }
}

impl FbcodeRoot {
    /// Directory name under fbsource where fbcode is.
    pub const fn dirname() -> &'static str {
        "fbcode"
    }

    #[cfg(test)]
    pub fn new_mock(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }
}

impl From<FbcodeRoot> for FbsourceRoot {
    fn from(mut this: FbcodeRoot) -> Self {
        this.0.pop();
        Self(this.0)
    }
}

impl From<FbsourceRoot> for FbcodeRoot {
    fn from(mut this: FbsourceRoot) -> Self {
        this.0.push(Self::dirname());
        Self(this.0)
    }
}

/// This is the path of the Rust vendor sources relative to FbsourceRoot.
pub const RUST_VENDOR_STR: &str = "third-party/rust/vendor";

/// Wrapper for PathBuf that holds path relative to root of fbcode which also
/// is inside of fbcode.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, AsRef, Deserialize)]
#[serde(transparent)]
pub struct PathInFbcode(PathBuf);

impl PathInFbcode {
    /// Filename of the build file used by generated from thrift Cargo.toml.
    pub const fn thrift_build_filename() -> &'static str {
        "thrift_build.rs"
    }

    /// Filename of the lib file used by generated from thrift Cargo.toml.
    pub const fn thrift_lib_filename() -> &'static str {
        "thrift_lib.rs"
    }

    /// List of all additional filenames that autocargo generates (excluding
    /// Cargo.toml).
    pub fn all_additional_filenames() -> Vec<&'static str> {
        vec![Self::thrift_build_filename(), Self::thrift_lib_filename()]
    }

    /// Given root of fbcode and an absolute path in fbcode computes path
    /// relative to fbcode.
    pub fn from_absolute(root: &FbcodeRoot, path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        ensure!(
            path.is_absolute(),
            "Provided path {} is not absolute",
            path.display()
        );
        let rel_path = path.strip_prefix(&root.0).with_context(|| {
            format!(
                "Failed to create PathInFbcode: {} is not inside {:?}",
                path.display(),
                root
            )
        })?;
        Ok(Self(rel_path.to_path_buf()))
    }

    #[cfg(test)]
    pub fn new_mock(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    /// Join path relative to folder containing this TARGETS file to get a path
    /// relative in fbcode. Handles "./" and "../" without inspecting filesystem,
    /// so it doesn't handle symlinks on the way if there are any.
    pub fn join_to_path_in_fbcode(&self, path: impl AsRef<Path>) -> PathInFbcode {
        let mut path_in_fbcode = self.0.clone();
        for component in path.as_ref().components() {
            match component {
                Component::Normal(_) | Component::RootDir | Component::Prefix(_) => {
                    path_in_fbcode.push(component);
                }
                Component::ParentDir => match path_in_fbcode.components().next_back() {
                    Some(Component::Normal(_)) => {
                        path_in_fbcode.pop();
                    }
                    Some(Component::ParentDir) | None => {
                        path_in_fbcode.push(Component::ParentDir);
                    }
                    Some(Component::RootDir | Component::Prefix(_)) => {}
                    Some(Component::CurDir) => unreachable!(),
                },
                Component::CurDir => {}
            }
        }
        PathInFbcode(path_in_fbcode)
    }
}

impl Display for PathInFbcode {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        self.as_ref().display().fmt(formatter)
    }
}

/// Wrapper for PathBuf that holds path to Cargo.toml file relative to fbcode.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, AsRef)]
pub struct CargoTomlPath {
    #[as_ref]
    file: PathInFbcode,
    dir: PathInFbcode,
}

impl CargoTomlPath {
    /// Filename that this path must point to.
    pub const fn filename() -> &'static str {
        "Cargo.toml"
    }

    /// Given a path relative in fbcode wrap it.
    pub fn new(path: PathInFbcode) -> Result<Self> {
        ensure!(
            path.0.ends_with(Self::filename()),
            "Provided path {} does not point to {} file",
            path.0.display(),
            Self::filename(),
        );
        Ok(Self {
            dir: PathInFbcode(path.0.parent().unwrap().to_owned()),
            file: path,
        })
    }

    /// Ref path to this file.
    pub fn as_file(&self) -> &PathInFbcode {
        &self.file
    }

    /// Ref path to folder containing this file.
    pub fn as_dir(&self) -> &PathInFbcode {
        &self.dir
    }
}

/// Wrapper for PathBuf that holds path to TARGETS file relative to fbcode.
#[derive(Debug, Clone, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub struct TargetsPath {
    dir: PathInFbcode,
}

impl TargetsPath {
    /// Return all the possible valid filenames for Buck targets that are supported by autocargo.
    pub const fn filenames() -> &'static [&'static str] {
        &["TARGETS", "BUCK", "TARGETS.v2", "BUCK.v2"]
    }

    /// Check if the path provided ends with one of the matching filenames from
    /// Self::filenames().
    pub fn matches_path(path: &Path) -> bool {
        Self::filenames().iter().any(|name| path.ends_with(name))
    }

    /// Given a path relative in fbcode wrap it.
    pub fn new(path: PathInFbcode) -> Result<Self> {
        let dir = if !path.0.is_dir() {
            ensure!(
                Self::filenames().iter().any(|name| path.0.ends_with(name)),
                "Provided path {} does not point to a valid BUCK file",
                path.0.display(),
            );
            PathInFbcode(path.0.parent().unwrap().to_owned())
        } else {
            path
        };

        Ok(Self { dir })
    }

    /// This constructor is for building TargetsPath directly from PathBuf,
    /// use it only when the provided path comes from a fully qualified buck
    /// rule. This path should point to the directory, not the build file.
    pub fn from_buck_rule(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        Self {
            dir: PathInFbcode(path.to_owned()),
        }
    }

    /// Returns a path in Fbcode to a BUCK file that would correspond to the
    /// given target. Note that this BUCK file may not actually exist.
    pub fn as_buck_path(&self) -> PathInFbcode {
        PathInFbcode(self.dir.0.join("BUCK"))
    }

    /// Ref path to folder containing this file.
    pub fn as_dir(&self) -> &PathInFbcode {
        &self.dir
    }
}

#[cfg(test)]
mod test {
    use quickcheck_macros::quickcheck;

    use super::*;

    #[quickcheck]
    fn fbsource_roots_roundtrip_test(path: PathBuf) -> bool {
        let fbsource = FbsourceRoot(path);
        let fbcode: FbcodeRoot = fbsource.clone().into();
        let fbsource_again: FbsourceRoot = fbcode.clone().into();
        let fbcode_again: FbcodeRoot = fbsource_again.clone().into();
        fbsource_again.0 == fbsource.0 && fbcode_again.0 == fbcode.0
    }

    #[test]
    fn path_in_fbcode_test() {
        if cfg!(windows) {
            return; // Broken on Windows
        }

        let fbcode = FbcodeRoot(PathBuf::from("/a/b/c".to_owned()));
        let p = PathBuf::from("/a/b/c/d/e/f".to_owned());
        assert_eq!(
            PathInFbcode::from_absolute(&fbcode, p).unwrap().0,
            PathBuf::from("d/e/f".to_owned())
        );

        let p = PathBuf::from("c/d/e/f".to_owned());
        PathInFbcode::from_absolute(&fbcode, p).unwrap_err();

        let p = PathBuf::from("/a2/b/c/d/e/f".to_owned());
        PathInFbcode::from_absolute(&fbcode, p).unwrap_err();
    }

    #[test]
    fn targets_path_join_to_dir() {
        let paths = [
            "foo/bar/biz/TARGETS",
            "foo/bar/biz/TARGETS.v2",
            "foo/bar/biz/BUCK",
            "foo/bar/biz/BUCK.v2",
        ];
        for path in paths {
            let targets_path = TargetsPath::new(PathInFbcode::new_mock(path)).unwrap();

            assert_eq!(
                targets_path.as_dir().join_to_path_in_fbcode("file.rs"),
                PathInFbcode::new_mock("foo/bar/biz/file.rs")
            );

            assert_eq!(
                targets_path
                    .as_dir()
                    .join_to_path_in_fbcode("./fiz/file.rs"),
                PathInFbcode::new_mock("foo/bar/biz/fiz/file.rs")
            );

            assert_eq!(
                targets_path
                    .as_dir()
                    .join_to_path_in_fbcode("../fiz/file.rs"),
                PathInFbcode::new_mock("foo/bar/fiz/file.rs")
            );

            assert_eq!(
                targets_path
                    .as_dir()
                    .join_to_path_in_fbcode("../../../../fiz/file.rs"),
                PathInFbcode::new_mock("..//fiz/file.rs")
            );

            assert_eq!(
                targets_path
                    .as_dir()
                    .join_to_path_in_fbcode("fiz/../file.rs"),
                PathInFbcode::new_mock("foo/bar/biz/file.rs")
            );
        }
    }
}
