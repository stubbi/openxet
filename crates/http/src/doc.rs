//! The OpenAPI 3.1 description of every route (ruling M2-R15).
//!
//! Each route is described where it is built: [`crate::on`] takes a [`Doc`]
//! beside each handler, and [`crate::Routes`] files it under the route's
//! path and method. The document `openxet openapi` prints therefore lists
//! exactly what the server serves, and a route can't be added without one.
//!
//! The model is `utoipa`'s (<https://docs.rs/utoipa>), built at runtime
//! rather than from its derive macros: handlers are often closures, and
//! one handler serves the path of each Hub type (`/api/models/…`,
//! `/api/datasets/…`), which a path fixed in an attribute can't express.
//! The same approach as `utoipa-axum`'s router, which also collects each
//! route's operation where the route is added.

use std::collections::BTreeMap;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use utoipa::openapi::header::Header;
use utoipa::openapi::path::{HttpMethod, Operation, Parameter, ParameterIn};
use utoipa::openapi::request_body::RequestBody;
use utoipa::openapi::response::Response;
use utoipa::openapi::security::{
    ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityRequirement, SecurityScheme,
};
use utoipa::openapi::tag::Tag;
use utoipa::openapi::{
    Components, Content, Info, License, OpenApi, Paths, Ref, RefOr, Required, Schema,
};

use crate::SCHEMA;

/// The part of the server a route belongs to: its tag in the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Area {
    /// The liveness and readiness probes.
    Health,
    /// The Xet CAS API.
    Cas,
    /// The Hub API stock Hugging Face clients call.
    Hub,
    /// OpenXet's own repository API, under `/api/openxet/v1`.
    Openxet,
    /// Git smart HTTP.
    Git,
    /// The Git LFS endpoint and its capability URLs.
    Lfs,
    /// The HTTP file gateway.
    Gateway,
    /// The OpenAPI document itself.
    Meta,
}

impl Area {
    const ALL: [Self; 8] = [
        Self::Health,
        Self::Cas,
        Self::Hub,
        Self::Openxet,
        Self::Git,
        Self::Lfs,
        Self::Gateway,
        Self::Meta,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Health => "health",
            Self::Cas => "cas",
            Self::Hub => "hub",
            Self::Openxet => "openxet",
            Self::Git => "git",
            Self::Lfs => "lfs",
            Self::Gateway => "gateway",
            Self::Meta => "meta",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Health => "Liveness and readiness probes, without a credential.",
            Self::Cas => {
                "The Xet CAS API as stock clients call it \
                 (https://huggingface.co/docs/xet/api). Routes: docs/reference/routes.md."
            }
            Self::Hub => {
                "The Hugging Face Hub API that `huggingface_hub`, `hf` and \
                 `huggingface.js` call. Each repository route is served under \
                 `/api/models`, `/api/datasets`, `/api/spaces` and `/api/kernels`, \
                 which see a repository through that label (ADR 0010)."
            }
            Self::Openxet => "OpenXet's own repository API: labels.",
            Self::Git => {
                "Git smart HTTP, protocol v2 and v0. `{name}` may end in `.git`. \
                 The dumb protocol is not served."
            }
            Self::Lfs => {
                "The Git LFS batch API with the `xet` transfer, and the capability \
                 URLs `git-xet` refreshes its CAS token from. Routes: docs/reference/lfs.md."
            }
            Self::Gateway => "Plain HTTP reads of a repository's Xet files, with `Range`.",
            Self::Meta => "This document.",
        }
    }
}

/// What a route takes as its credential: the operation's security
/// requirement. Names no value: a credential is never part of the
/// document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    /// None: the probes and this document.
    None,
    /// A static token or a CAS token as `Authorization: Bearer`.
    CasToken,
    /// An OpenXet access token as `Authorization: Bearer`, or none for
    /// what the operator's `anonymous` rules allow.
    AccessToken,
    /// As [`Credential::AccessToken`], or the token as the password of
    /// HTTP Basic, as git and `git-lfs` send it.
    AccessTokenOrBasic,
    /// A URL the server signed: its query is the credential, and no
    /// `Authorization` header is read.
    SignedUrl,
}

