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

    # --- Escouades multi-Workshops (tache 12.3, spec docs/specs/16-
    # escouades-multi-agents-swarms-mesh.md §3.1/§3.2) : les trois champs
    # ci-dessous ne s'appliquent qu'a un decoupage backend/frontend LEGITIME
    # (chaque cote s'execute et se teste seul — voir le prompt de
    # `plan_parallel_tasks`), jamais a une dependance d'execution ordinaire.
    #
    # Declare par une sous-tache PRODUCTRICE (typiquement backend) : le port
    # sur lequel son propre serveur ecoute, expose aux autres Workshops de
    # la meme campagne (`Workshop.spec.exported_services`, tache 12.1).
    service_port: NotRequired[int]
    # Declare par une sous-tache PRODUCTRICE : chemin (dans son propre
    # depot/branche) d'un artefact de contrat (ex: "openapi.yaml") a
    # injecter comme contexte IMMUABLE dans le prompt de toute sous-tache
    # CONSOMMATRICE (spec 16 §3.1, "Publication du Contrat").
    contract_path: NotRequired[str]
    # Declare par une sous-tache CONSOMMATRICE (typiquement frontend) :
    # `id` de la sous-tache PRODUCTRICE dont elle a besoin — a la fois pour
    # recevoir son contrat (`contract_path`) et pour joindre son service en
    # HTTP au runtime (`Workshop.spec.allowed_internal_targets`, tache
    # 12.1), a l'alias `api.<workshop_name du producteur>.atelier.internal`.
    # DOIT apparaitre APRES la sous-tache productrice dans `plan` : les
    # sous-taches sont traitees dans l'ordre par `DelegateToOpencode`
    # (aucun fan-out parallele actuel, voir sa docstring), donc le contrat
    # de la productrice a deja ete pousse sur sa branche au moment ou
    # celle-ci est lue.
    depends_on: NotRequired[str]


class ReviewVerdict(TypedDict):
    """Sortie commune aux quatre roles consultatifs (Architecte, QA,
    Securite, Ops — voir docs/specs/08-equipe-it-consultative.md). Une
    reponse LLM non parsable degrade toujours vers `"approve"`, jamais vers
    `"request_changes"` : un modele qui repond mal ne doit pas bloquer
    indefiniment un run par accident (meme doctrine que le repli sur une
    tache unique de `plan_parallel_tasks` face a une reponse non-JSON)."""

    verdict: str  # "approve" | "request_changes"
    comments: list[str]


class QAVerdict(TypedDict):
    """Sortie du validateur QA post-merge (docs/specs/
    09-qa-validation-post-merge.md, tache 5.7.2). Volontairement DISTINCT
    de `ReviewVerdict` : le repli sur reponse non exploitable est inverse
    (`"fail"`, pas `"approve"`) — ce noeud terminal ne bloque plus rien
    (la fusion a deja eu lieu), un repli optimiste masquerait une
    incertitude reelle plutot que de la rendre visible."""

    verdict: str  # "pass" | "fail"
    comments: list[str]
    evidence_files: list[str]  # chemins relatifs, cote Workshop de QA


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

    # --- QAValidation ---
    qa_verdict: NotRequired[QAVerdict]
    # Cles S3 (bucket `atelier-qa-evidence`), pas des URL : l'exposition
    # Dashboard des preuves est hors perimetre de cette premiere version
    # (voir docs/specs/09-qa-validation-post-merge.md, section 8).
    qa_evidence_keys: NotRequired[list[str]]

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
