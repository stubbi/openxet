//! Which paths of a workspace are reconstructible, per spec 2026-10-27
//! §6.2a: tracked files and untracked files Git doesn't ignore are durable,
//! and an ignored path is reconstructible only when a preset names it.
//! Nothing is reconstructible by guess, so every unclear case is durable.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::{WorkspaceError, git};

/// The project config file, relative to the workspace root.
pub const PROJECT_CONFIG: &str = ".openxet/workspace.toml";

/// What losing a path costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Work that exists nowhere else: never evicted.
    Durable,
    /// Build outputs and installed packages: costs a rebuild or a download,
    /// so it may be deduplicated and evicted.
    Reconstructible,
}

/// The directories, relative to a workspace root, whose ignored contents are
/// reconstructible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presets {
    dirs: Vec<PathBuf>,
}

impl Default for Presets {
    /// Cargo's `target`, npm's and pnpm's `node_modules` and Python's
    /// `.venv`, at the workspace root.
    fn default() -> Self {
        Self {
            dirs: ["target", "node_modules", ".venv"]
                .into_iter()
                .map(PathBuf::from)
                .collect(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectConfig {
    #[serde(default)]
    reconstructible: Vec<String>,
}

impl Presets {
    /// The defaults, extended by the `reconstructible = ["..."]` list in
    /// `root`'s [`PROJECT_CONFIG`] if it exists.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Config`] if the file is not valid TOML, has an
    /// unknown key, or lists a path that is empty, absolute, leaves the
    /// workspace or is inside `.git` or `.openxet`; [`WorkspaceError::Io`] if
    /// it exists but can't be read.
    pub fn load(root: &Path) -> Result<Self, WorkspaceError> {
        let mut presets = Self::default();
        let path = root.join(PROJECT_CONFIG);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(presets),
            Err(err) => return Err(WorkspaceError::io(path)(err)),
        };
        let invalid = |message: String| WorkspaceError::Config {
            path: path.clone(),
            message,
        };
        let config: ProjectConfig = toml::from_str(&text).map_err(|err| {
            invalid(format!(
                "{}; the only key is `reconstructible = [\"dir\", ...]`",
                err.message()
            ))
        })?;
        for entry in config.reconstructible {
            let dir = relative_dir(&entry).ok_or_else(|| {
                invalid(format!(
                    "`reconstructible` entry {entry:?} must be a directory relative to the \
                     workspace root, without `..`, and outside `.git` and `.openxet`"
                ))
            })?;
            if !presets.dirs.contains(&dir) {
                presets.dirs.push(dir);
            }
        }
        Ok(presets)
    }

    /// The preset directories, relative to the workspace root.
    #[must_use]
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }
}

/// `entry` as a path of plain components, or `None` if it could reach
/// outside the workspace or into Git's or OpenXet's own files.
fn relative_dir(entry: &str) -> Option<PathBuf> {
    let mut dir = PathBuf::new();
    for component in Path::new(entry).components() {
        match component {
            Component::Normal(part) => dir.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    let first = dir.components().next()?;
    if [".git", ".openxet"].contains(&first.as_os_str().to_str()?) {
        return None;
    }
    Some(dir)
}

/// Classifies `path`, relative to the workspace `root`.
///
/// It is [`Class::Reconstructible`] only when it is under a preset directory
/// and Git ignores it; a tracked file is never ignored, even under a
/// preset (`git check-ignore` without `--no-index`). A path that is absolute
/// or has `..` is durable.
///
/// # Errors
///
/// [`WorkspaceError::NotARepository`] if `root` is not in a Git repository
/// and [`WorkspaceError::Git`] if Git fails.
pub fn classify(root: &Path, path: &Path, presets: &Presets) -> Result<Class, WorkspaceError> {
    let Some(relative) = path.to_str().and_then(relative_dir) else {
        return Ok(Class::Durable);
    };
    if !presets.dirs.iter().any(|dir| relative.starts_with(dir)) {
        return Ok(Class::Durable);
    }
    let Some(relative) = relative.to_str() else {
        return Ok(Class::Durable);
    };
    if is_ignored(root, relative)? {
        Ok(Class::Reconstructible)
    } else {
        Ok(Class::Durable)
    }
}

/// The preset directories that exist under `root` and can be evicted as a
/// whole: real directories (no symlink on the way, so eviction stays inside
/// the workspace), ignored by Git, and holding no tracked file. A directory
/// Git can't answer for is left out.
#[must_use]
pub fn reconstructible_dirs(root: &Path, presets: &Presets) -> Vec<PathBuf> {
    presets
        .dirs
        .iter()
        .filter(|dir| is_real_dir(root, dir) && is_evictable(root, dir))
        .map(|dir| root.join(dir))
        .collect()
}

fn is_real_dir(root: &Path, dir: &Path) -> bool {
    let mut path = root.to_path_buf();
    dir.components().all(|component| {
        path.push(component);
        std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir())
    })
}

fn is_evictable(root: &Path, dir: &Path) -> bool {
    let Some(dir) = dir.to_str() else {
        return false;
    };
    // The trailing slash makes `git check-ignore` match directory-only
    // patterns such as `target/`.
    if !is_ignored(root, &format!("{dir}/")).unwrap_or(false) {
        return false;
    }
    let args = ["ls-files", "-z", "--", dir];
    git::run(root, &args).is_ok_and(|output| output.status.success() && output.stdout.is_empty())
}

fn is_ignored(root: &Path, relative: &str) -> Result<bool, WorkspaceError> {
    let args = ["check-ignore", "-q", "--", relative];
    let output = git::run(root, &args)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ if String::from_utf8_lossy(&output.stderr).contains("not a git repository") => {
            Err(WorkspaceError::NotARepository {
                path: root.to_path_buf(),
            })
        }
        _ => Err(git::failed(&args, &output)),
    }
}
