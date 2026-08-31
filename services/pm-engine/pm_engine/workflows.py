"""Lecture de l'etat des workflows PM, pour le suivi en direct depuis le
Dashboard (« mission control »).

Rien n'est instrumente ici : le graphe LangGraph persiste deja tout son etat
a chaque noeud dans le checkpointer PostgreSQL (`pm_engine.checkpointer`).
Ce module se contente de le RELIRE et de le mettre en forme. C'est
volontaire — un second chemin d'ecriture (table d'evenements maison,
compteurs...) pourrait diverger de la verite du graphe, alors que le
checkpoint EST cette verite, celle-la meme sur laquelle une reprise
redemarre.

Le pipeline dure une dizaine de minutes et ses phases longues
(`DelegateToClaudeCode`) ne produisent aucune transition intermediaire : la
progression fine (une microVM qui boote) se lit donc cote Workshops, pas
ici — voir `phase_index` et `PIPELINE_PHASES`, qui donnent au client de quoi
situer l'avancement sans inventer de pourcentage.
"""

from __future__ import annotations

from typing import Any

import asyncio

from .deps import PmEngineDeps
from .mcp_client import atelier_mcp_session, call_tool_json

# Ordre des phases telles que les noeuds les ecrivent dans `state["phase"]`
# (voir `pm_engine.graph`). Sert a situer un workflow dans son parcours ;
# `AutoCorrectionLoop` n'y figure pas : ce n'est pas une etape en avant mais
# un retour en arriere vers `DelegateToClaudeCode`, et l'afficher comme une
# progression donnerait une impression d'avancement alors qu'on recommence.
PIPELINE_PHASES: list[str] = [
    "AnalyzeIssue",
    "PlanParallelTasks",
    "ProvisionWorkshop",
    "DelegateToClaudeCode",
    "IntegrateSubTasks",
    "RunDevcontainerTests",
    "OpenPullRequest",
    "SuspendWhileWaitingReview",
    "AwaitHitlApproval",
    "MergeAndClose",
    "IndexKnowledge",
]


def phase_index(phase: str | None) -> int:
    """Rang de la phase dans `PIPELINE_PHASES`, ou -1 si inconnue (workflow
    a peine demarre, ou phase de correction)."""
    try:
        return PIPELINE_PHASES.index(phase or "")
    except ValueError:
        return -1


async def list_workflows(deps: PmEngineDeps, limit: int = 20) -> list[dict[str, Any]]:
    """Les workflows les plus recemment actifs, du plus recent au plus
    ancien.

    On interroge directement la table `checkpoints` : c'est la seule source
    qui connaisse TOUS les threads, y compris ceux dont le processus qui les
    a lances n'existe plus. `DISTINCT ON` retient le dernier checkpoint de
    chaque thread — un thread en compte des dizaines.
    """
    async with deps.db_pool.acquire() as conn:
        rows = await conn.fetch(
            """
            SELECT DISTINCT ON (thread_id) thread_id, checkpoint_id
            FROM checkpoints
            ORDER BY thread_id, checkpoint_id DESC
            """
        )
    # `checkpoint_id` est un UUIDv6/v7 monotone : le trier decroissant classe
    # bien du plus recemment actif au plus ancien, sans colonne de date.
    rows = sorted(rows, key=lambda r: r["checkpoint_id"], reverse=True)[:limit]
    return [{"thread_id": r["thread_id"]} for r in rows]


async def _workshop_phase(deps: PmEngineDeps, name: str) -> dict[str, Any]:
    """Phase courante d'une microVM du workflow.

    C'est le PM qui interroge, pas le Dashboard : les Workshops crees par le
    graphe appartiennent a l'identite de service `atelier-pm-bot`, et
    l'api-server refuse leur lecture a tout autre sujet (`ensure_owner`).
    Interroges depuis le navigateur, ils repondaient donc systematiquement
    « inconnu » — et la vue paraissait figee pendant toute la phase de
    developpement, la plus longue.

    Toute erreur vaut « pas encore de phase » : un Workshop peut ne pas
    encore exister, ou avoir ete supprime apres la revue. Ni l'un ni l'autre
    n'est une anomalie a faire remonter dans une vue de suivi.
    """
    try:
        async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
            status = await call_tool_json(session, "get_workshop_status", {"name": name})
        return {
            "name": name,
            "phase": (status or {}).get("phase"),
            "pod_name": (status or {}).get("podName"),
        }
    except Exception:
        return {"name": name, "phase": None, "pod_name": None}


async def get_workflow(graph: Any, deps: PmEngineDeps, thread_id: str) -> dict[str, Any] | None:
    """Etat courant d'un workflow, mis en forme pour l'affichage.

    Renvoie `None` si le thread n'existe pas — un thread_id inconnu est une
    absence, pas une erreur serveur.
    """
    snapshot = await graph.aget_state({"configurable": {"thread_id": thread_id}})
    values = getattr(snapshot, "values", None) or {}
    if not values:
        return None

    # Date du PREMIER checkpoint du thread : le vrai depart du workflow. Sans
    # elle, la vue ne peut qu'afficher un temps ecoule depuis l'ouverture de
    # la page — c'est-a-dire `00:00` sur un run demarre depuis dix minutes,
    # ce qui est faux et se voit tout de suite en demo.
    async with deps.db_pool.acquire() as conn:
        started_at = await conn.fetchval(
            """
            SELECT checkpoint->>'ts' FROM checkpoints
            WHERE thread_id = $1 ORDER BY checkpoint_id ASC LIMIT 1
            """,
            thread_id,
        )

    phase = values.get("phase")
    # `next` vide = le graphe n'a plus rien a executer : soit il est termine,
    # soit il est arrete sur une interruption (revue humaine). Les deux se
    # distinguent par la presence d'une PR en attente de decision.
    pending_nodes = list(getattr(snapshot, "next", ()) or ())

    # En parallele : une vue de suivi doit rester rapide meme avec plusieurs
    # microVM, et ces appels sont independants les uns des autres.
    workshops = await asyncio.gather(
        *(
            _workshop_phase(deps, task["workshop_name"])
            for task in (values.get("plan") or [])
            if task.get("workshop_name")
        )
    )

    return {
        "thread_id": thread_id,
        "started_at": started_at,
        "workshops": list(workshops),
        "repo": values.get("repo"),
        "issue_number": values.get("issue_number"),
        "issue_title": values.get("issue_title"),
        "issue_url": values.get("issue_url"),
        "analysis": values.get("analysis"),
        "phase": phase,
        "phase_index": phase_index(phase),
        "phases": PIPELINE_PHASES,
        "pending_nodes": pending_nodes,
        "plan": [
            {
                "id": task.get("id"),
                "title": task.get("title"),
                "scope": task.get("scope", []),
                "workshop_name": task.get("workshop_name"),
                "branch_name": task.get("branch_name"),
            }
            for task in values.get("plan", []) or []
        ],
        "correction_attempts": values.get("correction_attempts", 0),
        "max_correction_attempts": values.get("max_correction_attempts", 3),
        "test_passed": values.get("test_passed"),
        "test_output": values.get("test_output"),
        "integration_conflicts": values.get("integration_conflicts") or [],
        "pr_number": values.get("pr_number"),
        "pr_url": values.get("pr_url"),
        # Le garde-fou d'`OpenPullRequest` : `0` est une anomalie, `None` veut
        # dire « pas encore ouverte » ou « provider incapable de repondre ».
        "pr_changed_files": values.get("pr_changed_files"),
        "hitl_decision": values.get("hitl_decision"),
        "status": values.get("status"),
    }
