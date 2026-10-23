"""Captured fixtures must never carry credentials or signed URLs.

Fixtures are real exchanges with huggingface.co and object stores. Every
secret value in them is replaced by the literal `REDACTED`; parameter and
header names stay so the shape remains visible. A pattern hit is allowed
only when the value that follows it is that placeholder.
"""

import re
from pathlib import Path

import pytest

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures"

_SECRET_PATTERNS = {
    "HF user token": re.compile(r"hf_[A-Za-z0-9]{20,}"),
    "Xet token": re.compile(r"xet_[A-Za-z0-9]"),
    # `Bearer realm=` is the RFC 6750 challenge huggingface.co sends with a 401.
    "bearer credential": re.compile(r"Bearer (?!REDACTED|realm=)"),
    "URL signature": re.compile(r"Signature=(?!REDACTED)"),
    "S3 signature header": re.compile(
        r"X-Amz-Signature\"?\s*:\s*\"?(?!REDACTED)", re.IGNORECASE
    ),
    "S3 credential": re.compile(r"X-Amz-Credential=(?!REDACTED)"),
    "CloudFront key pair": re.compile(r"Key-Pair-Id=(?!REDACTED)"),
    "CloudFront policy": re.compile(r"[?&]Policy=(?!REDACTED)"),
    "CDN client address": re.compile(r"[?&]xip=(?!REDACTED)"),
    # Xet access tokens on huggingface.co are JWTs, not `xet_`-prefixed.
    "JWT": re.compile(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\."),
    # Headers appear as `"name": "value"` or as `["name", "value"]` pairs.
    "credential header": re.compile(
        r"\"(authorization|cookie|set-cookie|x-xet-access-token)\"\s*[:,]\s*"
        r"\"(?!REDACTED\")",
        re.IGNORECASE,
    ),
    # Also inside a JSON body that is itself stored as an escaped string.
    "token field": re.compile(
        r"\\?\"accessToken\\?\"\s*:\s*\\?\"(?!REDACTED\\?\")"
    ),
}


def secrets_in(text: str) -> list[str]:
    """Names and offsets of hits; never the matched value, so CI logs stay clean."""
    return [
        f"{name} at offset {match.start()}"
        for name, pattern in _SECRET_PATTERNS.items()
        for match in pattern.finditer(text)
    ]


def _fixture_files() -> list[Path]:
    return sorted(p for p in FIXTURES.rglob("*") if p.is_file())


def test_fixtures_exist():
    assert _fixture_files(), f"no fixtures under {FIXTURES}; did the path move?"


@pytest.mark.parametrize(
    "path", _fixture_files(), ids=lambda p: str(p.relative_to(FIXTURES))
)
def test_fixture_has_no_secrets(path):
    hits = secrets_in(path.read_text(encoding="utf-8", errors="replace"))
    assert not hits, (
        f"{path.relative_to(FIXTURES)} contains unredacted secrets: {hits[:5]}. "
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
        "xet_0123456789abcdef",
        "https://cas-bridge.xethub.hf.co/x?Expires=1&Policy=eyJ&Signature=Zm9v&Key-Pair-Id=K2",
        "https://s3/x?X-Amz-Credential=AKIA%2F&X-Amz-Signature=deadbeef",
        "https://us.aws.cdn.hf.co/xorbs/default/h?xip=abc",
        '"x-amz-signature": "deadbeef"',
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
        'WWW-Authenticate: Bearer realm="Authentication required", charset="UTF-8"',
    ],
)
def test_guard_allows_redacted_placeholders(redacted):
    assert not secrets_in(redacted)