impl Credential {
    fn requirements(self) -> Vec<SecurityRequirement> {
        let scheme = |name: &str| SecurityRequirement::new(name, Vec::<String>::new());
        match self {
            Self::None => Vec::new(),
            Self::CasToken => vec![scheme(CAS_TOKEN)],
            Self::AccessToken => vec![scheme(ACCESS_TOKEN), SecurityRequirement::default()],
            Self::AccessTokenOrBasic => vec![
                scheme(ACCESS_TOKEN),
                scheme(ACCESS_TOKEN_BASIC),
                SecurityRequirement::default(),
            ],
            Self::SignedUrl => vec![scheme(SIGNED_URL)],
        }
    }
}

const CAS_TOKEN: &str = "casToken";
const ACCESS_TOKEN: &str = "accessToken";
const ACCESS_TOKEN_BASIC: &str = "accessTokenBasic";
const SIGNED_URL: &str = "signedUrl";

/// The error body a route answers its errors with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorShape {
    /// [`crate::ErrorBody`].
    Plain,
    /// [`crate::HubErrorBody`].
    Hub,
    /// Git's: the Hub's status and headers, with the message and the hint
    /// as text, which git prints as `remote:` lines.
    Git,
}

/// One answer of an operation.
#[derive(Clone)]
struct Answer {
    /// Whether it carries the error body.
    error: bool,
    description: String,
    content: Vec<(&'static str, Value)>,
    headers: Vec<(&'static str, &'static str)>,
}

/// The description of one operation: what it does, what it takes, and
/// every status it answers. Every `4xx` and `5xx` answers with the error
/// body, so an error is described by its status and the `code`s it
/// carries.
///
/// Every operation can answer `500 internal` (a server bug) and
/// `503 not_ready` (while the server starts), which a new `Doc` already
/// holds; [`Doc::error`] with either status adds to the description.
#[derive(Clone)]
pub struct Doc {
    area: Area,
    summary: &'static str,
    description: Option<&'static str>,
    credential: Credential,
    errors: ErrorShape,
    parameters: Vec<Parameter>,
    body: Option<RequestBody>,
    answers: BTreeMap<u16, Answer>,
}

impl Doc {
    /// An operation of `area` that does `summary`, taking `credential`.
    #[must_use]
    pub fn new(area: Area, summary: &'static str, credential: Credential) -> Self {
        let mut doc = Self {
            area,
            summary,
            description: None,
            credential,
            errors: ErrorShape::Plain,
            parameters: Vec::new(),
            body: None,
            answers: BTreeMap::new(),
        };
        doc = doc.error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "`internal`: a server-side failure, logged under the `X-Request-Id`.",
        );
        doc.error(
            StatusCode::SERVICE_UNAVAILABLE,
            "`not_ready`: the server is starting. `Retry-After: 1`.",
        )
    }

    /// Answers errors with the Hub's error body, which adds huggingface.co's
    /// `error` field and the `X-Error-Code` and `X-Error-Message` headers
    /// (ruling M2-R6).
    #[must_use]
    pub fn hub_errors(mut self) -> Self {
        self.errors = ErrorShape::Hub;
        self
    }

    /// Answers errors as Git's smart HTTP routes do: the Hub's status and
    /// headers, with the message and the hint as `text/plain`.
    #[must_use]
    pub fn git_errors(mut self) -> Self {
        self.errors = ErrorShape::Git;
        self
    }

    /// More than the summary says, in Markdown.
    #[must_use]
    pub fn describe(mut self, description: &'static str) -> Self {
        self.description = Some(description);
        self
    }

    /// A query parameter of `schema`.
    #[must_use]
    pub fn query(
        self,
        name: &'static str,
        required: bool,
        schema: &Value,
        description: &'static str,
    ) -> Self {
        self.parameter(ParameterIn::Query, name, required, schema, description)
    }

    /// A request header, a string.
    #[must_use]
    pub fn header(self, name: &'static str, required: bool, description: &'static str) -> Self {
        self.parameter(
            ParameterIn::Header,
            name,
            required,
            &json!({"type": "string"}),
            description,
        )
    }

    fn parameter(
        mut self,
        location: ParameterIn,
        name: &'static str,
        required: bool,
        schema: &Value,
        description: &'static str,
    ) -> Self {
        self.parameters.push(
            Parameter::builder()
                .name(name)
                .parameter_in(location)
                .required(if required {
                    Required::True
                } else {
                    Required::False
                })
                .description(Some(description))
                .schema(Some(schema_of(schema)))
                .build(),
        );
        self
    }

