"""Adaptation de dimension pour `project_memories.embedding` (Jalon M5,
taches 5.2.2/5.5.1) : partagee entre `pm_engine.nodes.index_knowledge`
(ecriture) et `pm_engine.rag` (lecture) — les deux DOIVENT completer les
vecteurs de la meme facon, sinon la similarite cosinus calculee par
`<=>` (pgvector) n'a plus de sens."""

from __future__ import annotations

PROJECT_MEMORIES_VECTOR_DIM = 1536
"""`project_memories.embedding` est un `VECTOR(1536)` (calibre sur
`text-embedding-3-small`, voir la migration) — le modele d'embedding dev
local (`all-minilm`/tache 5.0.2) produit des vecteurs de 384 dimensions.
Complete a zero jusqu'a 1536 (`pad_embedding`) plutot que de changer le
schema de cette table dev-uniquement : le "padding" par des zeros ne
modifie ni le produit scalaire ni la norme des vecteurs originaux — la
similarite cosinus entre deux vecteurs ainsi completes reste
MATHEMATIQUEMENT IDENTIQUE a celle des vecteurs 384-dimensions d'origine,
tant que TOUTES les lignes de la table sont completees de la meme facon
(ce qui est le cas ici, un seul modele d'embedding pour tout ce lot)."""


def pad_embedding(embedding: list[float]) -> list[float]:
    if len(embedding) > PROJECT_MEMORIES_VECTOR_DIM:
        raise ValueError(
            f"embedding de dimension {len(embedding)} > {PROJECT_MEMORIES_VECTOR_DIM}, "
            "troncature non implementee (perte de similarite non maitrisee)"
        )
    return embedding + [0.0] * (PROJECT_MEMORIES_VECTOR_DIM - len(embedding))


def embedding_literal(embedding: list[float]) -> str:
    """Format texte attendu par pgvector pour un litteral `VECTOR` dans une
    requete parametree (`$1::vector` ou une colonne `VECTOR` directement) :
    `[v1,v2,...]`."""
    return "[" + ",".join(str(v) for v in embedding) + "]"
