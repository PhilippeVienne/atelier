-- Tache 14.5 (Jalon M14) : le resultat d'exec rend la sortie telle que la
-- commande l'a ecrite. Un `TEXT` PostgreSQL refuse l'octet nul : l'ecriture
-- echouait en silence et la sortie revenait vide. Les tampons passent en
-- `BYTEA` (leur contenu actuel, deja en UTF-8, est conserve), et le signal
-- qui a termine une commande a sa propre colonne, avec les statuts
-- 'Killed' (signal) et 'Timeout' (plafond de duree atteint).
ALTER TABLE exec_commands
    ALTER COLUMN stdout_buffer DROP DEFAULT,
    ALTER COLUMN stderr_buffer DROP DEFAULT,
    ALTER COLUMN stdout_buffer TYPE BYTEA USING convert_to(stdout_buffer, 'UTF8'),
    ALTER COLUMN stderr_buffer TYPE BYTEA USING convert_to(stderr_buffer, 'UTF8'),
    ALTER COLUMN stdout_buffer SET DEFAULT ''::bytea,
    ALTER COLUMN stderr_buffer SET DEFAULT ''::bytea,
    ADD COLUMN signal TEXT;