    /// The request body: `media_type` of `schema`.
    #[must_use]
    pub fn body(mut self, media_type: &str, schema: &Value, description: &'static str) -> Self {
        self.body = Some(
            RequestBody::builder()
                .description(Some(description))
                .required(Some(Required::True))
                .content(media_type, Content::new(Some(schema_of(schema))))
                .build(),
        );
        self
    }

    /// An answer other than an error body: a success, or a refusal HTTP
    /// defines without one (`416` on a file read), with no body or
    /// `media_type` of `schema`.
    #[must_use]
    pub fn answer(
        mut self,
        status: StatusCode,
        description: &str,
        content: Option<(&'static str, Value)>,
    ) -> Self {
        self.answers.insert(
            status.as_u16(),
            Answer {
                error: false,
                description: description.to_owned(),
                content: content.into_iter().collect(),
                headers: Vec::new(),
            },
        );
        self
    }

    /// Another media type the answer `status` may have, of `schema`.
    ///
    /// # Panics
    ///
    /// If no answer `status` is described yet. Routes are built at startup,
    /// and every test builds them.
    #[must_use]
    pub fn answer_media(
        mut self,
        status: StatusCode,
        media_type: &'static str,
        schema: Value,
    ) -> Self {
        self.answers
            .get_mut(&status.as_u16())
            .unwrap_or_else(|| panic!("no answer {status} to add {media_type} to"))
            .content
            .push((media_type, schema));
        self
    }

    /// A header the answer `status` carries.
    ///
    /// # Panics
    ///
    /// If no answer `status` is described yet. Routes are built at startup,
    /// and every test builds them.
    #[must_use]
    pub fn answer_header(
        mut self,
        status: StatusCode,
        name: &'static str,
        description: &'static str,
    ) -> Self {
        self.answers
            .get_mut(&status.as_u16())
            .unwrap_or_else(|| panic!("no answer {status} to add {name} to"))
            .headers
            .push((name, description));
        self
    }

    /// An error answer: `status`, with the error body, for `description`
    /// (the `code`s and what each means).
    ///
    /// # Panics
    ///
    /// If `status` is not a `4xx` or `5xx`.
    #[must_use]
    pub fn error(mut self, status: StatusCode, description: &str) -> Self {
        assert!(
            status.is_client_error() || status.is_server_error(),
            "{status} is not an error status"
        );
        self.answers
            .entry(status.as_u16())
            .and_modify(|answer| {
                answer.description.push(' ');
                answer.description.push_str(description);
            })
            .or_insert_with(|| Answer {
                error: true,
                description: description.to_owned(),
                content: Vec::new(),
                headers: Vec::new(),
            });
        self
    }

    /// The operation for `method` at the axum path template `path`.
    pub(crate) fn operation(&self, method: &Method, path: &str, private: bool) -> Operation {
        let mut parameters: Vec<Parameter> = path_parameters(path)
            .map(|(name, rest)| {
                let mut description = path_parameter(name).unwrap_or_default().to_owned();
                if rest {
                    description.push_str(" It may hold `/`.");
                }
                Parameter::builder()
                    .name(name)
                    .parameter_in(ParameterIn::Path)
                    .required(Required::True)
                    .description(Some(description.trim_start()))
                    .schema(Some(schema_of(&json!({"type": "string"}))))
                    .build()
            })
            .collect();
        parameters.extend(self.parameters.iter().cloned());

        let mut operation = Operation::builder()
            .tag(self.area.name())
            .summary(Some(self.summary))
            .description(self.description)
            .operation_id(Some(operation_id(method, path)))
            .parameters(Some(parameters))
            .request_body(self.body.clone())
            .securities(Some(self.credential.requirements()))
            .build();
        for (status, answer) in &self.answers {
            operation.responses.responses.insert(
                status.to_string(),
                self.response(*status, answer, private).into(),
            );
        }
        operation
    }

    fn response(&self, status: u16, answer: &Answer, private: bool) -> Response {
        let mut response = Response::new(answer.description.clone());
        if answer.error {
            // Before the server is ready, and on a panic, every route answers
            // the plain body, which a Hub body also matches.
            let anywhere = status == 500 || status == 503;
            let json = match self.errors {
                ErrorShape::Hub if !anywhere => Some("HubErrorBody"),
                ErrorShape::Git if !anywhere => None,
                _ => Some("ErrorBody"),
            };
            if let Some(schema) = json {
                response.content.insert(
                    "application/json".to_owned(),
                    Content::new(Some(Ref::from_schema_name(schema))).into(),
                );
            }
            if self.errors == ErrorShape::Git {
                response.content.insert(
                    "text/plain".to_owned(),
                    Content::new(Some(Ref::from_schema_name("GitErrorText"))).into(),
                );
            }
            if self.errors != ErrorShape::Plain {
                for name in ["X-Error-Code", "X-Error-Message"] {
                    response
                        .headers
                        .insert(name.to_owned(), RefOr::Ref(header_ref(name)));
                }
            }
        } else {
            for (media_type, schema) in &answer.content {
                response.content.insert(
                    (*media_type).to_owned(),
                    Content::new(Some(schema_of(schema))).into(),
                );
            }
        }
        response.headers.insert(
            "X-Request-Id".to_owned(),
            RefOr::Ref(header_ref("X-Request-Id")),
        );
        if private {
            response.headers.insert(
                "Cache-Control".to_owned(),
                RefOr::Ref(header_ref("Cache-Control")),
            );
        }
        for (name, description) in &answer.headers {
            let mut header = Header::new(schema_of(&json!({"type": "string"})));
            header.description = Some((*description).to_owned());
            response.headers.insert((*name).to_owned(), header.into());
        }
        response
    }
}

/// The schema of a body of raw bytes of `media_type`, a file or a pack
/// (JSON Schema's `contentMediaType`, as OpenAPI 3.1 describes binary
/// content).
#[must_use]
pub fn binary(media_type: &str) -> Value {
    json!({"type": "string", "contentMediaType": media_type})
}

/// The path parameters of the axum template `path`, each with whether it
/// is a catch-all.
fn path_parameters(path: &str) -> impl Iterator<Item = (&str, bool)> {
    path.split('/').filter_map(|segment| {
        let name = segment.strip_prefix('{')?.strip_suffix('}')?;
        Some(match name.strip_prefix('*') {
            Some(rest) => (rest, true),
            None => (name, false),
        })
    })
}

/// What the path parameter `name` holds, the same on every route that has
/// it.
fn path_parameter(name: &str) -> Option<&'static str> {
    Some(match name {
        "ns" => "The repository's namespace: a user or an organization.",
        "name" => "The repository's name. Under a Git URL it may end in `.git`.",
        "rev" => "A revision: a branch, a tag or a commit id.",
        "branch" => "A branch name.",
        "path" => "A path in the repository.",
        "rest" => "The rest of the path.",
        "label" => "A label: a lowercase slug of `a`-`z`, `0`-`9` and `-`.",
        "user" => "A user name.",
        "repo" => "The repository's id, as the server's own URLs give it.",
        "op" => "The operation the capability grants: `write`.",
        "file" => "A file's Xet hash, 64 lowercase hex characters.",
        "hash" => "A Xet hash, 64 lowercase hex characters.",
        "prefix" => "`default`; the dedup query also takes `default-merkledb`.",
        _ => return None,
    })
}

