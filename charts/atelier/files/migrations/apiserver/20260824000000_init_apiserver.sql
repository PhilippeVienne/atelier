-- Schema initial d'`atelier-api-server` (base `atelier_apiserver`, voir
-- deploy/dev/postgres/README.md). Isolation multi-tenant par Row Level
-- Security (RLS) sur `owner_subject` (le sujet JWT proprietaire, meme champ
-- que `WorkshopSpec.owner_subject`) : chaque requete applicative doit
-- positionner `SET LOCAL app.current_tenant = '<owner_subject>'` avant de
-- lire/ecrire ces tables (voir docs/specs/01-keycloak-forgejo-postgres.md).
--
-- Zero secret stocke ici (voir meme spec, "Zero Secret en Base
-- Relationnelle") : uniquement des metadonnees et des references.

CREATE TABLE session_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_subject TEXT NOT NULL,
    workshop_name TEXT NOT NULL,
    event_type TEXT NOT NULL,
    message TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX session_logs_owner_subject_idx ON session_logs (owner_subject);
CREATE INDEX session_logs_workshop_name_idx ON session_logs (workshop_name);

-- FORCE (pas seulement ENABLE) : sans elle, RLS ne s'applique jamais au
-- proprietaire de la table — en dev, `atelier_admin` (le role de connexion,
-- voir deploy/dev/postgres/README.md) est justement ce proprietaire.
ALTER TABLE session_logs ENABLE ROW LEVEL SECURITY;
ALTER TABLE session_logs FORCE ROW LEVEL SECURITY;

CREATE POLICY session_logs_tenant_isolation ON session_logs
    USING (owner_subject = current_setting('app.current_tenant', true))
    WITH CHECK (owner_subject = current_setting('app.current_tenant', true));

CREATE TABLE audit_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_subject TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX audit_events_owner_subject_idx ON audit_events (owner_subject);

ALTER TABLE audit_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE audit_events FORCE ROW LEVEL SECURITY;

CREATE POLICY audit_events_tenant_isolation ON audit_events
    USING (owner_subject = current_setting('app.current_tenant', true))
    WITH CHECK (owner_subject = current_setting('app.current_tenant', true));
