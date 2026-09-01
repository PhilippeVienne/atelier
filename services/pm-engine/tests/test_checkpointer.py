"""Verifie empiriquement, contre une vraie base atelier_pm, que
AsyncPostgresSaver persiste et relit un checkpoint (Jalon M5, tache 5.3.3).

Necessite l'instance PostgreSQL de dev reelle du depot (voir
deploy/dev/postgres/README.md) exposee sur DATABASE_URL_PM. Skip si non
disponible (pas de mock : un skip explicite, jamais un succes factice).
"""

from __future__ import annotations

import os
import uuid

import psycopg
import pytest

from pm_engine.checkpointer import build_checkpointer

DATABASE_URL_PM = os.environ.get(
    "DATABASE_URL_PM",
    "postgresql://atelier_admin:dev-only-not-for-production@127.0.0.1:5433/atelier_pm",
)


@pytest.mark.asyncio
async def test_checkpoint_roundtrip() -> None:
    try:
        async with build_checkpointer(DATABASE_URL_PM) as checkpointer:
            thread_id = str(uuid.uuid4())
            config = {"configurable": {"thread_id": thread_id, "checkpoint_ns": ""}}
            checkpoint = {
                "v": 1,
                "id": str(uuid.uuid4()),
                "ts": "2026-08-24T00:00:00+00:00",
                "channel_values": {"issue_status": "AnalyzeIssue"},
                "channel_versions": {},
                "versions_seen": {},
                "pending_sends": [],
            }
            await checkpointer.aput(config, checkpoint, {"source": "test", "step": 1, "parents": {}}, {})

            restored = await checkpointer.aget(config)
            assert restored is not None
            assert restored["channel_values"]["issue_status"] == "AnalyzeIssue"
    # `psycopg.OperationalError` n'herite PAS d'`OSError` : la garde
    # d'origine ne l'attrapait pas, si bien que ce test ne se sautait pas
    # faute de PostgreSQL — il ECHOUAIT. Invisible tant qu'aucune CI ne
    # l'executait sur une machine sans base.
    except (OSError, psycopg.OperationalError) as exc:
        pytest.skip(f"PostgreSQL atelier_pm indisponible pour ce test: {exc}")