/// The OpenAPI path of the axum template `path`: a catch-all `{*rest}` is
/// the parameter `{rest}`.
pub(crate) fn openapi_path(path: &str) -> String {
    path.replace("{*", "{")
}

/// A unique id for `method` at `path`, as code generators name a call:
/// `get_api_models_ns_name`.
fn operation_id(method: &Method, path: &str) -> String {
    let mut id = method.as_str().to_ascii_lowercase();
    for word in path
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        id.push('_');
        id.push_str(&word.to_ascii_lowercase());
    }
    id
}

/// `method` as the document names it.
///
/// # Panics
///
/// For a method axum can't route either, as [`crate::Handlers::on`].
pub(crate) fn http_method(method: &Method) -> HttpMethod {
    match *method {
        Method::GET => HttpMethod::Get,
        Method::POST => HttpMethod::Post,
        Method::PUT => HttpMethod::Put,
        Method::DELETE => HttpMethod::Delete,
        Method::HEAD => HttpMethod::Head,
        Method::PATCH => HttpMethod::Patch,
        Method::OPTIONS => HttpMethod::Options,
        Method::TRACE => HttpMethod::Trace,
        _ => panic!("the document has no method {method}"),
    }
}

/// `schema`, a JSON Schema, in the document's model.
///
/// # Panics
///
/// If the model would drop or change any of it: a keyword `utoipa`
/// doesn't model is a mistake to fix, not one to publish silently. Routes
/// are built at startup, and every test builds them.
fn schema_of(schema: &Value) -> RefOr<Schema> {
    let parsed: RefOr<Schema> = serde_json::from_value(schema.clone())
        .unwrap_or_else(|err| panic!("schema {schema} doesn't parse: {err}"));
    let back = serde_json::to_value(&parsed)
        .unwrap_or_else(|err| panic!("schema {schema} doesn't serialize: {err}"));
    assert_eq!(&back, schema, "the document's model changes the schema");
    parsed
}

