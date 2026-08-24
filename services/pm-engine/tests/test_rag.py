"""Verifie empiriquement, contre une vraie base `atelier_pm` et un vrai
LiteLLM dev (`embedding-dev-local`, tache 5.0.2), que `pm_engine.rag`
retrouve par similarite cosinus (`<=>`, pgvector) une memoire indexee au
prealable — memes primitives d'embedding (`pm_engine.embeddings`) que
`nodes.index_knowledge` (tache 5.2.2), donc coherentes avec l'ecriture
reelle du graphe.

Necessite DATABASE_URL_PM et LITELLM_MASTER_KEY. Skip si non disponible."""

from __future__ import annotations

import os
import uuid

import asyncpg
import pytest

from pm_engine.embeddings import embedding_literal, pad_embedding
from pm_engine.llm_client import LlmClient
from pm_engine.rag import search_memories

DATABASE_URL_PM = os.environ.get(
    "DATABASE_URL_PM",
    "postgresql://atelier_admin:dev-only-not-for-production@127.0.0.1:5433/atelier_pm",
)
LITELLM_URL = os.environ.get("LITELLM_URL", "http://127.0.0.1:4000")
LITELLM_MASTER_KEY = os.environ.get("LITELLM_MASTER_KEY")


@pytest.mark.asyncio
async def test_search_memories_finds_a_previously_indexed_memory() -> None:
    if not LITELLM_MASTER_KEY:
        pytest.skip("LITELLM_MASTER_KEY non defini, test ignore")
    try:
        pool = await asyncpg.create_pool(DATABASE_URL_PM, min_size=1, max_size=2)
    except OSError as exc:
        pytest.skip(f"PostgreSQL atelier_pm indisponible pour ce test: {exc}")

    tenant_id = f"test-rag-{uuid.uuid4()}"
    llm_client = LlmClient(LITELLM_URL, LITELLM_MASTER_KEY)
    content = "Resolu en ajoutant un index composite sur (tenant_id, status)."

    try:
        embedding = pad_embedding(await llm_client.embed("embedding-dev-local", content))
        async with pool.acquire() as conn:
            async with conn.transaction():
                await conn.execute(
                    "SELECT set_config('app.current_tenant', $1, true)", tenant_id
                )
                await conn.execute(
                    "INSERT INTO project_memories (tenant_id, project_id, content, metadata, embedding) "
                    "VALUES ($1, $2, $3, $4, $5)",
                    tenant_id,
                    "acme/widgets",
                    content,
                    "{}",
                    embedding_literal(embedding),
                )

        matches = await search_memories(
            pool, llm_client, "embedding-dev-local", tenant_id,
            "index manquant sur tenant_id et status", limit=3,
        )
        assert matches
        assert matches[0].content == content
        assert matches[0].repo == "acme/widgets"
        assert matches[0].distance < 0.5
    finally:
        async with pool.acquire() as conn:
            async with conn.transaction():
                await conn.execute(
                    "SELECT set_config('app.current_tenant', $1, true)", tenant_id
                )
                await conn.execute(
                    "DELETE FROM project_memories WHERE tenant_id = $1", tenant_id
                )
        await llm_client.aclose()
        await pool.close()
