-- Persiste les cartes de tool-call du chat PM (Jalon M5, "elements
-- interactifs") : jusqu'ici seul le texte final d'un tour etait ecrit dans
-- `pm_chat_messages` (20260903000000_pm_chat_history.sql), les evenements
-- SSE `tool_call`/`tool_result` restaient EPHEMERES (voir la docstring de
-- `pm_engine.main::chat`) — un rechargement de page faisait donc
-- disparaitre la carte "Import du projet ..." alors que le texte qui la
-- suit restait visible, un tour visuellement incoherent avec lui-meme.
--
-- Tableau JSON `[{"id","name","arguments","result"}, ...]` plutot qu'une
-- table dediee : au plus quelques elements par tour (un seul outil expose
-- aujourd'hui), jamais interroges independamment de leur message —
-- une table normalisee n'apporterait rien ici.
ALTER TABLE pm_chat_messages
  ADD COLUMN IF NOT EXISTS tool_calls JSONB NOT NULL DEFAULT '[]'::jsonb;