fn header_ref(name: &str) -> Ref {
    Ref::new(format!("#/components/headers/{name}"))
}

/// The document of `paths`, every part's, as `title` `version`.
#[must_use]
pub fn document(title: &str, version: &str, paths: impl IntoIterator<Item = Paths>) -> OpenApi {
    let mut all = Paths::new();
    for part in paths {
        all.merge(part);
    }
    let mut info = Info::new(title, version);
    info.description = Some(
        "The HTTP API of an OpenXet server: the Xet CAS, the Hugging Face Hub API, \
         Git smart HTTP, Git LFS and the file gateway. Every error is \
         `{\"schema\":\"openxet.error.v1\",\"code\",\"message\",\"hint\"}`: \
         `code` is stable, `hint` names the next action. Every answer carries \
         `X-Request-Id`."
            .to_owned(),
    );
    let mut license = License::new("Apache-2.0");
    license.identifier = Some("Apache-2.0".to_owned());
    info.license = Some(license);

    let mut components = components();
    share_responses(&mut all, &mut components);
    let mut doc = OpenApi::new(info, all);
    doc.tags = Some(
        Area::ALL
            .into_iter()
            .map(|area| {
                let mut tag = Tag::new(area.name());
                tag.description = Some(area.description().to_owned());
                tag
            })
            .collect(),
    );
    doc.components = Some(components);
    doc
}

/// Every operation of `paths`, in the document's order.
fn operations(paths: &mut Paths) -> impl Iterator<Item = &mut Operation> {
    paths.paths.values_mut().flat_map(|item| {
        [
            &mut item.get,
            &mut item.put,
            &mut item.post,
            &mut item.delete,
            &mut item.options,
            &mut item.head,
            &mut item.patch,
            &mut item.trace,
        ]
        .into_iter()
        .flatten()
    })
}

/// Moves every response that more than one operation gives into
/// `components`, named after the first operation that gives it, and refers
/// to it from each: the routes of each Hub type share their descriptions,
/// and the routes of one kind their errors.
fn share_responses(paths: &mut Paths, components: &mut Components) {
    let key = |response: &RefOr<Response>| {
        serde_json::to_string(response)
            .unwrap_or_else(|e| unreachable!("a response is plain JSON values: {e}"))
    };
    let mut uses: BTreeMap<String, usize> = BTreeMap::new();
    for operation in operations(paths) {
        for response in operation.responses.responses.values() {
            *uses.entry(key(response)).or_default() += 1;
        }
    }
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for operation in operations(paths) {
        let id = operation.operation_id.clone().unwrap_or_default();
        for (status, response) in &mut operation.responses.responses {
            let text = key(response);
            if uses.get(&text).copied().unwrap_or_default() < 2 {
                continue;
            }
            let name = names.entry(text).or_insert_with(|| {
                let name = format!("{id}_{status}");
                components.responses.insert(name.clone(), response.clone());
                name
            });
            *response = RefOr::Ref(Ref::new(format!("#/components/responses/{name}")));
        }
    }
}

