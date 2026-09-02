"""`wait_for_exec_completion` doit echouer franchement quand l'execution
deleguee ne termine jamais — voir le commentaire de
`DEFAULT_TOTAL_TIMEOUT_S` dans `pm_engine.exec_client` pour le run reel
(2026-09-02) qui a motive ce garde-fou : un agent reste suspendu 1h20 sans
que rien, ni cote PM ni cote atelier, ne le signale, la connexion vers le
modele etant partie dans une socket dont la destination avait raccroche."""

from __future__ import annotations

import functools

import httpx
import pytest

import pm_engine.exec_client as exec_client
from pm_engine.oidc import OidcTokenProvider


class _StaticTokenProvider(OidcTokenProvider):
    def __init__(self) -> None:
        pass

    async def get_token(self) -> str:
        return "test-token"


async def _never_ending_sse(request: httpx.Request) -> httpx.Response:
    # Un flux SSE qui n'emet jamais `status`/`error` ET ne se ferme jamais :
    # exactement la situation d'un agent bloque sur une reponse qui
    # n'arrivera jamais (connexion vers le modele restee ouverte, plus rien
    # ne transite dessus).
    import asyncio

    async def body():
        while True:
            yield b": ping\n\n"
            await asyncio.sleep(0.05)

    return httpx.Response(200, headers={"content-type": "text/event-stream"}, content=body())


@pytest.mark.asyncio
async def test_une_execution_qui_ne_termine_jamais_echoue_apres_le_plafond(monkeypatch):
    transport = httpx.MockTransport(_never_ending_sse)
    monkeypatch.setattr(
        exec_client.httpx,
        "AsyncClient",
        functools.partial(httpx.AsyncClient, transport=transport),
    )

    result = await exec_client.wait_for_exec_completion(
        "http://api.atelier.local",
        _StaticTokenProvider(),
        "workshop-test",
        "exec-test",
        timeout_s=5.0,
        total_timeout_s=0.2,
    )

    assert result.status == "Failed"
    assert "execution abandonnee" in result.stderr
