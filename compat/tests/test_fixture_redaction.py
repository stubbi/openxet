"""No tracked file may carry credentials or signed URLs.

Fixtures are real exchanges with huggingface.co and object stores, and the
research notes, ADRs and code quote them, so every text file `git ls-files`
lists is scanned, except the allowlist below. Every secret value is replaced by the literal
`REDACTED`; parameter and header names stay so the shape remains visible. A
pattern hit is allowed only when the value that follows it is that
placeholder.
"""

import base64
import binascii
import re
import subprocess
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
# This guard spells out leak shapes on purpose; plans quote them while
# designing redaction.
ALLOWLIST = (
    "compat/tests/test_fixture_redaction.py",
    "docs/superpowers/plans/",
)

# The placeholder, ending where a value would: `REDACTEDabc` or
# `REDACTED.x` is a real value that merely starts with it.
_R = r"REDACTED(?![\w%+/=-]|\.\w)"

_SECRET_PATTERNS = {
    "HF user token": re.compile(r"hf_[A-Za-z0-9]{20,}"),
    # A bare `xet_[A-Za-z0-9]` also hits code identifiers quoted in the notes
    # (`xet_get`, `assert_xet_download`); a token is one long unbroken run.
    "Xet token": re.compile(r"(?<![A-Za-z0-9_])xet_[A-Za-z0-9]{20,}"),
    # `Bearer realm=` is the RFC 6750 challenge huggingface.co sends with a 401;
    # `<token>`-style placeholders and the word "token" appear in prose.
    "bearer credential": re.compile(
        rf"Bearer (?!{_R}|realm=|<[^>]+>|token(?![\w.-]))"
    ),
    "URL signature": re.compile(rf"Signature=(?!{_R})", re.IGNORECASE),
    "S3 signature header": re.compile(
        rf"X-Amz-Signature\"?\s*:\s*\"?(?!{_R})", re.IGNORECASE
    ),
    "S3 credential": re.compile(rf"X-Amz-Credential=(?!{_R})", re.IGNORECASE),
    "S3 session token": re.compile(rf"X-Amz-Security-Token=(?!{_R})", re.IGNORECASE),
    "CloudFront key pair": re.compile(rf"Key-Pair-Id=(?!{_R})", re.IGNORECASE),
    "CloudFront policy": re.compile(rf"[?&]Policy=(?!{_R})", re.IGNORECASE),
    "CDN client address": re.compile(rf"[?&]xip=(?!{_R})"),
    # Xet access tokens on huggingface.co are JWTs, not `xet_`-prefixed.
    "JWT": re.compile(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\."),
    # Headers appear as `"name": "value"`, as `["name", "value"]` pairs, or
    # as raw `Name: value` lines. `Bearer REDACTED` keeps the auth scheme.
    "credential header": re.compile(
        r"\"(authorization|proxy-authorization|cookie|set-cookie|x-xet-access-token)\""
        r"\s*[:,]\s*\"(?!(Bearer )?REDACTED\")",
        re.IGNORECASE,
    ),
    "credential header line": re.compile(
        # `> ` / `< ` is the GIT_CURL_VERBOSE request/response prefix; docs
        # also indent, quote (`> `) and list (`* `, `- `) header lines.
        r"^[ \t<>*-]*(authorization|proxy-authorization|cookie|set-cookie|x-xet-access-token)"
        rf":(?![ \t]*(Bearer )?{_R})",
        re.IGNORECASE | re.MULTILINE,
    ),
    # Also inside a JSON body that is itself stored as an escaped string.
    "token field": re.compile(
        r"\\?\"accessToken\\?\"\s*:\s*\\?\"(?!(REDACTED|<[^>\"]+>)\\?\")"
    ),
}


# A Basic credential can appear anywhere: in prose, escaped JSON, a trace
# line or an action header. Prose such as "Basic auth" also matches the
# shape, so a hit only counts when the base64 decodes to printable
# `user:password` text.
# The scheme name is case-insensitive (RFC 9110 section 11.1).
_BASIC = re.compile(
    rf"\bBasic (?!{_R}|realm=)([A-Za-z0-9+/]{{8,}}={{0,2}})", re.IGNORECASE
)


def _is_basic_credential(candidate: str) -> bool:
    try:
        decoded = base64.b64decode(candidate + "=" * (-len(candidate) % 4), validate=True)
        text = decoded.decode("utf-8")
    except (binascii.Error, UnicodeDecodeError):
        return False
    return ":" in text and text.isprintable()


def secrets_in(text: str) -> list[str]:
    """Names and offsets of hits; never the matched value, so CI logs stay clean."""
    hits = [
        f"{name} at offset {match.start()}"
        for name, pattern in _SECRET_PATTERNS.items()
        for match in pattern.finditer(text)
    ]
    hits += [
        f"Basic credential at offset {match.start()}"
        for match in _BASIC.finditer(text)
        if _is_basic_credential(match.group(1))
    ]
    return hits


def _scanned_files() -> list[Path]:
    listed = subprocess.run(
        ["git", "ls-files", "-z"], cwd=REPO, capture_output=True, check=True
    ).stdout.decode()
    names = [n for n in listed.split("\0") if n and not n.startswith(ALLOWLIST)]
    return sorted(p for p in (REPO / n for n in names) if p.is_file() and _is_text(p))


def _is_text(path: Path) -> bool:
    with path.open("rb") as f:
        return b"\0" not in f.read(8192)


def test_scan_covers_fixtures_and_docs():
    scanned = {p.relative_to(REPO).as_posix() for p in _scanned_files()}
    for expected in (
        "compat/fixtures/lfs/README.md",
        "docs/research/S7-git-lfs-xet.md",
        "docs/superpowers/specs/2026-10-23-openxet-design.md",
        "AGENTS.md",
    ):
        assert expected in scanned, f"{expected} is not scanned; did the path move?"
    assert not any(p.startswith(ALLOWLIST) for p in scanned)


@pytest.mark.parametrize(
    "path", _scanned_files(), ids=lambda p: str(p.relative_to(REPO))
)
def test_file_has_no_secrets(path):
    hits = secrets_in(path.read_text(encoding="utf-8", errors="replace"))
    assert not hits, (
        f"{path.relative_to(REPO)} contains unredacted secrets: {hits[:5]}. "
        "Replace each value with the literal REDACTED (keep the name) before "
        "committing, and revoke the credential if it was ever pushed."
    )


@pytest.mark.parametrize(
    "leak",
    [
        "Authorization: Bearer hf_abcdefghijklmnopqrstuvwxyz0123",
        '"authorization": "Bearer abc.def.ghi"',
        '"X-Xet-Access-Token": "eyJhbGciOi"',
        '"accessToken": "eyJhbGciOi"',
        '{"text": "{\\"accessToken\\":\\"abc\\"}"}',
        '["X-Xet-Access-Token", "abc"]',
        '["Authorization", "Bearer abc"]',
        "eyJhbGciOiJFZERTQSJ9.eyJzdWIiOiJ4In0.c2ln",
        '"set-cookie": "token=abc; Path=/"',
        "xet_0123456789abcdef0123456789",
        '"accessToken":"<JWT>x"',
        "Authorization: Bearer tokenvalue123",
        "https://cas-bridge.xethub.hf.co/x?Expires=1&Policy=eyJ&Signature=Zm9v&Key-Pair-Id=K2",
        "https://s3/x?X-Amz-Credential=AKIA%2F&X-Amz-Signature=deadbeef",
        "https://us.aws.cdn.hf.co/xorbs/default/h?xip=abc",
        '"x-amz-signature": "deadbeef"',
        "https://cdn/x?expires=1&signature=Zm9v&key-pair-id=K2",
        "https://cdn/x?policy=eyJ",
        "https://s3/x?x-amz-credential=AKIA%2F",
        "https://s3/x?X-Amz-Security-Token=IQoJb3JpZ2lu",
        "https://s3/x?x-amz-security-token=IQoJb3JpZ2lu",
        '"Proxy-Authorization": "Basic dXNlcjpwYXNz"',
        '["proxy-authorization", "Basic dXNlcjpwYXNz"]',
        "Proxy-Authorization: Basic dXNlcjpwYXNz",
        "Cookie: token=abc",
        "HTTP/1.1 200 OK\nset-cookie: session=abc; Path=/",
        '"authorization": "Bearer REDACTEDabc"',
        "> Authorization: Basic dXNlcjpwYXNz",
        "> authorization: Basic dXNlcjpwYXNz",
        '{\\"Authorization\\":\\"Basic dXNlcjpwYXNz\\"}',
        "header Basic dXNlcjp4eHh4eHh4eA==",
        # Quoted, indented or listed header lines in docs.
        "> > Authorization: Bearer abc",
        "  Authorization: Bearer abc",
        "* Cookie: session=abc",
        "- X-Xet-Access-Token: abc",
        "\tProxy-Authorization: Basic dXNlcjpwYXNz",
        # A value that merely starts with the placeholder.
        "Authorization: Bearer REDACTEDabc",
        "https://cdn/x?Signature=REDACTEDZm9v",
        "https://s3/x?X-Amz-Credential=REDACTED.AKIA",
        '"accessToken": "REDACTEDeyJ"',
        "Bearer REDACTED-abc",
        # The word "token" glued to a real value.
        "Authorization: Bearer token.abc123",
        "Bearer token-abc",
        # Scheme names are case-insensitive (RFC 9110 §11.1).
        "authorization: basic dXNlcjpwYXNz",
        "BASIC dXNlcjpwYXNz",
    ],
)
def test_guard_catches_known_leak_shapes(leak):
    assert secrets_in(leak)


@pytest.mark.parametrize(
    "redacted",
    [
        "Authorization: Bearer REDACTED",
        '"authorization": "REDACTED"',
        '"x-xet-access-token": "REDACTED"',
        '"accessToken": "REDACTED"',
        '{"text": "{\\"accessToken\\":\\"REDACTED\\"}"}',
        '["X-Xet-Access-Token", "REDACTED"]',
        '"sample": "eyJoZWxsbyI6ICJvcGVueGV0In0K"',
        "https://cas-bridge.xethub.hf.co/x?Expires=1&Policy=REDACTED&Signature=REDACTED&Key-Pair-Id=REDACTED",
        "https://s3/x?X-Amz-Credential=REDACTED&X-Amz-SignedHeaders=host%3Brange&X-Amz-Signature=REDACTED",
        "user-agent: hf_hub/2.2.0; hf_xet/1.7.0",
        "`assert_xet_download`, `xet_get`, `parse_xet_file_data_from_response`",
        "Requests carry `Authorization: Bearer <token>`; a Bearer token is required.",
        '`{"casUrl":"…","exp":<epoch>,"accessToken":"<JWT>"}`',
        'WWW-Authenticate: Bearer realm="Authentication required", charset="UTF-8"',
        "https://cdn/x?expires=1&signature=REDACTED&key-pair-id=REDACTED&policy=REDACTED",
        "https://s3/x?X-Amz-Security-Token=REDACTED&x-amz-credential=REDACTED",
        '"Proxy-Authorization": "REDACTED"',
        '"authorization": "Bearer REDACTED"',
        '["Authorization", "Bearer REDACTED"]',
        "Proxy-Authorization: REDACTED",
        "Cookie: REDACTED",
        "HTTP/1.1 200 OK\nset-cookie: REDACTED",
        "the Set-Cookie header is dropped",
        'LFS-Authenticate: Basic realm="x"',
        "Basic auth is accepted",
        "a Basic base64 pair",
        "Basic credentials from the helper",
        "> Authorization: REDACTED",
        "> > Authorization: Bearer REDACTED",
        "  - Cookie: REDACTED",
        "https://cdn/x?Signature=REDACTED&Expires=1",
        '"Authorization": "Bearer REDACTED"',
        "a Bearer token is required",
        "basic auth is accepted",
        'lfs-authenticate: basic realm="x"',
    ],
)
def test_guard_allows_redacted_placeholders(redacted):
    assert not secrets_in(redacted)