fn components() -> Components {
    let mut components = Components::new();
    components.schemas.insert(
        "ErrorBody".to_owned(),
        schema_of(&json!({
            "type": "object",
            "description": "Every error answer's body.",
            "required": ["schema", "code", "message", "hint"],
            "properties": {
                "schema": {"type": "string", "enum": [SCHEMA]},
                "code": {
                    "type": "string",
                    "description": "Stable snake case, for scripts and agents."
                },
                "message": {"type": "string", "description": "What happened."},
                "hint": {"type": "string", "description": "What to do next."}
            }
        })),
    );
    components.schemas.insert(
        "HubErrorBody".to_owned(),
        schema_of(&json!({
            "description": "A Hub route's error body: the error body with \
                huggingface.co's `error` field, which holds the message.",
            "allOf": [
                {"$ref": "#/components/schemas/ErrorBody"},
                {
                    "type": "object",
                    "required": ["error"],
                    "properties": {"error": {"type": "string"}}
                }
            ]
        })),
    );
    components.schemas.insert(
        "GitErrorText".to_owned(),
        schema_of(&json!({
            "type": "string",
            "description": "A Git route's error: the message, then the hint, a line each, \
                which git prints as `remote:` lines. `X-Error-Message` holds the message."
        })),
    );
    for (name, description) in [
        (
            "X-Request-Id",
            "The request's id, under which the server logged it. Give it to the operator.",
        ),
        (
            "Cache-Control",
            "`no-store`: the answer depends on the caller's credential.",
        ),
        (
            "X-Error-Code",
            "huggingface.co's error code (`RepoNotFound`, `RevisionNotFound`, \
             `EntryNotFound`, …), where it defines one.",
        ),
        (
            "X-Error-Message",
            "The message, as `huggingface_hub` shows it; characters a header can't \
             carry are `?`.",
        ),
    ] {
        let mut header = Header::new(schema_of(&json!({"type": "string"})));
        header.description = Some(description.to_owned());
        components.headers.insert(name.to_owned(), header.into());
    }
    let mut http = |name: &str, scheme: HttpAuthScheme, description: &str| {
        let mut http = Http::new(scheme);
        http.description = Some(description.to_owned());
        components
            .security_schemes
            .insert(name.to_owned(), SecurityScheme::Http(http).into());
    };
    http(
        CAS_TOKEN,
        HttpAuthScheme::Bearer,
        "A static token or a CAS token (docs/reference/config.md#static-tokens).",
    );
    http(
        ACCESS_TOKEN,
        HttpAuthScheme::Bearer,
        "An OpenXet access token, which the Hub's token routes exchange for CAS tokens.",
    );
    http(
        ACCESS_TOKEN_BASIC,
        HttpAuthScheme::Basic,
        "An OpenXet access token as the password; the user name is ignored.",
    );
    let mut signed = ApiKeyValue::new("sig");
    signed.description = Some(
        "A URL the server signed and handed out: its `sig` query parameter, with the \
         others it signs, is the whole credential. Use the URL exactly as given."
            .to_owned(),
    );
    components.security_schemes.insert(
        SIGNED_URL.to_owned(),
        SecurityScheme::ApiKey(ApiKey::Query(signed)).into(),
    );
    components
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_catch_all_is_an_openapi_parameter() {
        assert_eq!(
            openapi_path("/{ns}/{name}/tree/{rev}/{*path}"),
            "/{ns}/{name}/tree/{rev}/{path}"
        );
        assert_eq!(
            path_parameters("/{ns}/{name}/tree/{rev}/{*path}").collect::<Vec<_>>(),
            [
                ("ns", false),
                ("name", false),
                ("rev", false),
                ("path", true)
            ]
        );
    }

    #[test]
    fn operation_ids_follow_method_and_path() {
        assert_eq!(
            operation_id(&Method::GET, "/api/models/{ns}/{name}/tree/{rev}/{*path}"),
            "get_api_models_ns_name_tree_rev_path"
        );
        assert_eq!(
            operation_id(&Method::POST, "/v1/xorbs/{prefix}/{hash}"),
            "post_v1_xorbs_prefix_hash"
        );
    }

    #[test]
    #[should_panic(expected = "the document's model changes the schema")]
    fn a_keyword_the_model_would_drop_is_refused() {
        let _ = schema_of(&json!({"type": "string", "const": "x"}));
    }
}
