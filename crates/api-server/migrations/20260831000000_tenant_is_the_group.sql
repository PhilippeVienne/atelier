-- Le tenant du RLS devient le GROUPE proprietaire, plus le sujet.
--
-- Depuis `docs/specs/07-groupes.md`, un Workshop appartient a un groupe :
-- l'acces est accorde a ses membres, pas a son createur. Tant que le RLS
-- restait indexe sur le sujet, deux barrieres independantes cohabitaient —
-- autorisation par groupe cote API, isolation par individu cote base — et
-- auraient diverge au premier oubli. Une seule notion de tenant, donc.
--
-- La colonne est RENOMMEE plutot que reutilisee telle quelle : `owner_subject`
-- contenant desormais un nom de groupe aurait ete un piege pour le prochain
-- lecteur. `tenant` ne prejuge pas de ce qu'est le locataire, ce qui est
-- exactement l'intention.
--
-- ⚠️ Lignes existantes : elles portent un SUJET et deviennent donc invisibles
-- une fois le tenant passe au groupe. En developpement c'est sans
-- consequence (ces tables ne servent qu'a l'historique d'executions). Sur une
-- instance reelle, il faut une migration de donnees decidee avec les groupes
-- cibles — non automatisable sans connaitre l'organisation, et volontairement
-- pas tentee ici.

ALTER TABLE session_logs RENAME COLUMN owner_subject TO tenant;
ALTER INDEX session_logs_owner_subject_idx RENAME TO session_logs_tenant_idx;
DROP POLICY session_logs_tenant_isolation ON session_logs;
CREATE POLICY session_logs_tenant_isolation ON session_logs
    USING (tenant = current_setting('app.current_tenant', true))
    WITH CHECK (tenant = current_setting('app.current_tenant', true));

ALTER TABLE audit_events RENAME COLUMN owner_subject TO tenant;
ALTER INDEX audit_events_owner_subject_idx RENAME TO audit_events_tenant_idx;
DROP POLICY audit_events_tenant_isolation ON audit_events;
CREATE POLICY audit_events_tenant_isolation ON audit_events
    USING (tenant = current_setting('app.current_tenant', true))
    WITH CHECK (tenant = current_setting('app.current_tenant', true));

ALTER TABLE exec_commands RENAME COLUMN owner_subject TO tenant;
ALTER INDEX exec_commands_owner_subject_idx RENAME TO exec_commands_tenant_idx;
DROP POLICY exec_commands_tenant_isolation ON exec_commands;
CREATE POLICY exec_commands_tenant_isolation ON exec_commands
    USING (tenant = current_setting('app.current_tenant', true))
    WITH CHECK (tenant = current_setting('app.current_tenant', true));
