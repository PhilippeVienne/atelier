"""Verifie empiriquement, contre une vraie base `atelier_pm`, que
`AwaitHitlApproval` (Jalon M5, tache 5.2.2) suspend reellement le graphe
(`interrupt`, LangGraph) et reprend exactement a ce noeud, avec l'etat
complet restaure depuis le checkpoint PostgreSQL (tache 5.3.3), quand on
resume avec une decision humaine — pas un mock du mecanisme d'interruption,
le vrai `AsyncPostgresSaver` et les vraies fonctions
`nodes.await_hitl_approval`/`nodes.route_after_hitl`.

Graphe minimal (pas les 11 noeuds du graphe complet, qui necessitent une
microVM Firecracker live pour `DelegateToOpencode`/
`RunDevcontainerTests` — voir tests/test_nodes.py) : un noeud de depart qui
pose `pr_url`, puis `AwaitHitlApproval`, puis `MergeAndClose` (bouchonne
ici pour ne pas dependre de Forgejo) en cas d'approbation.

Necessite DATABASE_URL_PM (voir deploy/dev/postgres/README.md). Skip si
non disponible.
"""

from __future__ import annotations

import os
import uuid

import psycopg
import pytest
from langgraph.graph import END, START, StateGraph
from langgraph.types import Command

from pm_engine import nodes
from pm_engine.checkpointer import build_checkpointer
from pm_engine.state import PMWorkflowState

DATABASE_URL_PM = os.environ.get(
    "DATABASE_URL_PM",
    "postgresql://atelier_admin:dev-only-not-for-production@127.0.0.1:5433/atelier_pm",
)


async def _seed_pr_url(state: PMWorkflowState, config) -> dict:
    return {"pr_url": "http://example.invalid/pr/1"}


async def _fake_merge(state: PMWorkflowState, config) -> dict:
    return {"status": "merged"}


@pytest.mark.asyncio
async def test_await_hitl_approval_interrupts_and_resumes_with_the_real_checkpointer() -> None:
    try:
        async with build_checkpointer(DATABASE_URL_PM) as checkpointer:
            graph_builder = StateGraph(PMWorkflowState)
            graph_builder.add_node("SeedPrUrl", _seed_pr_url)
            graph_builder.add_node("AwaitHitlApproval", nodes.await_hitl_approval)
            graph_builder.add_node("MergeAndClose", _fake_merge)
            graph_builder.add_edge(START, "SeedPrUrl")
            graph_builder.add_edge("SeedPrUrl", "AwaitHitlApproval")
            graph_builder.add_conditional_edges(
                "AwaitHitlApproval",
                nodes.route_after_hitl,
                {"MergeAndClose": "MergeAndClose", "__end__": END},
            )
            graph_builder.add_edge("MergeAndClose", END)
            graph = graph_builder.compile(checkpointer=checkpointer)

            thread_id = str(uuid.uuid4())
            config = {"configurable": {"thread_id": thread_id}}

            # 1er appel : s'arrete a l'interruption, jamais atteint MergeAndClose.
            result = await graph.ainvoke(
                PMWorkflowState(repo="r", issue_number=1), config=config
            )
            assert "__interrupt__" in result
            interrupt_payload = result["__interrupt__"][0].value
            assert interrupt_payload["pr_url"] == "http://example.invalid/pr/1"

            state_before_resume = await graph.aget_state(config)
            assert state_before_resume.next == ("AwaitHitlApproval",)

            # 2eme appel, sur un objet `graph` totalement recree (simule un
            # worker different qui reprend ce thread_id apres un crash) :
            # l'etat complet (dont `pr_url`, pose AVANT l'interruption) doit
            # etre restaure depuis PostgreSQL, pas seulement la decision.
            async with build_checkpointer(DATABASE_URL_PM) as checkpointer2:
                graph_builder2 = StateGraph(PMWorkflowState)
                graph_builder2.add_node("SeedPrUrl", _seed_pr_url)
                graph_builder2.add_node("AwaitHitlApproval", nodes.await_hitl_approval)
                graph_builder2.add_node("MergeAndClose", _fake_merge)
                graph_builder2.add_edge(START, "SeedPrUrl")
                graph_builder2.add_edge("SeedPrUrl", "AwaitHitlApproval")
                graph_builder2.add_conditional_edges(
                    "AwaitHitlApproval",
                    nodes.route_after_hitl,
                    {"MergeAndClose": "MergeAndClose", "__end__": END},
                )
                graph_builder2.add_edge("MergeAndClose", END)
                resumed_graph = graph_builder2.compile(checkpointer=checkpointer2)

                final = await resumed_graph.ainvoke(Command(resume="approved"), config=config)

            assert final["hitl_decision"] == "approved"
            assert final["status"] == "merged"
            assert final["pr_url"] == "http://example.invalid/pr/1"
    # `psycopg.OperationalError` n'herite PAS d'`OSError` : la garde
    # d'origine ne l'attrapait pas, si bien que ce test ne se sautait pas
    # faute de PostgreSQL — il ECHOUAIT. Invisible tant qu'aucune CI ne
    # l'executait sur une machine sans base.
    except (OSError, psycopg.OperationalError) as exc:
        pytest.skip(f"PostgreSQL atelier_pm indisponible pour ce test: {exc}")
