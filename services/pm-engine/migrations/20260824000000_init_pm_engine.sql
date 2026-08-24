-- Migration d'initialisation de la base `atelier_pm` (Jalon M5, taches
-- 5.3.1/5.3.2). A executer avec un role superutilisateur (`atelier_admin`,
-- meme convention que `crates/api-server/migrations/` et
-- `crates/controller/migrations/`) CONTRE LA BASE `atelier_pm` (pas
-- `postgres` : `CREATE DATABASE` ne peut pas etre execute dans le meme
-- script/transaction que le reste, voir deploy/dev/postgres/README.md).
--
-- services/pm-engine n'a pas encore de mecanisme de migration etabli
-- (pas de sqlx, pas d'Alembic a ce stade) : ce fichier SQL numerote,
-- execute a la main ou par un script `scripts/migrate.sh` simple, suffit
-- pour ce lot. A remplacer par un outil de migration Python en bonne et
-- due forme (Alembic) des que la logique metier (5.2.x) sera implementee.
--
-- Usage (dev, voir deploy/dev/postgres/README.md pour le port-forward) :
--
--   kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d postgres \
--     -c 'CREATE DATABASE atelier_pm;'
--   kubectl exec -i atelier-postgres-dev -- psql -U atelier_admin -d atelier_pm \
--     < services/pm-engine/migrations/20260824000000_init_pm_engine.sql

-- Extension pgvector (image `pgvector/pgvector:pg16` deja utilisee par
-- deploy/dev/postgres/dev-pod.yaml : precompilee, pas de build requis).
CREATE EXTENSION IF NOT EXISTS vector;

-- Role applicatif non-superutilisateur dedie a `atelier_pm`, meme
-- convention et meme raison que `atelier_app` dans
-- deploy/dev/postgres/dev-pod.yaml : verifie empiriquement dans cette
-- session (voir docs/PROGRESS.md, entree Postgres/RLS) qu'un role
-- superutilisateur (ou BYPASSRLS) ignore silencieusement
-- ENABLE/FORCE ROW LEVEL SECURITY, quelle que soit la policy. La RLS de
-- `project_memories` n'a donc d'effet reel qu'avec ce role, jamais avec
-- `atelier_admin`.
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'atelier_pm_app') THEN
    CREATE ROLE atelier_pm_app LOGIN PASSWORD 'dev-only-not-for-production' NOSUPERUSER NOBYPASSRLS;
  END IF;
END
$$;

GRANT CONNECT ON DATABASE atelier_pm TO atelier_pm_app;
GRANT USAGE ON SCHEMA public TO atelier_pm_app;
ALTER DEFAULT PRIVILEGES IN SCHEMA public
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO atelier_pm_app;
-- Necessaire en plus des privileges sur les tables : BIGSERIAL cree une
-- sequence implicite, et `nextval()` (utilise par tout INSERT sans `id`
-- explicite) requiert USAGE dessus -- l'oubli produit un
-- "permission denied for sequence ..." malgre le GRANT INSERT ci-dessus
-- (verifie empiriquement dans cette session).
ALTER DEFAULT PRIVILEGES IN SCHEMA public
  GRANT USAGE, SELECT ON SEQUENCES TO atelier_pm_app;

-- Memoire RAG multi-tenant (docs/specs/05-devfactory-pm-engine.md,
-- section 1.3) : chaque ligne appartient a un tenant (organisation/projet
-- client), isole par RLS sur `SET LOCAL app.current_tenant` -- meme
-- convention que `audit_events`/`session_logs` dans
-- crates/api-server/migrations/.
--
-- VECTOR(1536) : dimension standard `text-embedding-3-small`/-ada-002
-- d'OpenAI, alignee sur la spec (section 8.3 du plan). Le modele
-- d'embedding dev leger (tache 5.0.2, `all-MiniLM-L6-v2`, 384
-- dimensions) ne peut donc PAS ecrire directement dans cette colonne sans
-- adaptation (troncature/padding ou re-projection) -- voir la limite
-- documentee dans docs/PROGRESS.md pour cette session.
CREATE TABLE IF NOT EXISTS project_memories (
    id BIGSERIAL PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    content TEXT NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    embedding VECTOR(1536) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Index vectoriel ivfflat (approximation, necessite ANALYZE apres
-- population pour un plan de requete efficace ; `lists = 100` est une
-- valeur de depart raisonnable pour un volume de dev, a ajuster en
-- production selon la cardinalite reelle). `vector_cosine_ops` : la
-- similarite cosinus est la metrique standard pour les embeddings
-- textuels OpenAI-compatibles.
CREATE INDEX IF NOT EXISTS idx_project_memories_embedding
  ON project_memories USING ivfflat (embedding vector_cosine_ops)
  WITH (lists = 100);

CREATE INDEX IF NOT EXISTS idx_project_memories_tenant
  ON project_memories (tenant_id);

-- RLS : ENABLE ne suffit pas seul (voir deploy/dev/postgres/README.md) --
-- FORCE est necessaire pour que la policy s'applique aussi au proprietaire
-- de la table (ici `atelier_admin`, qui reste neanmoins BYPASSRLS en tant
-- que superutilisateur : voir role `atelier_pm_app` ci-dessus pour une
-- verification reelle).
ALTER TABLE project_memories ENABLE ROW LEVEL SECURITY;
ALTER TABLE project_memories FORCE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS tenant_isolation ON project_memories;
CREATE POLICY tenant_isolation ON project_memories
  USING (tenant_id = current_setting('app.current_tenant', true))
  WITH CHECK (tenant_id = current_setting('app.current_tenant', true));
