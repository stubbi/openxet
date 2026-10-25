//! The typed views of a repository (ADR 0010 item 4): Hugging Face's repo
//! types are labels, and each typed Hub route sees a repository through
//! one of them.

use openxet_index::Repo;

use crate::HubConfig;

/// The label a route sees a repository through.
///
/// `/api/models/…` and the unprefixed resolve route are the `model` view,
/// as on huggingface.co; `/api/datasets/…` and `datasets/…` the `dataset`
/// view, and so on (`REPO_TYPES_URL_PREFIXES` in `huggingface_hub` 2.2.0).
/// openxet's own addresses, the labels API and the unprefixed Git URL,
/// see every repository whatever its labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    /// The `model` label: `/api/models/…`.
    Model,
    /// The `dataset` label: `/api/datasets/…`, `datasets/…`.
    Dataset,
    /// The `space` label: `/api/spaces/…`, `spaces/…`.
    Space,
    /// The `kernel` label: `/api/kernels/…`, `kernels/…`.
    Kernel,
    /// No label: openxet's own addresses.
    Any,
}

impl View {
    /// The four Hub types, in `huggingface_hub`'s order.
    pub const TYPED: [Self; 4] = [Self::Model, Self::Dataset, Self::Space, Self::Kernel];

    /// The view a `POST /api/repos/create` or `DELETE /api/repos/delete`
    /// body's `type` names. No `type` is the `model` view, as
    /// `create_repo` sends `type` only for other types (`hf_api.py` line
    /// 4641). `None` for a type the Hub doesn't have.
    #[must_use]
    pub fn from_type(kind: Option<&str>) -> Option<Self> {
        match kind {
            None | Some("model") => Some(Self::Model),
            Some("dataset") => Some(Self::Dataset),
            Some("space") => Some(Self::Space),
            Some("kernel") => Some(Self::Kernel),
            Some(_) => None,
        }
    }

    /// The view whose label is `label`, if it is a Hub type's.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::TYPED.into_iter().find(|v| v.label() == Some(label))
    }

    /// The label the view requires; `None` for [`View::Any`].
    #[must_use]
    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::Model => Some("model"),
            Self::Dataset => Some("dataset"),
            Self::Space => Some("space"),
            Self::Kernel => Some("kernel"),
            Self::Any => None,
        }
    }

    /// What the view's repositories are called in messages, as
    /// huggingface.co's own (`a model`, `a dataset`).
    #[must_use]
    pub fn noun(self) -> &'static str {
        self.label().unwrap_or("repository")
    }

    /// The prefix of the view's web, resolve and Git URLs: none for models
    /// and openxet's own addresses, `datasets/`, `spaces/` or `kernels/`.
    #[must_use]
    pub fn url_prefix(self) -> &'static str {
        match self {
            Self::Model | Self::Any => "",
            Self::Dataset => "datasets/",
            Self::Space => "spaces/",
            Self::Kernel => "kernels/",
        }
    }

    /// Whether `repo` is visible through the view: it carries the view's
    /// label, or the view is [`View::Any`].
    #[must_use]
    pub fn admits(self, repo: &Repo) -> bool {
        self.label()
            .is_none_or(|label| repo.labels.iter().any(|l| l == label))
    }

    /// The view of the route whose template is `template`: the typed
    /// prefixes of the Hub API and of resolve URLs, [`View::Any`] for
    /// openxet's own API under `/api/openxet/`, and the `model` view for
    /// every other route, as the unprefixed resolve route is. The Git
    /// routes resolve [`View::Any`] themselves (ADR 0010 item 4).
    pub(crate) fn of_route(template: &str) -> Self {
        const PREFIXES: [(&str, View); 8] = [
            ("/api/models/", View::Model),
            ("/api/datasets/", View::Dataset),
            ("/api/spaces/", View::Space),
            ("/api/kernels/", View::Kernel),
            ("/api/openxet/", View::Any),
            ("/datasets/", View::Dataset),
            ("/spaces/", View::Space),
            ("/kernels/", View::Kernel),
        ];
        PREFIXES
            .iter()
            .find(|(prefix, _)| template.starts_with(prefix))
            .map_or(Self::Model, |(_, view)| *view)
    }

    /// The repository's URL through the view,
    /// `{public_url}/{prefix}{namespace}/{name}`, the form `RepoUrl` parses
    /// against the client's endpoint (`hf_api.py` lines 545–571).
    pub(crate) fn repo_url(self, cfg: &HubConfig, namespace: &str, name: &str) -> String {
        format!(
            "{}/{}{namespace}/{name}",
            cfg.public_url.as_str().trim_end_matches('/'),
            self.url_prefix()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_map_to_their_views() {
        for (template, view) in [
            ("/api/models/{ns}/{name}", View::Model),
            ("/api/datasets/{ns}/{name}/tree/{rev}", View::Dataset),
            ("/api/spaces/{ns}/{name}", View::Space),
            ("/api/kernels/{ns}/{name}", View::Kernel),
            ("/api/openxet/v1/repos/{ns}/{name}/labels", View::Any),
            ("/datasets/{ns}/{name}/resolve/{rev}/{*path}", View::Dataset),
            ("/{ns}/{name}/resolve/{rev}/{*path}", View::Model),
        ] {
            assert_eq!(View::of_route(template), view, "{template}");
        }
    }

    #[test]
    fn types_and_labels_round_trip() {
        assert_eq!(View::from_type(None), Some(View::Model));
        assert_eq!(View::from_type(Some("bucket")), None);
        for view in View::TYPED {
            assert_eq!(View::from_type(view.label()), Some(view));
            assert_eq!(View::from_label(view.noun()), Some(view));
        }
        assert_eq!(View::from_label("checkpoint"), None);
    }
}
