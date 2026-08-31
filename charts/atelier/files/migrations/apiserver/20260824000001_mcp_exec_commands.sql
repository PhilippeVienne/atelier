-- Tache 4.2.2 (Jalon M4) : buffer PostgreSQL de `exec_in_workshop` (MCP),
-- pour decoupler l'execution d'une commande dans le guest de la connexion
-- du client MCP qui l'a demandee (reconnexion possible via
-- `GET /v1/workshops/{name}/exec/{id}/stream`, voir crate::exec).
--
-- `owner_subject` ajoute par rapport au schema de
-- `docs/specs/04-external-mcp-server.md` (qui ne le listait pas) : meme
-- convention RLS que `session_logs`/`audit_events` ci-dessus — sans cette
-- colonne, l'isolation par proprietaire ne reposerait que sur la logique
-- applicative (`crate::routes::ensure_owner`), jamais sur la base
-- elle-meme.
CREATE TABLE exec_commands (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_subject TEXT NOT NULL,
    workshop_name TEXT NOT NULL,
    command TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'Running', -- 'Running', 'Completed', 'Failed', 'Timeout'
    exit_code INT,
    stdout_buffer TEXT NOT NULL DEFAULT '',
    stderr_buffer TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX exec_commands_owner_subject_idx ON exec_commands (owner_subject);
CREATE INDEX exec_commands_workshop_name_idx ON exec_commands (workshop_name);

ALTER TABLE exec_commands ENABLE ROW LEVEL SECURITY;
ALTER TABLE exec_commands FORCE ROW LEVEL SECURITY;

CREATE POLICY exec_commands_tenant_isolation ON exec_commands
    USING (owner_subject = current_setting('app.current_tenant', true))
    WITH CHECK (owner_subject = current_setting('app.current_tenant', true));
