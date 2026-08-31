"""Attend la fin d'une execution `exec_in_workshop` en consommant le flux
SSE de reconnexion (`GET /v1/workshops/{name}/exec/{id}/stream`, tache
4.2.3 cote atelier) — voir `crates/api-server/src/exec.rs::stream_handler`
pour le contrat exact des evenements (`stdout`/`stderr`/`status`/`error`)
consommes ici.

Utilise par `DelegateToClaudeCode`/`RunDevcontainerTests` (Jalon M5, tache
5.2.2) : ces deux noeuds ont besoin du resultat complet (stdout/stderr/exit
code) avant de pouvoir decider de la suite du graphe, contrairement a un
client interactif qui se contenterait de streamer vers un terminal.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field

import httpx
from httpx_sse import aconnect_sse

from .oidc import OidcTokenProvider


@dataclass
class ExecResult:
    status: str = "Running"
    exit_code: int | None = None
    stdout: str = ""
    stderr: str = field(default="")


async def wait_for_exec_completion(
    atelier_api_url: str,
    token_provider: OidcTokenProvider,
    workshop_name: str,
    execution_id: str,
    *,
    timeout_s: float = 600.0,
) -> ExecResult:
    """Se reconnecte au flux SSE jusqu'a recevoir l'evenement `status`
    (execution terminee) — le serveur rejoue depuis le debut du buffer a
    chaque connexion (voir `crate::exec::stream_handler`), donc un seul
    passage suffit ici (pas besoin de gerer une reconnexion sur coupure
    dans ce client, contrairement a un humain qui rouvrirait l'onglet).

    Prend le PROVIDER de jeton, pas un jeton : ses appelants bouclent sur
    plusieurs sous-taches et chaque attente peut durer un quart d'heure
    (Claude Code implemente une fonctionnalite). Un jeton recupere une fois
    avant la boucle etait deja expire a l'ouverture du flux de la sous-tache
    suivante — meme classe de bug que la session MCP, voir
    `pm_engine.mcp_client._OidcAuth`."""
    url = f"{atelier_api_url.rstrip('/')}/v1/workshops/{workshop_name}/exec/{execution_id}/stream"
    result = ExecResult()

    token = await token_provider.get_token()
    async with httpx.AsyncClient(
        headers={"Authorization": f"Bearer {token}"}, timeout=httpx.Timeout(timeout_s)
    ) as client:
        async with aconnect_sse(client, "GET", url) as event_source:
            async for sse in event_source.aiter_sse():
                if sse.event == "stdout":
                    result.stdout += sse.data
                elif sse.event == "stderr":
                    result.stderr += sse.data
                elif sse.event == "status":
                    payload = json.loads(sse.data)
                    result.status = payload["status"]
                    result.exit_code = payload.get("exitCode")
                    break
                elif sse.event == "error":
                    result.status = "Failed"
                    result.stderr += sse.data
                    break
                # "ping" : aucune donnee nouvelle depuis le dernier sondage
                # cote serveur, on continue simplement d'attendre.

    return result
