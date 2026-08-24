"""Recherche semantique dans `project_memories` (Jalon M5, tache 5.5.1,
"Ask Project Manager") : les memes primitives que
`pm_engine.nodes.index_knowledge` (embedding local, complete a zero
jusqu'a `VECTOR(1536)`), en lecture cette fois — `<=>` (distance cosinus
pgvector) exploite l'index `ivfflat` deja cree par la migration 5.3.2.

`tenant_id` : voir la note de `pm_engine.nodes.index_knowledge` — ce
n'est PAS l'identite de l'utilisateur qui pose la question, mais celle du
deploiement Atelier qui a ecrit la memoire (`atelier-pm-bot`) : tout
utilisateur authentifie aupres de CETTE instance peut interroger sa
memoire partagee (l'isolation RLS protege contre un futur deploiement
multi-organisation partageant la meme base, pas entre utilisateurs d'une
meme organisation)."""

from __future__ import annotations

from dataclasses import dataclass

from .embeddings import embedding_literal, pad_embedding
from .llm_client import LlmClient


@dataclass
class MemoryMatch:
    content: str
    repo: str
    distance: float


async def search_memories(
    db_pool: object,
    llm_client: LlmClient,
    embedding_model: str,
    tenant_id: str,
    query: str,
    *,
    limit: int = 5,
) -> list[MemoryMatch]:
    embedding = pad_embedding(await llm_client.embed(embedding_model, query))
    embedding_lit = embedding_literal(embedding)

    async with db_pool.acquire() as conn:  # type: ignore[attr-defined]
        async with conn.transaction():
            # `idx_project_memories_embedding` est cree avec `lists=100`
            # (calibre pour un volume de production) : avec `probes=1`
            # (defaut pgvector) et le tres faible volume de donnees en dev
            # (quelques lignes), la recherche approximative ivfflat rate
            # quasi systematiquement toute ligne — constate empiriquement
            # (`tests/test_rag.py` retournait `[]` malgre une ligne
            # pertinente presente). Remonter `probes` degrade le
            # rapport vitesse/rappel en gros volume mais reste largement
            # sous la latence acceptable d'un chat ; `SET LOCAL` limite
            # l'effet a cette seule transaction.
            await conn.execute("SET LOCAL ivfflat.probes = 10")
            await conn.execute("SELECT set_config('app.current_tenant', $1, true)", tenant_id)
            rows = await conn.fetch(
                "SELECT content, project_id, embedding <=> $1 AS distance "
                "FROM project_memories ORDER BY embedding <=> $1 LIMIT $2",
                embedding_lit,
                limit,
            )

    return [MemoryMatch(content=r["content"], repo=r["project_id"], distance=r["distance"]) for r in rows]
