"""Verifie que le graphe complet (Jalon M5, tache 5.2.2) compile avec les
11 noeuds attendus, et le comportement pur de `AutoCorrectionLoop`
(aucune I/O : pas besoin d'infra reelle pour ce test)."""

from __future__ import annotations

from langgraph.checkpoint.memory import InMemorySaver

from pm_engine import nodes
from pm_engine.graph import build_graph
from pm_engine.state import PMWorkflowState


def test_graph_compiles_with_all_eleven_nodes() -> None:
    graph = build_graph(InMemorySaver())
    node_names = set(graph.get_graph().nodes.keys())
    expected = {
        "AnalyzeIssue",
        "PlanParallelTasks",
        "ProvisionWorkshop",
        "DelegateToClaudeCode",
        "RunDevcontainerTests",
        "AutoCorrectionLoop",
        "OpenPullRequest",
        "SuspendWhileWaitingReview",
        "AwaitHitlApproval",
        "MergeAndClose",
        "IndexKnowledge",
    }
    assert expected <= node_names


async def test_auto_correction_loop_increments_attempts_and_reinjects_the_error() -> None:
    state = PMWorkflowState(
        analysis="analyse initiale", error_trace="AssertionError: x != y", correction_attempts=1
    )
    update = await nodes.auto_correction_loop(state, {"configurable": {"deps": None}})

    assert update["correction_attempts"] == 2
    assert "AssertionError: x != y" in update["analysis"]
    assert "Tentative de correction 2" in update["analysis"]
