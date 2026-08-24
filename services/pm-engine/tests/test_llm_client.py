"""Verifie `LlmClient` contre la vraie instance LiteLLM de dev (`chat` via
un modele mock LiteLLM natif — gratuit, deterministe, aucune cle payante
requise ; `embed` via Ollama, tache 5.0.2).

Necessite LITELLM_URL/LITELLM_MASTER_KEY (voir deploy/dev/llm-proxy/README.md).
Skip si non disponible.
"""

from __future__ import annotations

import os

import httpx
import pytest

from pm_engine.llm_client import LlmClient

LITELLM_URL = os.environ.get("LITELLM_URL", "http://127.0.0.1:4000")
LITELLM_MASTER_KEY = os.environ.get("LITELLM_MASTER_KEY")


def _skip_if_unavailable() -> None:
    if not LITELLM_MASTER_KEY:
        pytest.skip("LITELLM_MASTER_KEY non defini, test ignore")


@pytest.mark.asyncio
async def test_chat_returns_the_mock_response_content() -> None:
    _skip_if_unavailable()
    client = LlmClient(LITELLM_URL, LITELLM_MASTER_KEY)
    try:
        content = await client.chat("atelier-budget-test", [{"role": "user", "content": "hi"}])
    except httpx.HTTPError as exc:
        pytest.skip(f"LiteLLM injoignable pour ce test: {exc}")
    else:
        assert content == "ok"
    finally:
        await client.aclose()


@pytest.mark.asyncio
async def test_embed_returns_a_real_384_dim_vector() -> None:
    _skip_if_unavailable()
    client = LlmClient(LITELLM_URL, LITELLM_MASTER_KEY)
    try:
        vector = await client.embed("embedding-dev-local", "test")
    except httpx.HTTPError as exc:
        pytest.skip(f"LiteLLM/Ollama injoignable pour ce test: {exc}")
    else:
        assert len(vector) == 384
        assert any(v != 0.0 for v in vector)
    finally:
        await client.aclose()
