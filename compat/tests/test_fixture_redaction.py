"""Captured fixtures and research notes must never carry credentials or signed URLs.

Fixtures are real exchanges with huggingface.co and object stores, and the
research notes quote them. Every secret value is replaced by the literal
`REDACTED`; parameter and header names stay so the shape remains visible. A
pattern hit is allowed only when the value that follows it is that
placeholder.
"""

import re
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
FIXTURES = REPO / "compat" / "fixtures"
SCANNED = (FIXTURES, REPO / "docs" / "research")

_SECRET_PATTERNS = {
    "HF user token": re.compile(r"hf_[A-Za-z0-9]{20,}"),
    # A bare `xet_[A-Za-z0-9]` also hits code identifiers quoted in the notes
    # (`xet_get`, `assert_xet_download`); a token is one long unbroken run.
    "Xet token": re.compile(r"(?<![A-Za-z0-9_])xet_[A-Za-z0-9]{20,}"),
    # `Bearer realm=` is the RFC 6750 challenge huggingface.co sends with a 401;
    # `<token>`-style placeholders and the word "token" appear in prose.
    "bearer credential": re.compile(r"Bearer (?!REDACTED|realm=|<[^>]+>|token\b)"),
    "URL signature": re.compile(r"Signature=(?!REDACTED)", re.IGNORECASE),
    "S3 signature header": re.compile(
        r"X-Amz-Signature\"?\s*:\s*\"?(?!REDACTED)", re.IGNORECASE
    ),
    "S3 credential": re.compile(r"X-Amz-Credential=(?!REDACTED)", re.IGNORECASE),
    "S3 session token": re.compile(
        r"X-Amz-Security-Token=(?!REDACTED)", re.IGNORECASE
    ),
    "CloudFront key pair": re.compile(r"Key-Pair-Id=(?!REDACTED)", re.IGNORECASE),
    "CloudFront policy": re.compile(r"[?&]Policy=(?!REDACTED)", re.IGNORECASE),
    "CDN client address": re.compile(r"[?&]xip=(?!REDACTED)"),
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
        r"^(authorization|proxy-authorization|cookie|set-cookie|x-xet-access-token)"
        r":(?![ \t]*(Bearer )?REDACTED)",
        re.IGNORECASE | re.MULTILINE,
    ),
    # Also inside a JSON body that is itself stored as an escaped string.
    "token field": re.compile(
        r"\\?\"accessToken\\?\"\s*:\s*\\?\"(?!(REDACTED|<[^>\"]+>)\\?\")"
    ),
}


def secrets_in(text: str) -> list[str]:
    """Names and offsets of hits; never the matched value, so CI logs stay clean."""
    return [
        f"{name} at offset {match.start()}"
        for name, pattern in _SECRET_PATTERNS.items()
        for match in pattern.finditer(text)
    ]


def _scanned_files() -> list[Path]:
    return sorted(p for root in SCANNED for p in root.rglob("*") if p.is_file())


@pytest.mark.parametrize("root", SCANNED, ids=lambda p: str(p.relative_to(REPO)))
def test_scanned_directories_exist(root):
    assert any(root.rglob("*")), f"nothing under {root}; did the path move?"


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
    ],
)
def test_guard_allows_redacted_placeholders(redacted):
    assert not secrets_in(redacted)
