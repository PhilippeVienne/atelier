-- Schema initial d'`atelier-controller` (base `atelier_controller`, voir
-- deploy/dev/postgres/README.md). Pas de RLS ici (contrairement a
-- `atelier_apiserver`) : ces tables sont un usage interne du controller,
-- jamais exposees directement a un client externe.

-- Index du cache rootfs content-addressed (voir `crates/controller/src/storage.rs` :
-- le cache lui-meme reste un PVC/systeme de fichiers, cette table n'en est
-- qu'un index consultable pour, par exemple, du garbage collection base sur
-- `last_used_at` (pas encore implemente).
CREATE TABLE rootfs_cache_index (
    digest TEXT PRIMARY KEY,
    devcontainer_repo TEXT NOT NULL,
    devcontainer_revision TEXT NOT NULL,
    size_bytes BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Historique des transitions de phase observees par le controller pour
-- chaque Workshop (complementaire a `Workshop.status`, qui ne garde que
-- l'etat courant) — utile pour du diagnostic post-mortem sans dependre de
-- la retention des Events Kubernetes.
CREATE TABLE workshop_reconciliation_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    namespace TEXT NOT NULL,
    workshop_name TEXT NOT NULL,
    phase TEXT NOT NULL,
    message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX workshop_reconciliation_history_workshop_idx
    ON workshop_reconciliation_history (namespace, workshop_name);
