"""Construction du graphe LangGraph du PM (Jalon M5, tache 5.2.2) : cablage
des 11 noeuds de `pm_engine.nodes` selon le flux decrit par
`docs/specs/05-devfactory-pm-engine.md`, section 2 :

    AnalyzeIssue -> PlanParallelTasks -> ProvisionWorkshop
      -> DelegateToClaudeCode -> RunDevcontainerTests
      -> [tests ok, ou budget de corrections epuise] -> OpenPullRequest
      -> [tests en echec, budget restant] -> AutoCorrectionLoop -> DelegateToClaudeCode (boucle)
    OpenPullRequest -> SuspendWhileWaitingReview -> AwaitHitlApproval
      -> [approuve] -> MergeAndClose -> IndexKnowledge -> FIN
      -> [rejete] -> FIN
"""

from __future__ import annotations

from langgraph.checkpoint.base import BaseCheckpointSaver
from langgraph.graph import END, START, StateGraph

from . import nodes
from .state import PMWorkflowState


def build_graph(checkpointer: BaseCheckpointSaver) -> object:
    graph = StateGraph(PMWorkflowState)

    graph.add_node("AnalyzeIssue", nodes.analyze_issue)
    graph.add_node("PlanParallelTasks", nodes.plan_parallel_tasks)
    graph.add_node("ProvisionWorkshop", nodes.provision_workshop)
    graph.add_node("DelegateToClaudeCode", nodes.delegate_to_claude_code)
    graph.add_node("RunDevcontainerTests", nodes.run_devcontainer_tests)
    graph.add_node("AutoCorrectionLoop", nodes.auto_correction_loop)
    graph.add_node("OpenPullRequest", nodes.open_pull_request)
    graph.add_node("SuspendWhileWaitingReview", nodes.suspend_while_waiting_review)
    graph.add_node("AwaitHitlApproval", nodes.await_hitl_approval)
    graph.add_node("MergeAndClose", nodes.merge_and_close)
    graph.add_node("IndexKnowledge", nodes.index_knowledge)

    graph.add_edge(START, "AnalyzeIssue")
    graph.add_edge("AnalyzeIssue", "PlanParallelTasks")
    graph.add_edge("PlanParallelTasks", "ProvisionWorkshop")
    graph.add_edge("ProvisionWorkshop", "DelegateToClaudeCode")
    graph.add_edge("DelegateToClaudeCode", "RunDevcontainerTests")
    graph.add_conditional_edges(
        "RunDevcontainerTests",
        nodes.route_after_tests,
        {"OpenPullRequest": "OpenPullRequest", "AutoCorrectionLoop": "AutoCorrectionLoop"},
    )
    graph.add_edge("AutoCorrectionLoop", "DelegateToClaudeCode")
    graph.add_edge("OpenPullRequest", "SuspendWhileWaitingReview")
    graph.add_edge("SuspendWhileWaitingReview", "AwaitHitlApproval")
    graph.add_conditional_edges(
        "AwaitHitlApproval",
        nodes.route_after_hitl,
        {"MergeAndClose": "MergeAndClose", "__end__": END},
    )
    graph.add_edge("MergeAndClose", "IndexKnowledge")
    graph.add_edge("IndexKnowledge", END)

    return graph.compile(checkpointer=checkpointer)
