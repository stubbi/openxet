//! Git's smart HTTP protocol (ruling M2-R11, ADR 0004 item 2):
//! `info/refs?service=git-upload-pack` and `git-receive-pack`, and the two
//! `POST`s, served by the Git engine at `/{prefix}{ns}/{name}` and
//! `/{prefix}{ns}/{name}.git`. The dumb protocol is not served: a path git
//! would only ask a dumb server for answers `404`.
//!
//! The unprefixed URL is openxet's own address and sees every repository
//! whatever its labels; `datasets/`, `spaces/` and `kernels/` see a
//! repository through that label, as on huggingface.co (ADR 0010 item 4).
//! Credentials are the Hub's, as `Bearer`, or as HTTP Basic whose password
//! is the token, as git's credential helpers send it (ADR 0005 item 2). A
//! repository the caller may not read answers exactly as one that doesn't
//! exist, on every path (ruling M2-R6). The first push to a missing
//! repository creates it when the caller may create in its namespace: its
//! advertisement shows an empty repository and changes nothing, and its
//! `POST` creates the repository. Errors are `text/plain`, which git shows
//! its user as `remote:` lines.
//!
//! The `POST`s take only git's own request types, as `git http-backend`
//! does (`check_content_type`, `http-backend.c`, git v2.39.5): a browser
//! can't send those without a CORS preflight, so a web page can't push or
//! fetch with credentials the browser cached for this origin.

use std::io::{self, Write as _};

use axum::body::{Body, Bytes};
use axum::extract::{MatchedPath, RawPathParams, RawQuery, State};
use axum::http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use flate2::write::GzDecoder;
use futures_util::StreamExt as _;
use openxet_auth::{Action, UserPrincipal};
use openxet_git::{GitError, Service, protocol_version};
use openxet_index::Repo;
use openxet_storage::BodyStream;

use crate::authz::{self, RepoAccess, Who};
use crate::routes::{Area, Doc, Routes, git_url, on, with_write};
use crate::{HubError, HubState, View, names, param, repos};

/// The paths of one Git route, unprefixed and under each typed prefix
/// but the model's, which has none on huggingface.co.
macro_rules! git_paths {
    ($suffix:literal) => {
        [
            concat!("/{ns}/{name}", $suffix),
            concat!("/datasets/{ns}/{name}", $suffix),
            concat!("/spaces/{ns}/{name}", $suffix),
            concat!("/kernels/{ns}/{name}", $suffix),
        ]
    };
}

/// The Git routes. Every answer depends on the caller's credential.
pub(crate) fn routes() -> Routes {
    let refs = git_paths!("/info/refs");
    let upload = git_paths!("/git-upload-pack");
    let receive = git_paths!("/git-receive-pack");
    refs.into_iter().zip(upload).zip(receive).fold(
        Routes::new(),
        |routes, ((refs, upload), receive)| {
            routes
                .route(refs, on(Method::GET, info_refs_doc(), info_refs).private())
                .route(
                    upload,
                    on(Method::POST, upload_pack_doc(), upload_pack).private(),
                )
                .route(
                    receive,
                    on(Method::POST, receive_pack_doc(), receive_pack).private(),
                )
        },
    )
}

/// A Git route: errors are `text/plain`, which git shows as `remote:`
/// lines.
fn git_doc(summary: &'static str) -> Doc {
    git_url(Area::Git, summary).git_errors().error(
        StatusCode::SERVICE_UNAVAILABLE,
        "`busy`: too many requests in the repository at once. `Retry-After: 1`.",
    )
}

