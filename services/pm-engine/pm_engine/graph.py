"""Construction du graphe LangGraph du PM (Jalon M5, tache 5.2.2 ; roles
consultatifs ReviewArchitecture/ReviewCode/ReviewSecurity/ReviewOps
ajoutes taches 5.6.3/5.6.4, voir docs/specs/08-equipe-it-consultative.md) :
cablage des noeuds de `pm_engine.nodes` selon le flux decrit par
`docs/specs/05-devfactory-pm-engine.md`, section 2 :

    AnalyzeIssue -> PlanParallelTasks
      -> [depot vierge] -> ExpandGreenfieldSpec -> ReviewArchitecture
      -> [depot deja pourvu] -> ReviewArchitecture
      -> [approuve, ou budget de revue epuise] -> ProvisionWorkshop
      -> [rejete, budget restant] -> ArchitectureReconsideration -> PlanParallelTasks (boucle)
    ProvisionWorkshop -> DelegateToOpencode -> RunDevcontainerTests
      -> [tests ok, ou budget de corrections epuise] -> ReviewCode
      -> [tests en echec, budget restant] -> AutoCorrectionLoop -> DelegateToOpencode (boucle)
    ReviewCode -> [chemins sensibles/infra detectes] -> ReviewSecurity et/ou ReviewOps (parallele)
      -> [rien de sensible/infra] -> ReviewGate (directement)
    ReviewSecurity/ReviewOps -> ReviewGate
    ReviewGate -> [tout approuve, ou budget de revue epuise] -> OpenPullRequest
      -> [rejete, budget restant] -> ReviewReconsideration -> DelegateToOpencode (boucle)
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
    graph.add_node("ReviewArchitecture", nodes.review_architecture)
    graph.add_node("ArchitectureReconsideration", nodes.prepare_architecture_reconsideration)
    graph.add_node("ProvisionWorkshop", nodes.provision_workshop)
    graph.add_node("DelegateToOpencode", nodes.delegate_to_opencode)
    graph.add_node("IntegrateSubTasks", nodes.integrate_sub_tasks)
    graph.add_node("RunDevcontainerTests", nodes.run_devcontainer_tests)
    graph.add_node("AutoCorrectionLoop", nodes.auto_correction_loop)
    graph.add_node("ReviewCode", nodes.review_code)
    graph.add_node("ReviewSecurity", nodes.review_security)
    graph.add_node("ReviewOps", nodes.review_ops)
    graph.add_node("ReviewGate", nodes.review_gate)
    graph.add_node("ReviewReconsideration", nodes.prepare_review_reconsideration)
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
        {"ExpandGreenfieldSpec": "ExpandGreenfieldSpec", "ReviewArchitecture": "ReviewArchitecture"},
    )
    graph.add_edge("ExpandGreenfieldSpec", "ReviewArchitecture")
    # Le seul des quatre roles consultatifs (docs/specs/08-...) qui se
    # prononce AVANT toute creation de Workshop : un decoupage rejete
    # reboucle sur PlanParallelTasks (via ArchitectureReconsideration, qui
    # injecte les objections), jamais directement sur ProvisionWorkshop —
    # un decoupage malsain doit etre refait a la source, pas corrige en
    # aval par les devs qui l'executent deja.
    graph.add_conditional_edges(
        "ReviewArchitecture",
        nodes.route_after_architecture_review,
        {
            "ProvisionWorkshop": "ProvisionWorkshop",
            "ArchitectureReconsideration": "ArchitectureReconsideration",
        },
    )
    graph.add_edge("ArchitectureReconsideration", "PlanParallelTasks")
    graph.add_edge("ProvisionWorkshop", "DelegateToOpencode")
    # Les branches des sous-taches sont reunies AVANT de tester : chaque
    # Workshop ne contient que sa part, une suite de tests ne veut donc rien
    # dire tant que le travail parallele n'a pas ete integre.
    graph.add_edge("DelegateToOpencode", "IntegrateSubTasks")
    graph.add_edge("IntegrateSubTasks", "RunDevcontainerTests")
    graph.add_conditional_edges(
        "RunDevcontainerTests",
        nodes.route_after_tests,
        # `route_after_tests` renvoie la cle "OpenPullRequest" (inchangee,
        # voir sa docstring) mais la fait desormais atterrir sur ReviewCode :
        # c'est exactement a cela que sert l'indirection `path_map`, la
        # decision "tests ok" et la decision "quel noeud vient ensuite"
        # restent deux choses distinctes.
        {"OpenPullRequest": "ReviewCode", "AutoCorrectionLoop": "AutoCorrectionLoop"},
    )
    graph.add_edge("AutoCorrectionLoop", "DelegateToOpencode")
    # `route_after_code_review` peut renvoyer une liste de plusieurs cles :
    # LangGraph declenche alors chacun des noeuds correspondants EN
    # PARALLELE (fan-out natif, sans `Send` explicite necessaire ici).
    graph.add_conditional_edges(
        "ReviewCode",
        nodes.route_after_code_review,
        {"ReviewSecurity": "ReviewSecurity", "ReviewOps": "ReviewOps", "ReviewGate": "ReviewGate"},
    )
    graph.add_edge("ReviewSecurity", "ReviewGate")
    graph.add_edge("ReviewOps", "ReviewGate")
    graph.add_conditional_edges(
        "ReviewGate",
        nodes.route_after_review,
        {"OpenPullRequest": "OpenPullRequest", "ReviewReconsideration": "ReviewReconsideration"},
    )
    graph.add_edge("ReviewReconsideration", "DelegateToOpencode")
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
