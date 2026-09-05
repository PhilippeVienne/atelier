-- Socle HITL (Human-in-the-Loop), tache 9.5 (spec
-- docs/specs/14-devex-cli-simulateurs-hitl.md §5.2) : une demande
-- d'approbation par action sensible (extension d'allowlist, secret,
-- validation de PR...), expirant automatiquement (fail-closed) si personne
-- ne decide dans le TTL.
--
-- `category`/`status` en TEXT avec CHECK plutot qu'un type ENUM Postgres :
-- meme convention que `exec_commands.status` (voir
-- `20260824000001_mcp_exec_commands.sql`) — un ENUM Postgres impose une
-- migration ALTER TYPE pour chaque valeur ajoutee plus tard, un CHECK se
-- modifie par une migration ordinaire.
--
-- `tenant` (= groupe proprietaire du Workshop, meme convention que
-- `session_logs`/`audit_events`/`exec_commands` depuis
-- `20260831000000_tenant_is_the_group.sql`) plutot que `workshop_name` seul
-- pour le RLS : c'est l'appartenance au groupe qui donne le droit de
-- decider, pas le nom du Workshop.
CREATE TABLE hitl_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant TEXT NOT NULL,
    workshop_name TEXT NOT NULL,
    category TEXT NOT NULL CHECK (category IN ('ALLOWLIST_EXPANSION', 'SECRET_REQUEST', 'PR_GATEWAY', 'SHELL_COMMAND')),
    requested_by TEXT NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    status TEXT NOT NULL DEFAULT 'PENDING' CHECK (status IN ('PENDING', 'APPROVED', 'REJECTED', 'EXPIRED')),
    decided_by TEXT,
    decision_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (now() + INTERVAL '15 minutes'),
    decided_at TIMESTAMPTZ
);

CREATE INDEX hitl_requests_tenant_idx ON hitl_requests (tenant);
CREATE INDEX hitl_requests_workshop_idx ON hitl_requests (workshop_name, status);

ALTER TABLE hitl_requests ENABLE ROW LEVEL SECURITY;
ALTER TABLE hitl_requests FORCE ROW LEVEL SECURITY;

-- Deuxieme predicat `app.is_admin` : un administrateur de l'instance doit
-- pouvoir decider une demande d'un Workshop dont il n'est pas membre du
-- groupe proprietaire (meme logique que `list_workshops`, qui montre tous
-- les Workshops a un admin sans etre membre de leurs groupes). Positionne
-- explicitement par `crate::routes` seulement quand `Claims::has_role("admin")`
-- est vrai — jamais par defaut (`current_setting(..., true)` renvoie NULL,
-- pas 'true', si jamais positionne).
CREATE POLICY hitl_requests_tenant_isolation ON hitl_requests
    USING (tenant = current_setting('app.current_tenant', true) OR current_setting('app.is_admin', true) = 'true')
    WITH CHECK (tenant = current_setting('app.current_tenant', true) OR current_setting('app.is_admin', true) = 'true');
