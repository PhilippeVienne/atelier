"""Etat du graphe LangGraph du PM (Jalon M5, tache 5.2.1).

Un seul `PMWorkflowState` par ticket traite (un `thread_id` de checkpoint
LangGraph par issue, voir `pm_engine.checkpointer` et
`docs/specs/05-devfactory-pm-engine.md`, section 8.2 du plan). Les cles
suivent la nomenclature des noeuds (`AnalyzeIssue`, `PlanParallelTasks`,
...) : chaque noeud ne lit/ecrit que les champs dont il a besoin, jamais
tout l'etat.
"""

from __future__ import annotations

from typing import NotRequired, TypedDict


class SubTask(TypedDict):
    """Une sous-tache issue de `PlanParallelTasks` : un perimetre de
    fichiers disjoint (`scope`) assigne a un Workshop dedie, pour eviter
    tout conflit entre agents paralleles (voir
    docs/specs/05-devfactory-pm-engine.md, section 1)."""

    id: str
    title: str
    scope: list[str]
    workshop_name: str
    branch_name: str


class PMWorkflowState(TypedDict):
    # --- Source (renseigne avant le premier noeud) ---
    repo: str
    issue_number: int
    devcontainer_repo: str
    devcontainer_revision: NotRequired[str]

    # --- AnalyzeIssue ---
    issue_title: NotRequired[str]
    issue_body: NotRequired[str]
    issue_url: NotRequired[str]
    analysis: NotRequired[str]

    # --- PlanParallelTasks ---
    plan: NotRequired[list[SubTask]]

    # --- ProvisionWorkshop / DelegateToClaudeCode / RunDevcontainerTests ---
    current_task_index: NotRequired[int]
    test_output: NotRequired[str]
    test_passed: NotRequired[bool]

    # --- AutoCorrectionLoop ---
    error_trace: NotRequired[str]
    correction_attempts: NotRequired[int]
    max_correction_attempts: NotRequired[int]

    # --- OpenPullRequest ---
    # Branches de sous-taches que `IntegrateSubTasks` n'a pas pu fusionner
    # dans celle de tete. Vide = integration complete.
    integration_conflicts: NotRequired[list[str]]
    pr_number: NotRequired[int]
    pr_url: NotRequired[str]
    # Nombre de fichiers modifies par la PR, ou `None` si le provider ne sait
    # pas repondre. `0` est une anomalie : voir le garde-fou d'`OpenPullRequest`.
    pr_changed_files: NotRequired[int | None]

    # --- AwaitHitlApproval ---
    hitl_decision: NotRequired[str]  # "approved" | "rejected"

    # --- IndexKnowledge ---
    knowledge_indexed: NotRequired[bool]

    # --- Observabilite ---
    phase: NotRequired[str]
    status: NotRequired[str]
    error: NotRequired[str]


def initial_state(
    repo: str,
    issue_number: int,
    devcontainer_repo: str,
    *,
    devcontainer_revision: str = "HEAD",
    max_correction_attempts: int = 3,
) -> PMWorkflowState:
    return PMWorkflowState(
        repo=repo,
        issue_number=issue_number,
        devcontainer_repo=devcontainer_repo,
        devcontainer_revision=devcontainer_revision,
        current_task_index=0,
        correction_attempts=0,
        max_correction_attempts=max_correction_attempts,
        phase="pending",
        status="running",
    )
