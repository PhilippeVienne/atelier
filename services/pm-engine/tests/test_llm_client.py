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
async def test_chat_with_tools_returns_the_full_assistant_message() -> None:
    """Le modele mock (`mock_response`) ne genere jamais de `tool_calls`
    reel (LiteLLM court-circuite l'appel avant tout raisonnement) : ce test
    couvre donc uniquement que `chat_with_tools` renvoie bien le message
    assistant complet (pas seulement `content`, contrairement a `chat()`),
    forme consommee par `pm_engine.main::chat`. Le vrai chemin `tool_calls`
    est couvert cote execution par `test_run_tool_call_*`
    (`tests/test_main_chat_tools.py`), avec un `tool_call` construit a la
    main plutot qu'un vrai aller-retour LLM."""
    _skip_if_unavailable()
    client = LlmClient(LITELLM_URL, LITELLM_MASTER_KEY)
    try:
        message = await client.chat_with_tools(
            "atelier-budget-test",
            [{"role": "user", "content": "hi"}],
            tools=[
                {
                    "type": "function",
                    "function": {
                        "name": "noop",
                        "description": "outil factice, jamais reellement invoque ici",
                        "parameters": {"type": "object", "properties": {}},
                    },
                }
            ],
        )
    except httpx.HTTPError as exc:
        pytest.skip(f"LiteLLM injoignable pour ce test: {exc}")
    else:
        assert message["content"] == "ok"
        assert message.get("tool_calls") in (None, [])
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
