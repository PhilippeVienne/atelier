-- Jalon M5, tache 5.5.2 : suivi des approbations Human-in-the-Loop
-- (`AwaitHitlApproval`, voir pm_engine/nodes.py). `langgraph-checkpoint-postgres`
-- (tache 5.3.3) persiste l'ETAT COMPLET du graphe par `thread_id`, mais
-- n'offre aucune facon d'enumerer "tous les threads actuellement en
-- pause a AwaitHitlApproval" (son API est concue autour d'un acces PAR
-- thread_id connu a l'avance, pas d'un listing global) -- cette table,
-- tenue par `pm_engine.runner` (pas un noeud du graphe lui-meme), comble
-- ce manque pour l'interface Dashboard (`GET /reviews`).
--
-- Meme convention RLS que `project_memories` (tache 5.3.2) : `tenant_id`
-- isole les deploiements multi-organisation partageant la meme base
-- `atelier_pm`, PAS les utilisateurs individuels d'une meme organisation
-- (qui voient tous les memes revues en attente -- l'autorisation reelle
-- reste "authentifie aupres de cette instance Atelier", voir
-- pm_engine.auth).
CREATE TABLE IF NOT EXISTS pm_reviews (
    thread_id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    repo TEXT NOT NULL,
    issue_number INT NOT NULL,
    pr_url TEXT,
    status TEXT NOT NULL DEFAULT 'pending', -- 'pending' | 'approved' | 'rejected'
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_pm_reviews_tenant_status
  ON pm_reviews (tenant_id, status);

ALTER TABLE pm_reviews ENABLE ROW LEVEL SECURITY;
ALTER TABLE pm_reviews FORCE ROW LEVEL SECURITY;

DROP POLICY IF EXISTS tenant_isolation ON pm_reviews;
CREATE POLICY tenant_isolation ON pm_reviews
  USING (tenant_id = current_setting('app.current_tenant', true))
  WITH CHECK (tenant_id = current_setting('app.current_tenant', true));
