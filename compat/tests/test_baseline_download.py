"""Baseline: the stock Python client against the target, over the Xet path."""

import httpx
import pytest

from conftest import (
    REFERENCE_FILE,
    assert_xet_download,
    run_hub_download,
    sha256,
)


def test_xet_download_of_reference_csv_matches_sha256(target, hf_env):
    path = run_hub_download(target, hf_env, REFERENCE_FILE)

    assert sha256(path) == REFERENCE_FILE.sha256
    assert_xet_download(hf_env, REFERENCE_FILE)


def test_xet_proof_fails_when_xet_is_disabled(target, hf_env):
    """Negative control: the proof must reject a plain-HTTP download."""
    hf_env["HF_HUB_DISABLE_XET"] = "1"
    path = run_hub_download(target, hf_env, REFERENCE_FILE)

    assert sha256(path) == REFERENCE_FILE.sha256
    with pytest.raises(AssertionError, match="did not use the Xet path"):
        assert_xet_download(hf_env, REFERENCE_FILE)


def test_resolve_exposes_xet_hash(target):
    r = httpx.head(REFERENCE_FILE.resolve_url(target.endpoint), timeout=30)

    assert r.headers["x-xet-hash"] == REFERENCE_FILE.xet_hash