/// A `POST` of git's: its own request type and a pack answer.
fn service_doc(summary: &'static str, service: &'static str) -> Doc {
    let (request, result) = match service {
        "upload-pack" => (
            "application/x-git-upload-pack-request",
            "application/x-git-upload-pack-result",
        ),
        _ => (
            "application/x-git-receive-pack-request",
            "application/x-git-receive-pack-result",
        ),
    };
    git_doc(summary)
        .header(
            "Content-Encoding",
            false,
            "`gzip`, as git sends a large fetch request; inflated as it arrives.",
        )
        .body(
            request,
            &openxet_http::binary(request),
            "Git's request, in pkt-lines. Only this type is taken, as `git http-backend` \
             does, so a browser can't send it without a CORS preflight.",
        )
        .answer(
            StatusCode::OK,
            "Git's answer, in pkt-lines.",
            Some((result, openxet_http::binary(result))),
        )
        .error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "`unsupported_media_type`: not git's request type.",
        )
        .error(
            StatusCode::REQUEST_TIMEOUT,
            "`body_timeout`: the body arrived too slowly.",
        )
}

fn info_refs_doc() -> Doc {
    git_doc("Ref advertisement")
        .describe(
            "Smart HTTP's first request, for a clone, fetch or push. `{name}` may end in \
             `.git`. The unprefixed URL sees every repository whatever its labels; \
             `/datasets`, `/spaces` and `/kernels` see it through that label. For a push to \
             a missing repository the caller may create in its namespace, it advertises an \
             empty repository.",
        )
        .query(
            "service",
            true,
            &serde_json::json!({"type": "string", "enum": ["git-upload-pack", "git-receive-pack"]}),
            "The service; a request without one is the dumb protocol's, answered `404`.",
        )
        .header("Git-Protocol", false, "`version=2` for protocol v2.")
        .answer(
            StatusCode::OK,
            "The advertisement, in pkt-lines, typed after the service.",
            Some((
                "application/x-git-upload-pack-advertisement",
                openxet_http::binary("application/x-git-upload-pack-advertisement"),
            )),
        )
        .answer_media(
            StatusCode::OK,
            "application/x-git-receive-pack-advertisement",
            openxet_http::binary("application/x-git-receive-pack-advertisement"),
        )
        .error(
            StatusCode::NOT_FOUND,
            "`dumb_http_not_served`: no `service`; only smart HTTP is served.",
        )
}

fn upload_pack_doc() -> Doc {
    service_doc("Fetch", "upload-pack")
        .describe(
            "One stateless fetch or `ls-refs` request. A want the refs don't reach is \
             refused.",
        )
        .error(
            StatusCode::TOO_MANY_REQUESTS,
            "`too_many_requests`: the reachability check is over its concurrency limit for \
             the repository or the caller. `Retry-After: 1`.",
        )
}

fn receive_pack_doc() -> Doc {
    with_write(service_doc("Push", "receive-pack"))
        .describe(
            "One push, whose refs move only through the fenced commit transaction. The first \
             push to a missing repository creates it when the caller may create in its \
             namespace.",
        )
        .error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "`push_too_large`: the push is larger than `git.max_push_bytes`.",
        )
}

/// How much compressed input is inflated at a time: deflate expands at
/// most [`MAX_INFLATE_RATIO`] times, so one step yields at most about 4 MiB.
const INFLATE_STEP: usize = 4 << 10;

/// The most deflate expands: one 258-byte match per two bits, 1,032 to 1
/// (RFC 1951 §3.2.5). A gzip body inflating past this many times the bytes
/// read so far is not deflate, and is refused.
const MAX_INFLATE_RATIO: u64 = 1032;

/// The label a repository created by a push through the unprefixed URL
/// carries, as the Hub's default view does (ruling M2-R11).
const DEFAULT_LABEL: &str = "model";

/// The repository a Git request names, as the caller wrote it: the route's
/// view, `{ns}`, and `{name}` without a trailing `.git`. Names never end in
/// `.git` (ruling M2-R2), so stripping it is unambiguous.
struct Target {
    view: View,
    namespace: String,
    name: String,
}

