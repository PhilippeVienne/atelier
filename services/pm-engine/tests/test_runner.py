"""Verifie empiriquement, contre une vraie base `atelier_pm` (checkpointer
`AsyncPostgresSaver`, tache 5.3.3, ET table `pm_reviews`, tache 5.5.2), que
`pm_engine.runner` :
- enregistre une revue `pending` dans `pm_reviews` quand le graphe se
  suspend a `AwaitHitlApproval` ;
- la marque `approved`/`rejected` (avec `decided_at`) quand on la resout via
  `resume_review`, sur un objet `graph` totalement recree (simule un worker
  different qui reprend apres un crash, meme garantie que
  `tests/test_hitl_interrupt.py`).

Graphe minimal (memes raisons que `tests/test_hitl_interrupt.py`) : pas de
microVM Firecracker live necessaire.

Necessite DATABASE_URL_PM. Skip si non disponible."""

from __future__ import annotations

import os
import uuid

import asyncpg
import pytest
from langgraph.graph import END, START, StateGraph

from pm_engine import nodes, runner
from pm_engine.checkpointer import build_checkpointer
from pm_engine.deps import PmEngineDeps
from pm_engine.state import PMWorkflowState

DATABASE_URL_PM = os.environ.get(
    "DATABASE_URL_PM",
    "postgresql://atelier_admin:dev-only-not-for-production@127.0.0.1:5433/atelier_pm",
)


async def _seed_pr_url(state: PMWorkflowState, config) -> dict:
    return {"pr_url": "http://example.invalid/pr/runner-test"}


async def _fake_merge(state: PMWorkflowState, config) -> dict:
    return {"status": "merged"}


def _build_minimal_graph(checkpointer):
    graph_builder = StateGraph(PMWorkflowState)
    graph_builder.add_node("AnalyzeIssue", _seed_pr_url)
    graph_builder.add_node("AwaitHitlApproval", nodes.await_hitl_approval)
    graph_builder.add_node("MergeAndClose", _fake_merge)
    graph_builder.add_edge(START, "AnalyzeIssue")
    graph_builder.add_edge("AnalyzeIssue", "AwaitHitlApproval")
    graph_builder.add_conditional_edges(
        "AwaitHitlApproval",
        nodes.route_after_hitl,
        {"MergeAndClose": "MergeAndClose", "__end__": END},
    )
    graph_builder.add_edge("MergeAndClose", END)
    return graph_builder.compile(checkpointer=checkpointer)


@pytest.mark.asyncio
async def test_start_workflow_then_resume_review_tracks_pm_reviews() -> None:
    try:
        pool = await asyncpg.create_pool(DATABASE_URL_PM, min_size=1, max_size=2)
    except OSError as exc:
        pytest.skip(f"PostgreSQL atelier_pm indisponible pour ce test: {exc}")

    tenant_id = f"test-runner-{uuid.uuid4()}"
    thread_id = str(uuid.uuid4())
    deps = PmEngineDeps(
        git_provider=None,  # type: ignore[arg-type]
        llm_client=None,  # type: ignore[arg-type]
        atelier_api_url="unused",
        mcp_token_provider=None,  # type: ignore[arg-type]
        db_pool=pool,
        pm_bot_subject=tenant_id,
    )

    try:
        async with build_checkpointer(DATABASE_URL_PM) as checkpointer:
            graph = _build_minimal_graph(checkpointer)
            result = await runner.start_workflow(
                graph, deps, thread_id, repo="acme/widgets", issue_number=1,
                devcontainer_repo="acme/devcontainer",
            )
            assert "__interrupt__" in result

        pending = await runner.list_pending_reviews(deps)
        assert [r["thread_id"] for r in pending] == [thread_id]
        assert pending[0]["pr_url"] == "http://example.invalid/pr/runner-test"

        async with build_checkpointer(DATABASE_URL_PM) as checkpointer2:
            resumed_graph = _build_minimal_graph(checkpointer2)
            final = await runner.resume_review(resumed_graph, deps, thread_id, "approved")
            assert final["status"] == "merged"

        pending_after = await runner.list_pending_reviews(deps)
        assert pending_after == []

        async with pool.acquire() as conn:
            async with conn.transaction():
                await conn.execute(
                    "SELECT set_config('app.current_tenant', $1, true)", tenant_id
                )
                row = await conn.fetchrow(
                    "SELECT status, decided_at FROM pm_reviews WHERE thread_id = $1", thread_id
                )
        assert row["status"] == "approved"
        assert row["decided_at"] is not None
    finally:
        async with pool.acquire() as conn:
            async with conn.transaction():
                await conn.execute(
                    "SELECT set_config('app.current_tenant', $1, true)", tenant_id
                )
                await conn.execute("DELETE FROM pm_reviews WHERE thread_id = $1", thread_id)
        await pool.close()
