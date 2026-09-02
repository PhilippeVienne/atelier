"""Construction du graphe LangGraph du PM (Jalon M5, tache 5.2.2) : cablage
des 12 noeuds de `pm_engine.nodes` selon le flux decrit par
`docs/specs/05-devfactory-pm-engine.md`, section 2 :

    AnalyzeIssue -> PlanParallelTasks
      -> [depot vierge] -> ExpandGreenfieldSpec -> ProvisionWorkshop
      -> [depot deja pourvu] -> ProvisionWorkshop
      -> DelegateToOpencode -> RunDevcontainerTests
      -> [tests ok, ou budget de corrections epuise] -> OpenPullRequest
      -> [tests en echec, budget restant] -> AutoCorrectionLoop -> DelegateToOpencode (boucle)
    OpenPullRequest -> SuspendWhileWaitingReview -> AwaitHitlApproval
      -> [approuve] -> MergeAndClose -> IndexKnowledge -> FIN
      -> [rejete] -> FIN

`ExpandGreenfieldSpec` n'ajoute un appel LLM que pour les tickets sur un
depot vierge (rare) : elle fixe l'architecture d'un projet parti de zero
AVANT de le confier a l'agent unique, plutot que de le laisser improviser
un point d'entree et un manifeste au hasard (voir `nodes.expand_greenfield_spec`).
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
    graph.add_node("ExpandGreenfieldSpec", nodes.expand_greenfield_spec)
    graph.add_node("ProvisionWorkshop", nodes.provision_workshop)
    graph.add_node("DelegateToOpencode", nodes.delegate_to_opencode)
    graph.add_node("IntegrateSubTasks", nodes.integrate_sub_tasks)
    graph.add_node("RunDevcontainerTests", nodes.run_devcontainer_tests)
    graph.add_node("AutoCorrectionLoop", nodes.auto_correction_loop)
    graph.add_node("OpenPullRequest", nodes.open_pull_request)
    graph.add_node("SuspendWhileWaitingReview", nodes.suspend_while_waiting_review)
    graph.add_node("AwaitHitlApproval", nodes.await_hitl_approval)
    graph.add_node("MergeAndClose", nodes.merge_and_close)
    graph.add_node("IndexKnowledge", nodes.index_knowledge)

    graph.add_edge(START, "AnalyzeIssue")
    graph.add_edge("AnalyzeIssue", "PlanParallelTasks")
    graph.add_conditional_edges(
        "PlanParallelTasks",
        nodes.route_after_plan,
        {"ExpandGreenfieldSpec": "ExpandGreenfieldSpec", "ProvisionWorkshop": "ProvisionWorkshop"},
    )
    graph.add_edge("ExpandGreenfieldSpec", "ProvisionWorkshop")
    graph.add_edge("ProvisionWorkshop", "DelegateToOpencode")
    # Les branches des sous-taches sont reunies AVANT de tester : chaque
    # Workshop ne contient que sa part, une suite de tests ne veut donc rien
    # dire tant que le travail parallele n'a pas ete integre.
    graph.add_edge("DelegateToOpencode", "IntegrateSubTasks")
    graph.add_edge("IntegrateSubTasks", "RunDevcontainerTests")
    graph.add_conditional_edges(
        "RunDevcontainerTests",
        nodes.route_after_tests,
        {"OpenPullRequest": "OpenPullRequest", "AutoCorrectionLoop": "AutoCorrectionLoop"},
    )
    graph.add_edge("AutoCorrectionLoop", "DelegateToOpencode")
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
