# atelier-pm-engine

Moteur DevFactory & Project Manager autonome d'Atelier. Voir
[`docs/specs/05-devfactory-pm-engine.md`](../../docs/specs/05-devfactory-pm-engine.md)
pour l'architecture cible complete (LangGraph, Redis Streams, checkpointer
PostgreSQL, RAG `pgvector` avec RLS multi-tenant).

## Etat actuel (Jalon M5)

- **5.1.1/5.1.2** : scaffolding (dependances, `/health`).
- **5.2.1/5.2.2** : machine d'etats LangGraph complete (`pm_engine.graph`),
  les 20 noeuds (`pm_engine.nodes`) decrits par
  `docs/specs/05-devfactory-pm-engine.md`, section 8.2 — dont, depuis la
  tache 5.7.x, `QAValidation` (`nodes.run_qa_validation`), noeud de
  validation dynamique post-merge sur preuve S3 (`pm_engine.evidence_store`).
  Pilote Atelier via le vrai serveur MCP externe (`pm_engine.mcp_client`,
  Jalon M4), avec sa propre identite de service OIDC (`atelier-pm-bot`,
  `pm_engine.oidc`).
  **Limite assumee** : `DelegateToOpencode`/`RunDevcontainerTests`
  (`exec_in_workshop`) ne sont pas testes de bout en bout avec une vraie
  microVM Firecracker (aucun `atelier-controller` actif dans
  l'environnement de developpement au moment de cette session) — voir
  `docs/PROGRESS.md`.
- **5.4.1-5.4.3** : adaptateurs Git multi-forges (`pm_engine.git_providers`)
  et consommateur Redis Streams (`pm_engine.redis_consumer`).
- **5.5.x** (interface Dashboard "Ask Project Manager") : non implemente.

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

## Tests reels (aucun mock)

Chaque suite se `skip` proprement si sa dependance n'est pas disponible.
Variables d'environnement (valeurs de dev par defaut, voir les README de
chaque composant) :

```sh
export FORGEJO_URL=http://127.0.0.1:3000        FORGEJO_TOKEN=...            # deploy/dev/forgejo
export ATELIER_API_URL=http://127.0.0.1:8080                                 # atelier-api-server (Jalon M4)
export KEYCLOAK_TOKEN_URL=http://127.0.0.1:8090/realms/atelier/protocol/openid-connect/token
export KEYCLOAK_PM_BOT_SECRET=dev-only-not-for-production-pm-bot-secret      # deploy/dev/keycloak
export LITELLM_URL=http://127.0.0.1:4000        LITELLM_MASTER_KEY=...       # deploy/dev/llm-proxy
export REDIS_URL=redis://127.0.0.1:6379/0                                    # deploy/dev/redis
export DATABASE_URL_PM=postgresql://atelier_admin:dev-only-not-for-production@127.0.0.1:5433/atelier_pm

.venv/bin/pytest -v
```
