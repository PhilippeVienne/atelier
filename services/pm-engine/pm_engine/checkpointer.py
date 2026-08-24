"""Fabrique du checkpointer PostgreSQL persistant pour LangGraph.

Jalon M5, tache 5.3.3 : `AsyncPostgresSaver` (package
`langgraph-checkpoint-postgres`) persiste l'etat complet du graphe
LangGraph (taches 5.2.x, pas encore implementees) dans la base
`atelier_pm`, permettant la reprise exacte au dernier checkpoint apres un
crash du worker (voir docs/specs/05-devfactory-pm-engine.md, section 1.2).

Ce module ne fait que la connexion + le `setup()` (creation idempotente
des tables `checkpoints`/`checkpoint_writes`/... propres a LangGraph) :
aucune logique de graphe n'est branchee ici (hors perimetre de ce lot).
"""

from __future__ import annotations

from contextlib import asynccontextmanager
from typing import AsyncIterator

from langgraph.checkpoint.postgres.aio import AsyncPostgresSaver


@asynccontextmanager
async def build_checkpointer(database_url: str) -> AsyncIterator[AsyncPostgresSaver]:
    """Ouvre une connexion et retourne un `AsyncPostgresSaver` pret a l'emploi.

    `database_url` doit pointer vers la base `atelier_pm` avec un role
    disposant des privileges DDL necessaires a `setup()` (idempotent :
    `CREATE TABLE IF NOT EXISTS`). En dev, `atelier_admin` -- voir
    `deploy/dev/postgres/README.md` pour la convention role
    admin/applicatif de ce projet.
    """
    async with AsyncPostgresSaver.from_conn_string(database_url) as checkpointer:
        await checkpointer.setup()
        yield checkpointer
