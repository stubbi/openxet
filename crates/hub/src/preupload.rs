//! `POST /api/{type}s/{id}/preupload/{rev}`: whether each file of a commit
//! travels inside it or through Xet (ruling M2-R4, S3 answer 1).
//!
//! `"uploadMode": "lfs"` is the server's only lever: `huggingface_hub`
//! then uploads the file through Xet whenever `hf_xet` is installed
//! (`_commit_api.py` line 396). A file goes through Xet when it is at least
//! `hub.lfs_threshold` bytes, or its path matches a `filter=lfs` pattern of
//! the target revision's `.gitattributes`: the generated one for a new
//! repository (ruling M2-R5).

use axum::Json;
use axum::body::Body;
use axum::extract::{RawPathParams, State};
use axum::http::Method;
use bstr::{BStr, ByteSlice as _};
use gix_attributes::StateRef;
use gix_attributes::glob::Pattern;
use gix_attributes::glob::pattern::Case;
use gix_attributes::glob::wildmatch::Mode as MatchMode;
use gix_attributes::parse::{Kind, Lines};
use openxet_auth::Action;
use openxet_git::{Blob, GitError};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::authz::{self, RepoAccess};
use crate::refs::at_revision;
use crate::routes::{Routes, on};
use crate::{HubError, HubState, param, parse_json, read_body};

/// huggingface.co's default `.gitattributes`, which a new repository's
/// initial commit holds (ruling M2-R5): the S3 capture's content, byte for
/// byte (blob `a6344aac…`, S3 fixtures `16`, `29-…-08`).
pub const DEFAULT_GITATTRIBUTES: &[u8] = include_bytes!("default.gitattributes");

/// The largest preupload body: `huggingface_hub` asks about at most 256
/// files at a time, each with a 512-byte sample in base64 (`_commit_api.py`
/// line 748), a few hundred KiB; this leaves ample room for long paths.
const PREUPLOAD_BODY_LIMIT: usize = 4 << 20;

/// The preupload route of every Hub type.
pub(crate) fn routes() -> Routes {
    let mut routes = Routes::new();
    for path in typed_paths!("/{ns}/{name}/preupload/{rev}") {
        routes = routes.route(path, on(Method::POST, preupload).private());
    }
    routes
}

/// `POST …/preupload/{rev}`'s body (`_commit_api.py` line 753).
#[derive(Deserialize)]
struct PreuploadBody {
    files: Vec<FileInfo>,
}

#[derive(Deserialize)]
struct FileInfo {
    path: String,
    size: u64,
}

/// `POST …/preupload/{rev}`: `200 {"files":[{"path","uploadMode",
/// "shouldIgnore"}],"commitOid"}` (S3 fixture `09-…-02`), files in request
/// order. Nothing is ignored. Needs `write`, as the commit it prepares does.
async fn preupload(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
    body: Body,
) -> Result<Json<Value>, HubError> {
    authz::require(&state, &access.who, Action::Write, &access.repo).await?;
    let body: PreuploadBody = parse_json(&read_body(body, PREUPLOAD_BODY_LIMIT).await?)?;
    let rev = param(&params, "rev");
    let snap = state.engine.snapshot(access.repo.id).await?;
    // A repository without refs gets its first commit from this upload: its
    // `.gitattributes` is the one `create_repo` would have written.
    let (patterns, commit) = if snap.refs.is_empty() {
        (LfsPatterns::parse(DEFAULT_GITATTRIBUTES), None)
    } else {
        drop(snap);
        let (snap, resolved) = at_revision(&state, &access, &rev).await?;
        let patterns = match state
            .engine
            .read_blob(&snap, resolved.commit, ".gitattributes")
            .await
        {
            Ok(Blob::Inline { bytes, .. }) => LfsPatterns::parse(&bytes),
            Ok(Blob::Lfs { .. }) | Err(GitError::EntryNotFound { .. }) => LfsPatterns::default(),
            Err(e) => return Err(e.into()),
        };
        (patterns, Some(resolved.commit))
    };
    let files: Vec<Value> = body
        .files
        .iter()
        .map(|file| {
            let lfs = file.size > 0
                && (file.size >= state.cfg.lfs_threshold || patterns.matches(&file.path));
            json!({
                "path": file.path,
                "uploadMode": if lfs { "lfs" } else { "regular" },
                "shouldIgnore": false,
            })
        })
        .collect();
    Ok(Json(json!({
        "files": files,
        "commitOid": commit.map(|c| c.to_string()),
    })))
}

/// The patterns of a `.gitattributes` and whether each sets `filter=lfs`,
/// in file order: as in git, the last line matching a path decides.
#[derive(Debug, Default)]
pub(crate) struct LfsPatterns(Vec<(Pattern, bool)>);

impl LfsPatterns {
    /// Reads `text`'s pattern lines that assign `filter`. Macros and lines
    /// git would refuse are skipped, as git skips them.
    pub(crate) fn parse(text: &[u8]) -> Self {
        let mut patterns = Vec::new();
        for line in Lines::new(text) {
            let Ok((Kind::Pattern(pattern), assignments, _)) = line else {
                continue;
            };
            let filter = assignments
                .filter_map(Result::ok)
                .filter(|a| a.name.as_str() == "filter")
                .last();
            if let Some(filter) = filter {
                let lfs = matches!(filter.state, StateRef::Value(v) if v.as_bstr() == "lfs");
                patterns.push((pattern, lfs));
            }
        }
        Self(patterns)
    }

    /// Whether the last pattern matching `path`, relative to the root,
    /// sets `filter=lfs`.
    pub(crate) fn matches(&self, path: &str) -> bool {
        let path = path.trim_start_matches('/');
        let path: &BStr = path.as_bytes().as_bstr();
        let basename = path.rfind_byte(b'/').map(|at| at + 1);
        self.0
            .iter()
            .rev()
            .find(|(pattern, _)| {
                pattern.matches_repo_relative_path(
                    path,
                    basename,
                    Some(false),
                    Case::Sensitive,
                    MatchMode::NO_MATCH_SLASH_LITERAL,
                )
            })
            .is_some_and(|(_, lfs)| *lfs)
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_GITATTRIBUTES, LfsPatterns};

    #[test]
    fn default_patterns_route_as_git_lfs_would() {
        let patterns = LfsPatterns::parse(DEFAULT_GITATTRIBUTES);
        for path in [
            "big.bin",
            "sub/model.safetensors",
            "x.tar.gz",
            "a.lfs.b",
            "saved_model/v1/vars.data",
            "logs/events.out.tfevents.1",
        ] {
            assert!(patterns.matches(path), "{path}");
        }
        for path in [
            "README.md",
            "config.json",
            "bin",
            "saved_model",
            "sub/saved_model/x",
        ] {
            assert!(!patterns.matches(path), "{path}");
        }
    }

    #[test]
    fn the_last_matching_line_decides() {
        let patterns = LfsPatterns::parse(
            b"*.txt filter=lfs\nkeep.txt -filter\n# *.md filter=lfs\n\"q a.md\" filter=lfs\n\
              [attr]binary -diff\n*.json text\n",
        );
        assert!(patterns.matches("a.txt"));
        assert!(!patterns.matches("keep.txt"));
        assert!(!patterns.matches("x.md"));
        assert!(patterns.matches("q a.md"));
        assert!(!patterns.matches("c.json"));
        assert!(!LfsPatterns::parse(b"*.txt filter=other\n").matches("a.txt"));
    }
}
