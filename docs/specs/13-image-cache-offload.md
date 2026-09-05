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

- **`image-builder`** (`publish_to_cache`) : après publication locale, téléverse aussi vers S3 (`bucket` dédié, à distinguer de `bucket_snapshots` — voir §3.2).
  **Correction (vérifiée en implémentant, pas seulement en concevant)** : une première rédaction de cette spec affirmait que ceci permettrait d'éviter un rebuild `envbuilder` complet en vérifiant d'abord une présence S3 — **faux**. Le digest content-addressé est calculé par `sha256_file` **après** la construction complète du `rootfs.ext4` (`main()`, `crates/image-builder/src/main.rs`) : il n'existe aucun moyen de le connaître AVANT de lancer `build_via_microvm`/`export_image_filesystem`/`package_ext4`, donc aucune vérification préalable (S3 ou PVC local) ne peut éviter cette construction pour un Workshop qui en a besoin. Le gain réel de l'offload S3 ici est plus modeste mais réel : une fois `publish_to_cache` exécuté (donc le digest connu), le résultat survit à une éviction ultérieure du PVC local (§3.1/8.5) — sans quoi une éviction perdrait l'artefact pour de bon, obligeant à reconstruire depuis zéro le jour où ce même digest redeviendrait nécessaire (Workshop suspendu longtemps, ou nouveau Workshop dont la construction produit par coïncidence le même contenu).
- **`vm-supervisor`** (reprise d'un `Workshop` suspendu) : si `ATELIER_VM_SNAPSHOT_DIR` ne contient plus le snapshot localement (évincé), le retélécharger depuis S3 avant de tenter `Vm::restore_persisted` ; seulement si absent des deux, rebooter à froid (comportement actuel, dégradé mais jamais bloquant).

### 3.1. Éviction du PVC local

**Implémenté et vérifié empiriquement.** Boucle périodique côté `controller` (`crates/controller/src/eviction.rs`, `tokio::spawn` indépendant du cycle de réconciliation d'un Workshop précis, toutes les 15 minutes) : le controller ne monte jamais le PVC lui-même (même raison que `cleanup_snapshot_cache`, 8.1), la passe crée donc un Job éphémère (image `minio/mc`, déjà utilisée par `s3-init-job.yaml`) qui calcule la taille totale du cache monté ; au-delà d'un plafond configurable (`.Values.imageCache.evictionThresholdGb`/`ATELIER_IMAGE_CACHE_EVICTION_THRESHOLD_GB`, `15` par défaut pour un PVC nominal de `20Gi`), supprime les entrées `sha256_*` les moins récemment modifiées, **dont la présence sur S3 est confirmée** (`mc stat`) avant suppression — jamais une éviction optimiste. Désactivée entièrement si `S3_BUCKET_IMAGE_CACHE` n'est pas configuré.

**Piège trouvé en vérifiant, pas en concevant** : l'image `minio/mc` est quasi-distroless — ni `find` ni `awk` n'y sont installés (vérifié en exécutant un script de test dans un pod jetable contre le cluster de dev réel avant d'écrire la version finale). Le script s'appuie donc uniquement sur `stat -c '%Y %n' | sort -n` pour le tri par ancienneté et un accumulateur shell pur (`du -sb ... | while read -r size _; do t=$((...+size)); done`) pour la somme, plutôt que la combinaison `find -printf`/`awk` utilisée par `s3-init-job.yaml` pour un besoin voisin.

Scénario de sécurité vérifié explicitement (pas seulement supposé) : une entrée ancienne mais absente de S3 est bien **préservée** malgré son ancienneté ; une entrée plus récente mais confirmée sur S3 est **évincée** à sa place. Le Job réel, créé par le vrai binaire `controller`, se termine `Complete` en quelques secondes.

### 3.2. Bucket S3 dédié, pas de réutilisation de `bucket_snapshots` pour les deux usages

`S3_BUCKET_SNAPSHOTS` existe déjà pour les snapshots Firecracker (nom cohérent avec son usage prévu). Le cache d'images, lui, est un usage différent (contenu content-addressé, partagé entre Workshops, pas scopé à un seul) — **nouvelle variable `S3_BUCKET_IMAGE_CACHE`**, plutôt que de surcharger `bucket_snapshots` avec une convention de préfixe de clé qui mélangerait deux natures de données dans un seul bucket. Cohérent avec la séparation déjà faite entre `S3_BUCKET_SESSIONS` et `S3_BUCKET_SNAPSHOTS`. Contrairement aux deux autres, **optionnelle** (`Option<String>` dans `S3Config`) : seul `image-builder` en a besoin, `api-server` (qui charge cette même configuration pour les sessions) n'a aucun usage pour ce bucket.

**Piège trouvé en implémentant (8.3), même famille que `llm_proxy_pod_addr`/`OpenBaoConfig::pod_addr`** : le `controller` ne parle jamais directement à S3, il ne fait que retransmettre sa propre configuration au Job `image-builder` qu'il crée — mais en développement, le controller tourne HORS cluster (port-forward, `S3_ENDPOINT=http://127.0.0.1:9000`), alors que le Job, lui, tourne DANS le cluster, où cette adresse ne désigne rien. Vérifié empiriquement en inspectant le Job réellement créé : le premier essai transmettait bien `127.0.0.1:9000`, injoignable depuis le pod. Corrigé par un `s3_pod_endpoint` dédié sur `ReconcileCtx` (nouvelle variable `ATELIER_S3_POD_ENDPOINT`), égal à `s3.endpoint` par défaut — aucun effet en production, où les deux adresses coïncident déjà.

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
