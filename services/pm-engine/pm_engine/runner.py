"""Enveloppe `graph.ainvoke()`/`Command(resume=...)` (Jalon M5, taches
5.5.1/5.5.2) et tient a jour la table `pm_reviews` (voir
`services/pm-engine/migrations/20260825000000_pm_reviews.sql`) : cette
table n'est PAS un noeud du graphe (`pm_engine.nodes`) ni une source de
verite sur l'etat du workflow — `AsyncPostgresSaver` (tache 5.3.3) reste la
seule source de verite. Elle sert uniquement a l'interface Dashboard
`GET /reviews` (tache 5.5.2), qui a besoin d'enumerer "tous les threads
actuellement en pause a `AwaitHitlApproval`" — chose que
`langgraph-checkpoint-postgres` ne permet pas nativement (acces uniquement
par `thread_id` connu a l'avance)."""

from __future__ import annotations

import logging
from typing import Any

from langgraph.types import Command

from .deps import PmEngineDeps
from .state import initial_state

logger = logging.getLogger(__name__)


async def _upsert_pending_review(
    deps: PmEngineDeps, thread_id: str, repo: str, issue_number: int, pr_url: str | None
) -> None:
    async with deps.db_pool.acquire() as conn:  # type: ignore[attr-defined]
        async with conn.transaction():
            await conn.execute(
                "SELECT set_config('app.current_tenant', $1, true)", deps.pm_bot_subject
            )
            await conn.execute(
                "INSERT INTO pm_reviews (thread_id, tenant_id, repo, issue_number, pr_url, status) "
                "VALUES ($1, $2, $3, $4, $5, 'pending') "
                "ON CONFLICT (thread_id) DO UPDATE SET pr_url = EXCLUDED.pr_url",
                thread_id,
                deps.pm_bot_subject,
                repo,
                issue_number,
                pr_url,
            )


async def _mark_review_decided(deps: PmEngineDeps, thread_id: str, status: str) -> None:
    async with deps.db_pool.acquire() as conn:  # type: ignore[attr-defined]
        async with conn.transaction():
            await conn.execute(
                "SELECT set_config('app.current_tenant', $1, true)", deps.pm_bot_subject
            )
            await conn.execute(
                "UPDATE pm_reviews SET status = $1, decided_at = now() WHERE thread_id = $2",
                status,
                thread_id,
            )


def _run_config(deps: PmEngineDeps, thread_id: str) -> dict:
    return {"configurable": {"thread_id": thread_id, "deps": deps}}


async def start_workflow(
    graph: Any,
    deps: PmEngineDeps,
    thread_id: str,
    repo: str,
    issue_number: int,
    devcontainer_repo: str,
) -> dict:
    """Demarre un nouveau workflow pour un ticket (`thread_id` = un `str`
    stable choisi par l'appelant, ex: `f"{repo}#{issue_number}"` — un seul
    workflow actif par ticket). Si le graphe se suspend a
    `AwaitHitlApproval` (le seul point d'arret du graphe complet, voir
    `pm_engine.graph`), enregistre une revue en attente dans `pm_reviews`."""
    result = await graph.ainvoke(
        initial_state(repo, issue_number, devcontainer_repo), config=_run_config(deps, thread_id)
    )
    if "__interrupt__" in result:
        await _upsert_pending_review(deps, thread_id, repo, issue_number, result.get("pr_url"))
    return result


async def resume_review(
    graph: Any, deps: PmEngineDeps, thread_id: str, decision: str
) -> dict:
    """Reprend un workflow suspendu a `AwaitHitlApproval` avec la decision
    humaine (`"approved"`/`"rejected"`, voir `nodes.route_after_hitl`) et
    reflete la decision dans `pm_reviews` — meme en cas de rejet (pas de
    reprise ulterieure possible sur ce `thread_id`, `route_after_hitl`
    termine le graphe)."""
    if decision not in ("approved", "rejected"):
        raise ValueError(f"decision HITL invalide: {decision!r}")

    result = await graph.ainvoke(Command(resume=decision), config=_run_config(deps, thread_id))
    await _mark_review_decided(deps, thread_id, decision)
    return result


async def list_pending_reviews(deps: PmEngineDeps) -> list[dict]:
    async with deps.db_pool.acquire() as conn:  # type: ignore[attr-defined]
        async with conn.transaction():
            await conn.execute(
                "SELECT set_config('app.current_tenant', $1, true)", deps.pm_bot_subject
            )
            rows = await conn.fetch(
                "SELECT thread_id, repo, issue_number, pr_url, created_at "
                "FROM pm_reviews WHERE status = 'pending' ORDER BY created_at ASC"
            )
    return [dict(row) for row in rows]
