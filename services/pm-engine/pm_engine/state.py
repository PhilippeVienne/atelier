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


class ReviewVerdict(TypedDict):
    """Sortie commune aux quatre roles consultatifs (Architecte, QA,
    Securite, Ops — voir docs/specs/08-equipe-it-consultative.md). Une
    reponse LLM non parsable degrade toujours vers `"approve"`, jamais vers
    `"request_changes"` : un modele qui repond mal ne doit pas bloquer
    indefiniment un run par accident (meme doctrine que le repli sur une
    tache unique de `plan_parallel_tasks` face a une reponse non-JSON)."""

    verdict: str  # "approve" | "request_changes"
    comments: list[str]


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
    # Vrai uniquement quand le depot etait vierge (voir `_is_greenfield`) —
    # c'est ce flag, et non le nombre de sous-taches, qui declenche
    # `ExpandGreenfieldSpec` : un decoupage incoherent replie aussi a une
    # seule tache, mais dans un depot deja pourvu ou une spec n'apporte rien.
    greenfield: NotRequired[bool]

    # --- ExpandGreenfieldSpec ---
    greenfield_spec: NotRequired[str]

    # --- ReviewArchitecture ---
    # Compteur DISTINCT de `correction_attempts` : un decoupage refuse et un
    # code refuse sont des echecs de nature differente, confondre leurs
    # budgets bornerait a tort l'un par l'usure de l'autre (voir
    # docs/specs/08-equipe-it-consultative.md, section 4.4).
    architecture_review: NotRequired[ReviewVerdict]
    architecture_review_attempts: NotRequired[int]
    max_architecture_review_attempts: NotRequired[int]

    # --- ProvisionWorkshop / DelegateToOpencode / RunDevcontainerTests ---
    current_task_index: NotRequired[int]
    test_output: NotRequired[str]
    test_passed: NotRequired[bool]

    # --- AutoCorrectionLoop ---
    error_trace: NotRequired[str]
    correction_attempts: NotRequired[int]
    max_correction_attempts: NotRequired[int]

    # --- ReviewCode / ReviewSecurity / ReviewOps ---
    # Compteur DISTINCT de `correction_attempts`/`architecture_review_attempts`
    # (voir docs/specs/08-equipe-it-consultative.md, section 4.4) : un code
    # rejete par la revue, un code qui ne compile pas et un decoupage
    # malsain sont trois echecs de nature differente.
    code_review: NotRequired[ReviewVerdict]
    # `None` (pas seulement absent) signifie explicitement "pas declenche" :
    # ReviewSecurity/ReviewOps ne tournent que si le diff touche des chemins
    # sensibles/infra (`ReviewCode` calcule `security_review_needed`/
    # `ops_review_needed`, lus par `route_after_code_review`).
    security_review_needed: NotRequired[bool]
    security_review: NotRequired[ReviewVerdict | None]
    ops_review_needed: NotRequired[bool]
    ops_review: NotRequired[ReviewVerdict | None]
    review_attempts: NotRequired[int]
    max_review_attempts: NotRequired[int]

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
    max_architecture_review_attempts: int = 3,
    max_review_attempts: int = 3,
) -> PMWorkflowState:
    return PMWorkflowState(
        repo=repo,
        issue_number=issue_number,
        devcontainer_repo=devcontainer_repo,
        devcontainer_revision=devcontainer_revision,
        current_task_index=0,
        correction_attempts=0,
        max_correction_attempts=max_correction_attempts,
        architecture_review_attempts=0,
        max_architecture_review_attempts=max_architecture_review_attempts,
        review_attempts=0,
        max_review_attempts=max_review_attempts,
        phase="pending",
        status="running",
    )
