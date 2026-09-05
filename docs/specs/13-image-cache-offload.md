# Spécification Technique : Offload S3 du Cache d'Images et des Snapshots

> **Statut** : Proposé (rédigé avant implémentation, conformément à la démarche déjà suivie pour les specs 08/09/10/11/12)
> **Principe cadre** : Conforme à [`00-architecture-principles-substitutability.md`](00-architecture-principles-substitutability.md). Étend `crates/api-server/src/storage.rs::S3StorageBackend` (déjà utilisé pour les archives de session) plutôt que d'introduire un second client S3.
> **Date** : 2026-09-05
> **Auteur** : Équipe Atelier

---

## 1. Constat, vérifié empiriquement — et pas juste un `TODO` théorique

Le `TODO` de `crates/image-builder/src/main.rs::publish_to_cache` (« offload/reload vers S3 une fois le PVC trop rempli ») a été traité, jusqu'ici, comme un confort d'optimisation reporté sans urgence. Ce n'est pas le cas : en creusant ce chantier, **le disque de la machine de dev était à 96 % d'utilisation (829 Go/916 Go, 41 Go libres)**, et la cause directe est ce même PVC (`atelier-image-cache`, nominalement demandé à `20Gi`) :

1. **`local-path-provisioner` (le `StorageClass` par défaut d'un cluster kind) ne fait respecter AUCUN quota** : la taille demandée dans le `PersistentVolumeClaim` est purement indicative, le répertoire `hostPath` sous-jacent peut croître sans limite jusqu'à saturer le disque du nœud. Vérifié : `du -sh` sur le `hostPath` réel du PVC renvoyait **400 Go**, vingt fois la taille nominale.
2. **Une fuite réelle, pas seulement une croissance attendue** : `crates/controller/src/reconcile.rs::cleanup()` (le finalizer appelé à la suppression d'un `Workshop`) révoque le rôle OpenBao et la Virtual Key LiteLLM, mais **ne supprime jamais** le sous-répertoire `snapshots/<ns>_<name>` de ce même Workshop sur le cache partagé (`crates/controller/src/storage.rs::snapshot_cache_subdir`). Sur l'instance de dev, ces snapshots orphelins représentaient **103 Go**, pour un seul `Workshop` encore recensé par `kubectl get workshops` (les autres avaient déjà été supprimés — leurs snapshots leur ont survécu indéfiniment).
3. Le reste (**~286 Go**) est le cache d'images content-addressé lui-même (106 variantes de `rootfs.ext4`, ~2,7 Go chacune) — celui-là croît de façon *attendue* (une entrée par digest de devcontainer distinct jamais revu depuis), mais sans aucune éviction : jamais purgé, jamais borné.

Correction immédiate appliquée en attendant cette spec : le contenu du PVC a été vidé manuellement (`rm -rf` du contenu, pas du PVC lui-même) — disque redescendu de 96 % à 50 %. **Ce n'est pas une solution, seulement un pansement** : sans changement structurel, le même incident se reproduira.

**Prérequis déjà en place, jamais branché** : `crates/api-server/src/storage.rs::S3StorageBackend` connaît déjà `S3_BUCKET_SNAPSHOTS` (`bucket_snapshots`, chargé depuis l'environnement, provisionné en dev — `deploy/dev/s3`, bucket `atelier-snapshots`) — mais ce champ est marqué `#[allow(dead_code)]` avec le commentaire *« réservé aux futures tâches de snapshots »* : la structure existe, la logique d'upload/download pour ce cas d'usage n'a jamais été écrite (seule `upload_session_archive`/`get_session_stream`, pour les enregistrements de terminal, existe réellement).

---

## 2. Objectifs / non-objectifs

**Dans le périmètre :**
1. **Corriger la fuite (bug, pas juste une optimisation)** : `cleanup()` doit supprimer le snapshot d'un `Workshop` supprimé — que ce snapshot vive sur le PVC local, sur S3, ou les deux (voir §3).
2. Offload vers S3 des snapshots ET des images de cache, **avec le PVC local qui reste la source lue par Firecracker** (voir §3 pour pourquoi un stream S3 direct n'est pas une option) : le PVC devient un cache chaud à éviction, S3 la source durable.
3. Une politique d'éviction simple sur le PVC local : plafond de taille configurable, éviction LRU (par date de dernier accès) des entrées **déjà présentes sur S3** — jamais d'une entrée qui n'y est pas encore montée, sous peine de perte de données réelle.

**Hors périmètre (reporté) :**
- Un vrai `StorageClass` avec quota réellement appliqué (CSI avec `VolumeAttributesClass`/quotas de fichiers) : orthogonal, et n'empêcherait pas la fuite de snapshots à elle seule.
- Politiques de rétention configurables par Workshop/groupe (TTL, nombre de versions gardées) : un plafond de taille global suffit pour ce premier lot.
- Migration automatique d'un cache déjà rempli vers S3 au déploiement de cette fonctionnalité : le PVC vidé manuellement pendant cette session repart de zéro, rien à migrer sur l'instance de dev actuelle.

---

## 3. Décision d'architecture : PVC local = cache chaud, S3 = source durable — jamais de stream direct

Firecracker a besoin d'un **fichier local reel** pour son rootfs (`mmap`-backed) et pour charger un snapshot (`Vm::restore_persisted`, `crates/vm-supervisor/src/main.rs`) — un flux réseau ne peut pas remplacer ça. S3 ne peut donc pas se substituer au PVC : il en devient la source de secours, avec un flux **pull-to-local-puis-lecture**, jamais un accès direct depuis Firecracker.

Conséquence sur les deux composants concernés :

- **`image-builder`** (`publish_to_cache`) : après publication locale, téléverse aussi vers S3 (`bucket` dédié, à distinguer de `bucket_snapshots` — voir §3.2). Avant de lancer un rebuild complet via `envbuilder` (coûteux), vérifie d'abord si le digest existe déjà sur S3 : si oui, le retélécharge vers le PVC local plutôt que de reconstruire — c'est le vrai gain de cette spec, pas seulement « libérer de la place », mais éviter des rebuilds inutiles pour un devcontainer déjà construit une fois.
- **`vm-supervisor`** (reprise d'un `Workshop` suspendu) : si `ATELIER_VM_SNAPSHOT_DIR` ne contient plus le snapshot localement (évincé), le retélécharger depuis S3 avant de tenter `Vm::restore_persisted` ; seulement si absent des deux, rebooter à froid (comportement actuel, dégradé mais jamais bloquant).

### 3.1. Éviction du PVC local

Nouvelle passe périodique côté `controller` (même processus que la réconciliation, pas un CronJob séparé — cohérent avec le reste du projet qui évite les composants supplémentaires quand une boucle existe déjà) : avant `ensure_image_cache_pvc`, calcule la taille totale du répertoire de cache monté ; au-delà d'un plafond configurable (`.Values.imageCache.evictionThresholdGb`, ex. `15` pour un PVC nominal de `20Gi` — marge avant que `local-path-provisioner` ne sature réellement le nœud), supprime les entrées `sha256_*` les moins récemment accédées (`atime`/`mtime` du fichier `rootfs.ext4`) **dont la présence sur S3 est confirmée** avant suppression — jamais une éviction optimiste.

### 3.2. Bucket S3 dédié, pas de réutilisation de `bucket_snapshots` pour les deux usages

`S3_BUCKET_SNAPSHOTS` existe déjà pour les snapshots Firecracker (nom cohérent avec son usage prévu). Le cache d'images, lui, est un usage différent (contenu content-addressé, partagé entre Workshops, pas scopé à un seul) — **nouvelle variable `S3_BUCKET_IMAGE_CACHE`**, plutôt que de surcharger `bucket_snapshots` avec une convention de préfixe de clé qui mélangerait deux natures de données dans un seul bucket. Cohérent avec la séparation déjà faite entre `S3_BUCKET_SESSIONS` et `S3_BUCKET_SNAPSHOTS`.

### 3.3. Qui parle à S3 : `controller`, pas `image-builder`/`vm-supervisor` directement

`crates/api-server/src/storage.rs::S3StorageBackend` vit aujourd'hui dans `api-server`. Ni `image-builder` (Job Kubernetes éphémère) ni `vm-supervisor` (process dans le pod parent) n'ont de client S3 aujourd'hui. Deux options :
1. Dupliquer `S3StorageBackend` dans `crates/common` (partagé par les trois crates) — cohérent avec `atelier_common::telemetry`, déjà partagé de la même façon.
2. Faire passer l'upload/download PAR le `controller` (qui, lui, a déjà un accès réseau large et pourrait exposer un endpoint interne) — ajoute un saut réseau et un couplage inutile pour un simple transfert d'octets.

**Retenu : l'option 1.** Déplacer `StorageBackend`/`S3StorageBackend` de `crates/api-server/src/storage.rs` vers `crates/common/src/storage.rs`, `api-server` important désormais ce type depuis `atelier_common` plutôt que de le définir lui-même. `image-builder` et `vm-supervisor` gagnent la même dépendance `aws-sdk-s3` (déjà dans l'arbre de dépendances via `api-server`, pas un nouveau fournisseur).

---

## 4. Risques identifiés, non résolus par cette spec

- **Coût réseau d'un rebuild évité vs. d'un pull S3** : un pull S3 d'un `rootfs.ext4` de plusieurs Go reste coûteux (bien que moins qu'un rebuild `envbuilder` complet) — pas chiffré ici, à mesurer une fois implémenté.
- **Éviction concurrente** : si deux réconciliations tournent en parallèle (plusieurs réplicas du `controller`, non supporté aujourd'hui mais pas explicitement interdit), une passe d'éviction pourrait supprimer une entrée qu'un `image-builder` vient de commencer à lire. À garder en tête si le `controller` passe un jour multi-réplica (hors périmètre actuel, un seul réplica aujourd'hui).
- **Snapshots ET cache d'images continuent de partager le même PVC** (juste avec éviction en plus) : un pic de créations de snapshots pourrait encore, en théorie, remplir le disque plus vite que la passe d'éviction ne le libère si le plafond est mal calibré — à surveiller empiriquement une fois en place, pas un problème résolu par la seule éviction LRU.
