"""Verifie empiriquement, contre une vraie instance S3/RustFS de dev
(docs/specs/09-qa-validation-post-merge.md, tache 5.7.1), que
`pm_engine.evidence_store` televerse et relit un objet tel quel.

Necessite S3_ENDPOINT/S3_REGION/AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY/
S3_BUCKET_QA_EVIDENCE (voir deploy/dev/s3/README.md). Skip si non
disponible — pas de mock, un skip explicite plutot qu'un succes factice."""

from __future__ import annotations

import uuid

import pytest

from pm_engine.evidence_store import read_evidence, s3_config_from_env, upload_evidence


def _skip_if_unavailable():
    config = s3_config_from_env()
    if config is None:
        pytest.skip("S3_ENDPOINT non defini (voir deploy/dev/s3/README.md), test ignore")
    return config


@pytest.mark.asyncio
async def test_upload_then_read_evidence_round_trips_binary_content() -> None:
    config = _skip_if_unavailable()
    key = f"test/{uuid.uuid4().hex}.png"
    # Contenu binaire arbitraire (pas du texte) : une preuve reelle est un
    # PNG ou une sortie de requete HTTP quelconque, jamais garanti UTF-8.
    content = bytes(range(256))

    returned_key = await upload_evidence(config, key, content)
    assert returned_key == key

    read_back = await read_evidence(config, key)
    assert read_back == content


@pytest.mark.asyncio
async def test_s3_config_from_env_is_none_without_endpoint(monkeypatch) -> None:
    monkeypatch.delenv("S3_ENDPOINT", raising=False)
    assert s3_config_from_env() is None


@pytest.mark.asyncio
async def test_s3_config_from_env_raises_on_incomplete_config(monkeypatch) -> None:
    monkeypatch.setenv("S3_ENDPOINT", "http://127.0.0.1:9000")
    monkeypatch.delenv("S3_BUCKET_QA_EVIDENCE", raising=False)
    with pytest.raises(RuntimeError, match="S3_BUCKET_QA_EVIDENCE"):
        s3_config_from_env()
