-- Historique persistant du chat PM ("Ask Project Manager", tache 5.5.1) :
-- avant cette table, `entries` (dashboard/app/pm/pm-chat.tsx) n'etait
-- qu'un `useState` React -- un rechargement de page ou un changement
-- d'onglet perdait toute la conversation, alors que `POST /chat` sait deja
-- rejouer un `history` complet pour ne pas re-presenter l'agent comme
-- amnesique (voir `pm_engine.main::ChatRequest.history`).
--
-- Cle d'isolation : `user_sub` (le `sub` du JWT utilisateur, PAS
-- `pm_bot_subject`) -- contrairement a `project_memories`/`pm_reviews`
-- (donnees partagees par toute l'organisation, isolees seulement entre
-- deploiements Atelier via `tenant_id`, voir `pm_engine.rag`), une
-- conversation de chat est personnelle : deux utilisateurs de la meme
-- instance ne doivent pas se lire mutuellement.
--
-- Pas de RLS ici (contrairement a `project_memories`/`pm_reviews`) : aucune
-- convention etablie dans ce projet pour un `current_setting` scope
-- utilisateur (seulement `app.current_tenant`, scope organisation) --
-- l'isolation reste donc appliquee cote application (`WHERE user_sub = $1`
-- systematique dans `pm_engine.main`), a durcir avec une policy RLS si un
-- besoin de defense en profondeur se confirme.
CREATE TABLE IF NOT EXISTS pm_chat_messages (
    id BIGSERIAL PRIMARY KEY,
    user_sub TEXT NOT NULL,
    -- Chaine vide = conversation "generale" (aucun projet cible), meme
    -- convention que `ChatRequest.repo` cote API.
    repo TEXT NOT NULL DEFAULT '',
    role TEXT NOT NULL, -- 'user' | 'assistant'
    content TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_pm_chat_messages_user_repo
  ON pm_chat_messages (user_sub, repo, created_at);