impl Target {
    fn of(path: &MatchedPath, params: &RawPathParams) -> Self {
        // `View::of_route` gives unprefixed routes the model view, as the
        // Hub's resolve route; the unprefixed Git URL is openxet's own
        // address, which sees every repository (ADR 0010 item 4).
        let view = match View::of_route(path.as_str()) {
            View::Model => View::Any,
            view => view,
        };
        let name = param(params, "name");
        let name = name.strip_suffix(".git").unwrap_or(&name).to_owned();
        Self {
            view,
            namespace: param(params, "ns"),
            name,
        }
    }

    /// The repository for reading: the unknown-repository answer for one
    /// the caller may not read.
    async fn readable(&self, state: &HubState, who: UserPrincipal) -> Result<Repo, HubError> {
        RepoAccess::resolve(state, who, &self.namespace, &self.name, self.view)
            .await
            .map(|access| access.repo)
    }

    /// The repository for pushing: readable and writable by `who`.
    async fn writable(&self, state: &HubState, who: &UserPrincipal) -> Result<Repo, HubError> {
        let repo = self.readable(state, who.clone()).await?;
        authz::require_as(state, who, Action::Write, &repo, "push to").await?;
        Ok(repo)
    }

    /// The repository a push goes to: `Some` if it exists and `who` may
    /// push to it, `None` if it is missing and `who` may create it (ruling
    /// M2-R11). One that exists but that `who` may not read answers as
    /// missing all the same, and so does a missing one `who` may not
    /// create.
    async fn push_target(
        &self,
        state: &HubState,
        who: &UserPrincipal,
    ) -> Result<Option<Repo>, HubError> {
        match self.writable(state, who).await {
            Ok(repo) => Ok(Some(repo)),
            Err(e @ (HubError::UnknownRepo | HubError::MissingCredential)) => {
                if self.may_create(state, who).await?
                    && state
                        .index
                        .repo_by_name(&self.namespace, &self.name)
                        .await?
                        .is_none()
                {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Creates the missing repository a push goes to, without the initial
    /// commit (its refs are the push's own) and with the view's label or
    /// `model`, and returns it once `who` may push to it.
    async fn create(&self, state: &HubState, who: &UserPrincipal) -> Result<Repo, HubError> {
        let label = self.view.label().unwrap_or(DEFAULT_LABEL);
        match repos::create_repo_for_push(state, who, &self.namespace, &self.name, label).await {
            // Another request created it meanwhile: push to that one.
            Ok(_) | Err(HubError::NameTaken { .. }) => {}
            Err(e) => return Err(e),
        }
        self.writable(state, who).await
    }

    /// Whether `who` may create this repository and then read and push to
    /// it: a push never creates a repository its pusher can't use.
    async fn may_create(&self, state: &HubState, who: &UserPrincipal) -> Result<bool, HubError> {
        let (namespace, name) = (self.namespace.as_str(), Some(self.name.as_str()));
        Ok(names::check_name(namespace, &self.name).is_ok()
            && authz::allowed(state, who, Action::Create, namespace, None).await?
            && authz::allowed(state, who, Action::Read, namespace, name).await?
            && authz::allowed(state, who, Action::Write, namespace, name).await?)
    }
}

/// `GET …/info/refs?service=git-{upload,receive}-pack`: the ref
/// advertisement, after the `# service=` preamble smart HTTP adds for
/// protocols 0 and 1. A request without one of the two services is the
/// dumb protocol's, answered `404` once the caller may read the
/// repository. It never changes anything: a push to a missing repository
/// its caller may create is shown an empty one.
async fn info_refs(
    State(state): State<HubState>,
    path: MatchedPath,
    params: Result<RawPathParams, axum::extract::rejection::RawPathParamsRejection>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let service = query.as_deref().and_then(service_of);
    let answer = async {
        let who = who(&state, &headers)?;
        let target = target(&path, params, &who)?;
        let repo = match service {
            Some(Service::ReceivePack) => target.push_target(&state, &who).await?,
            _ => Some(target.readable(&state, who).await?),
        };
        let Some(service) = service else {
            return Err(HubError::DumbHttp);
        };
        let protocol = protocol(&headers);
        let advertisement = match repo {
            Some(repo) => state
                .engine
                .advertise(repo.id, protocol, service)
                .await
                .map_err(git_error)?,
            None => Bytes::copy_from_slice(state.engine.empty_receive_advertisement()),
        };
        let mut body = Vec::with_capacity(advertisement.len() + 64);
        // git serves receive-pack over protocol 0 whatever is asked, and
        // `git http-backend` writes the preamble for everything but a v2
        // upload-pack (`http-backend.c`, git v2.39.5).
        if protocol != 2 || service == Service::ReceivePack {
            let line = format!("# service=git-{}\n", service.as_str());
            body.extend_from_slice(format!("{:04x}{line}0000", line.len() + 4).as_bytes());
        }
        body.extend_from_slice(&advertisement);
        Ok(git_response(service, "advertisement", Body::from(body)))
    };
    answer.await.unwrap_or_else(git_refusal)
}

/// `POST …/git-upload-pack`: one stateless fetch or `ls-refs` request,
/// checked for reachability before git sees it (R22).
async fn upload_pack(
    State(state): State<HubState>,
    path: MatchedPath,
    params: Result<RawPathParams, axum::extract::rejection::RawPathParamsRejection>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let answer = async {
        let who = who(&state, &headers)?;
        let target = target(&path, params, &who)?;
        let subject = who.subject.clone();
        let repo = target.readable(&state, who).await?;
        require_request_type(&headers, Service::UploadPack)?;
        let body = request_body(&headers, body)?;
        let out = state
            .engine
            .upload_pack(repo.id, protocol(&headers), &subject, body)
            .await
            .map_err(git_error)?;
        Ok(git_response(
            Service::UploadPack,
            "result",
            Body::from_stream(out),
        ))
    };
    answer.await.unwrap_or_else(git_refusal)
}

/// `POST …/git-receive-pack`: one push, whose refs move only through the
/// fenced commit transaction (ADR 0004 item 3). The first push to a missing
/// repository its caller may create creates it, once the request is git's
/// and within `git.max_push_bytes`.
async fn receive_pack(
    State(state): State<HubState>,
    path: MatchedPath,
    params: Result<RawPathParams, axum::extract::rejection::RawPathParamsRejection>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let answer = async {
        let who = who(&state, &headers)?;
        let target = target(&path, params, &who)?;
        let repo = target.push_target(&state, &who).await?;
        require_request_type(&headers, Service::ReceivePack)?;
        let limit = state.engine.max_push_bytes();
        if content_length(&headers).is_some_and(|len| len > limit) {
            return Err(HubError::PushTooLarge { limit });
        }
        // Every check of the request comes before a missing repository is
        // created, so a refused push creates nothing.
        let body = request_body(&headers, body)?;
        let repo = match repo {
            Some(repo) => repo,
            None => target.create(&state, &who).await?,
        };
        let out = state
            .engine
            .receive_pack(repo.id, protocol(&headers), &who, body)
            .await
            .map_err(git_error)?;
        Ok(git_response(
            Service::ReceivePack,
            "result",
            Body::from_stream(out),
        ))
    };
    answer.await.unwrap_or_else(git_refusal)
}

/// The request's `Content-Length`, if it sent one. git sends one for a
/// request that fits its post buffer, and streams a larger one chunked,
/// which git itself bounds (`receive.maxInputSize`).
fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

/// Refuses a `POST` whose `Content-Type` isn't git's request type for
/// `service`, parameters aside.
fn require_request_type(headers: &HeaderMap, service: Service) -> Result<(), HubError> {
    let expected = match service {
        Service::UploadPack => "application/x-git-upload-pack-request",
        Service::ReceivePack => "application/x-git-receive-pack-request",
    };
    let sent = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::trim);
    if sent.is_some_and(|t| t.eq_ignore_ascii_case(expected)) {
        Ok(())
    } else {
        Err(HubError::WrongContentType { expected })
    }
}

/// The caller, by `Bearer` or by HTTP Basic with the token as the password.
fn who(state: &HubState, headers: &HeaderMap) -> Result<UserPrincipal, HubError> {
    Who::from_headers(headers, state, true).map(|Who(who)| who)
}

/// The request's repository. Parameters that aren't UTF-8 name no
/// repository that can exist, which answers as one that doesn't.
fn target(
    path: &MatchedPath,
    params: Result<RawPathParams, axum::extract::rejection::RawPathParamsRejection>,
    who: &UserPrincipal,
) -> Result<Target, HubError> {
    let params = params.map_err(|_| authz::denied(who))?;
    Ok(Target::of(path, &params))
}

/// The service a query names: `service=git-upload-pack` or
/// `service=git-receive-pack`.
fn service_of(query: &str) -> Option<Service> {
    query.split('&').find_map(|pair| match pair {
        "service=git-upload-pack" => Some(Service::UploadPack),
        "service=git-receive-pack" => Some(Service::ReceivePack),
        _ => None,
    })
}

/// The protocol version the `Git-Protocol` header asks for; protocol 0
/// without one, or for a value git doesn't know either.
fn protocol(headers: &HeaderMap) -> u8 {
    headers
        .get_all("git-protocol")
        .iter()
        .find_map(|v| protocol_version(v.as_bytes()))
        .unwrap_or(0)
}

/// The request body as the engine reads it, inflated when git sent it with
/// `Content-Encoding: gzip`, as it does for a fetch request over 1 KiB
/// (`remote-curl.c`, git v2.39.5).
fn request_body(headers: &HeaderMap, body: Body) -> Result<BodyStream, HubError> {
    let stream: BodyStream = body
        .into_data_stream()
        .map(|r| r.map_err(io::Error::other))
        .boxed();
    let mut encodings = headers.get_all(CONTENT_ENCODING).iter();
    match (encodings.next(), encodings.next()) {
        (None, _) => Ok(stream),
        (Some(value), None) if value.as_bytes().eq_ignore_ascii_case(b"identity") => Ok(stream),
        (Some(value), None)
            if value.as_bytes().eq_ignore_ascii_case(b"gzip")
                || value.as_bytes().eq_ignore_ascii_case(b"x-gzip") =>
        {
            Ok(gunzip(stream))
        }
        _ => Err(HubError::BadRequest {
            message: "the request body's Content-Encoding is not supported".to_owned(),
            hint: "Send the body as it is, or compressed with gzip and `Content-Encoding: gzip`, \
                   as git does.",
        }),
    }
}

/// `body` inflated from gzip as it arrives, [`INFLATE_STEP`] bytes of input
/// at a time, so a small body can't inflate into memory all at once. A
/// truncated or corrupt body, or one inflating past [`MAX_INFLATE_RATIO`]
/// times what was read, ends the stream with an error, which the engine
/// treats as a failed request body.
fn gunzip(body: BodyStream) -> BodyStream {
    inflate(body, MAX_INFLATE_RATIO)
}

/// [`gunzip`], refusing past `max_ratio` times the bytes read.
fn inflate(body: BodyStream, max_ratio: u64) -> BodyStream {
    struct Inflating {
        body: BodyStream,
        input: Bytes,
        decoder: Option<GzDecoder<Vec<u8>>>,
        read: u64,
        inflated: u64,
    }
    let start = Inflating {
        body,
        input: Bytes::new(),
        decoder: Some(GzDecoder::new(Vec::new())),
        read: 0,
        inflated: 0,
    };
    futures_util::stream::unfold(start, move |mut s| async move {
        loop {
            let decoder = s.decoder.as_mut()?;
            if !s.input.is_empty() {
                let step = s.input.split_to(s.input.len().min(INFLATE_STEP));
                if let Err(e) = decoder.write_all(&step) {
                    s.decoder = None;
                    return Some((Err(e), s));
                }
                let out = std::mem::take(decoder.get_mut());
                s.read += step.len() as u64;
                s.inflated += out.len() as u64;
                if s.inflated > s.read.saturating_mul(max_ratio) {
                    s.decoder = None;
                    return Some((
                        Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "the gzip body inflates further than deflate can encode",
                        )),
                        s,
                    ));
                }
                if !out.is_empty() {
                    return Some((Ok(Bytes::from(out)), s));
                }
                continue;
            }
            match s.body.next().await {
                Some(Ok(bytes)) => s.input = bytes,
                Some(Err(e)) => {
                    s.decoder = None;
                    return Some((Err(e), s));
                }
                None => {
                    let mut decoder = s.decoder.take()?;
                    if let Err(e) = decoder.try_finish() {
                        return Some((Err(e), s));
                    }
                    let out = std::mem::take(decoder.get_mut());
                    return (!out.is_empty()).then(|| (Ok(Bytes::from(out)), s));
                }
            }
        }
    })
    .boxed()
}

/// A Git response: `application/x-git-{service}-{kind}`.
fn git_response(service: Service, kind: &str, body: Body) -> Response {
    let mut response = body.into_response();
    if let Ok(value) =
        HeaderValue::try_from(format!("application/x-git-{}-{kind}", service.as_str()))
    {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    response
}

/// The engine's failure `e` as the Hub answers it, except that a
/// reachability check over a concurrency limit is `429` (ADR 0004 item 2)
/// and a request body that failed is the body's error.
fn git_error(e: GitError) -> HubError {
    match e {
        GitError::Refused(refused) if refused.retryable() => {
            HubError::Throttled(refused.to_string())
        }
        GitError::Body(e) => {
            let timed_out = e
                .get_ref()
                .is_some_and(|inner| openxet_http::timed_out(inner).is_some());
            if timed_out {
                HubError::BodyTimeout
            } else {
                HubError::BadRequest {
                    message: "the request body could not be read".to_owned(),
                    hint: "Send the request again; a body sent with `Content-Encoding: gzip` \
                           must be one complete gzip stream.",
                }
            }
        }
        e => e.into(),
    }
}

/// `e`'s answer as `text/plain`, with a `Basic` challenge before the `Bearer` one on a
/// `401`: git asks its credential helpers for a user name and password
/// only when the server offers `Basic`.
fn git_refusal(e: HubError) -> Response {
    let mut response = e.into_git_response();
    if response.status() == StatusCode::UNAUTHORIZED {
        let headers = response.headers_mut();
        let bearer: Vec<HeaderValue> = headers.get_all(WWW_AUTHENTICATE).iter().cloned().collect();
        headers.insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Basic realm="openxet""#),
        );
        for value in bearer {
            headers.append(WWW_AUTHENTICATE, value);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::Write as _;

    use futures_util::StreamExt as _;

    use super::*;

    fn gzipped(data: &[u8]) -> BodyStream {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gz.write_all(data).unwrap();
        let bytes = Bytes::from(gz.finish().unwrap());
        futures_util::stream::iter([Ok(bytes)]).boxed()
    }

    /// A body inflating to many steps' worth arrives in pieces of at most
    /// one step's output, never whole.
    #[tokio::test]
    async fn inflation_is_bounded_per_step() {
        let data = vec![0u8; INFLATE_STEP * 1032 * 4];
        let pieces: Vec<Bytes> = gunzip(gzipped(&data)).map(Result::unwrap).collect().await;
        assert!(pieces.len() > 1, "{} piece", pieces.len());
        let step = INFLATE_STEP * usize::try_from(MAX_INFLATE_RATIO).unwrap();
        assert!(pieces.iter().all(|p| p.len() <= step));
        assert_eq!(pieces.concat(), data);
    }

    /// A body inflating past the ratio bound ends with an error.
    #[tokio::test]
    async fn inflation_past_the_ratio_is_refused() {
        let data = vec![0u8; 1 << 20];
        let results: Vec<io::Result<Bytes>> = inflate(gzipped(&data), 10).collect().await;
        let error = results.last().unwrap().as_ref().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
