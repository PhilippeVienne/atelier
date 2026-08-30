"""Verifie `_run_tool_call` (`pm_engine.main`, outil `setup_mirror_project`
du chat PM — Jalon M5, "Projets") avec un `tool_call` construit a la main,
plutot qu'un vrai aller-retour LLM : le modele mock de dev
(`atelier-budget-test`) ne genere jamais de `tool_calls` reel
(`mock_response` court-circuite tout raisonnement, voir
`tests/test_llm_client.py::test_chat_with_tools_returns_the_full_assistant_message`),
donc c'est la seule facon de couvrir ce chemin pour de vrai (vraie instance
Forgejo, jamais de mock) sans un fournisseur LLM payant.

Necessite FORGEJO_URL/FORGEJO_TOKEN (voir deploy/dev/forgejo/README.md).
Skip si non disponible.
"""

from __future__ import annotations

import json
import os
import uuid

import httpx
import pytest

from pm_engine.deps import PmEngineDeps
from pm_engine.git_providers import ForgejoProvider
from pm_engine.llm_client import LlmClient
from pm_engine.main import _run_tool_call

FORGEJO_URL = os.environ.get("FORGEJO_URL", "http://127.0.0.1:3000")
FORGEJO_TOKEN = os.environ.get("FORGEJO_TOKEN")
FORGEJO_OWNER = os.environ.get("FORGEJO_OWNER", "atelier_admin")


def _skip_if_unavailable() -> None:
    if not FORGEJO_TOKEN:
        pytest.skip("FORGEJO_TOKEN non defini (voir deploy/dev/forgejo/README.md), test ignore")


def _tool_call(call_id: str, arguments: dict) -> dict:
    return {
        "id": call_id,
        "function": {"name": "setup_mirror_project", "arguments": json.dumps(arguments)},
    }


@pytest.fixture
async def deps():
    _skip_if_unavailable()
    provider = ForgejoProvider(FORGEJO_URL, FORGEJO_TOKEN)
    # Seuls `git_provider` (execute reellement) et le reste (jamais touche
    # par `_run_tool_call`, valeurs factices suffisent) sont necessaires ici.
    yield PmEngineDeps(
        git_provider=provider,
        llm_client=LlmClient("http://unused.invalid", "unused"),
        atelier_api_url="http://unused.invalid",
        mcp_token_provider=None,  # type: ignore[arg-type]
        db_pool=None,
    )
    await provider.aclose()


@pytest.mark.asyncio
async def test_run_tool_call_creates_a_real_mirror(deps: PmEngineDeps) -> None:
    repo_name = f"pm-engine-chat-mirror-{uuid.uuid4().hex[:8]}"
    try:
        result = json.loads(
            await _run_tool_call(
                deps,
                _tool_call(
                    "call_1",
                    {
                        "name": repo_name,
                        "source_url": "https://github.com/octocat/Hello-World.git",
                        "private": False,
                    },
                ),
            )
        )
        assert result["status"] == "ok"
        assert result["full_name"] == f"{FORGEJO_OWNER}/{repo_name}"
        assert result["private"] is False
    finally:
        async with httpx.AsyncClient(
            base_url=f"{FORGEJO_URL.rstrip('/')}/api/v1",
            headers={"Authorization": f"token {FORGEJO_TOKEN}"},
            timeout=30.0,
        ) as admin_client:
            await admin_client.delete(f"/repos/{FORGEJO_OWNER}/{repo_name}")


@pytest.mark.asyncio
async def test_run_tool_call_reports_forgejo_errors_without_raising(deps: PmEngineDeps) -> None:
    """URL source invalide : Forgejo la refuse (`clone_addr` non resolvable),
    `_run_tool_call` doit traduire l'echec en resultat JSON `status: error`
    (lisible/reformulable par le LLM), jamais laisser l'exception remonter
    et couper le flux SSE du chat."""
    result = json.loads(
        await _run_tool_call(
            deps,
            _tool_call(
                "call_1",
                {"name": "whatever", "source_url": "https://example.invalid/nope.git", "private": False},
            ),
        )
    )
    assert result["status"] == "error"
    assert result["message"]


@pytest.mark.asyncio
async def test_run_tool_call_rejects_unknown_tool(deps: PmEngineDeps) -> None:
    result = json.loads(
        await _run_tool_call(
            deps, {"id": "call_1", "function": {"name": "delete_everything", "arguments": "{}"}}
        )
    )
    assert result["status"] == "error"
    assert "delete_everything" in result["message"]
