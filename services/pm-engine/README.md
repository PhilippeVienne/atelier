# atelier-pm-engine

Moteur DevFactory & Project Manager autonome d'Atelier. Voir
[`docs/specs/05-devfactory-pm-engine.md`](../../docs/specs/05-devfactory-pm-engine.md)
pour l'architecture cible complete (LangGraph, Redis Streams, checkpointer
PostgreSQL, RAG `pgvector` avec RLS multi-tenant).

## Etat actuel (Jalon M5, taches 5.1.1/5.1.2 du plan)

Scaffolding uniquement : dependances du projet et un endpoint `/health`.
La machine d'etats LangGraph (taches 5.2.x), les adaptateurs Git (5.4.x)
et l'interface dashboard (5.5.x) ne sont **pas** implementes dans ce lot —
ils dependent du serveur MCP externe du Jalon M4 (`/v1/mcp`), pas encore
construit.

## Developpement local

```sh
cd services/pm-engine
uv venv .venv --python 3.12
uv pip install -e ".[dev]" --python .venv/bin/python

# Lancer le service (endpoint /health uniquement pour l'instant)
.venv/bin/uvicorn pm_engine.main:app --reload --port 8100
curl http://127.0.0.1:8100/health
# -> {"status":"ok"}

# Tests
.venv/bin/pytest
```

## Base de donnees `atelier_pm`

Voir `deploy/dev/postgres/README.md` pour l'instance PostgreSQL de dev
partagee et `migrations/` dans ce dossier pour le script d'initialisation
de la base `atelier_pm` (extension `vector`, table `project_memories` avec
RLS, voir tache 5.3.x du plan).

## Docker

```sh
docker build -t atelier-pm-engine:dev -f Dockerfile .
docker run --rm -p 8100:8100 atelier-pm-engine:dev
curl http://127.0.0.1:8100/health
```
