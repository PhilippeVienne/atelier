# Spécification Technique : Réutilisation des Images par Source

> **Statut** : Proposé (rédigé avant implémentation, conformément à la démarche déjà suivie pour les specs 08 à 13)
> **Principe cadre** : Conforme à [`00-architecture-principles-substitutability.md`](00-architecture-principles-substitutability.md). Prolonge [`13-image-cache-offload.md`](13-image-cache-offload.md), qui a rendu le cache d'images durable et borné mais pas réutilisable.
> **Date** : 2026-10-05
> **Auteur** : Équipe Atelier

---

## 1. Constat, vérifié empiriquement

Mesuré le 2026-10-05 sur `kind-atelier-dev` (controller et api-server lancés depuis les sources, commit `be66aa8`), avec `https://github.com/microsoft/vscode-remote-try-python`, 1 CPU et 768 Mi. Une seule exécution par mesure : ce sont des ordres de grandeur.

| Mesure | Résultat |
| :--- | :--- |
| Premier Workshop : création → microVM démarrée | environ 110 s, dont 105 s de build (le controller, hors cluster, n'a confirmé `Running` qu'une fois la route vers le réseau des pods ajoutée) |
| **Second Workshop, même dépôt, même révision** : création → `Running` | **106 s, dont 90 s en `BuildingImage`** |
| Pod parent créé → microVM démarrée, image prête | environ 4,5 s |
| Reprise d'un Workshop suspendu | environ 16 s |

Le second Workshop reconstruit entièrement une image qui existe déjà. Quatre causes indépendantes, chacune suffisante à elle seule :

1. **Le controller ne regarde que le Workshop courant.** Dans la branche `Running` de la boucle de réconciliation (`crates/controller/src/reconcile.rs`), tant que `status.imageDigest` de CE Workshop est absent, `ensure_image_build_job` crée un Job pour lui. Rien ne demande si un autre Workshop a déjà construit la même source.
2. **L'image est nommée d'après le Workshop.** `crates/image-builder/src/main.rs` pousse vers `<registre>/atelier-workshops/<nom du workshop>:latest` : la référence décrit qui construit, pas ce qui est construit.
3. **Le cache de couches suit ce même nom.** `crates/builder-vm-init/src/main.rs` dérive `ENVBUILDER_CACHE_REPO` de cette référence : il repart vide pour chaque nouveau Workshop. Le second build a pris 90 s contre 105 s pour le premier : aucun gain qui ressemble à une réutilisation de couches.
4. **La clé du cache est l'empreinte du fichier produit.** `publish_to_cache` indexe par le SHA-256 du `rootfs.ext4` fini. Deux builds de la même source ne produisent pas les mêmes octets (identifiants du système de fichiers, horodatages) : la clé diffère, et le cache « content-addressed » ne peut pas reconnaître un rebuild. La spec 13 l'avait déjà constaté (§3, « Correction ») sans pouvoir le résoudre : le digest n'est connu qu'après le build.

**Ce qui ne fait pas obstacle** : rien de secret ni de propre à un Workshop n'est gravé dans l'image. La clé SSH autorisée et le mot de passe de session sont récupérés au démarrage auprès du serveur de métadonnées du pod (`crates/net-proxy/src/metadata.rs`). Une image est donc partageable telle quelle.

**Coût supplémentaire, hors build** : `crates/firecracker/src/vm.rs` déclare le rootfs comme ressource **copiée** (`MovedResourceType::Copied`) et l'attache en écriture. L'image de l'essai pesait 2,5 Gio ; sa copie est une part plausible, mais non mesurée séparément, des 16 s entre `Provisioning` et `Running`.

---

## 2. Objectifs / non-objectifs

**Dans le périmètre :**
1. Une source déjà construite n'est jamais reconstruite : le second Workshop d'une même source passe directement à `Provisioning`.
2. Plusieurs Workshops de la même source créés en même temps attendent **un seul** build.
3. Deux révisions proches d'un même dépôt partagent leurs couches inchangées.

**Hors périmètre (reporté, voir §5) :**
- Rootfs en lecture seule avec une couche inscriptible par VM, pour ne plus copier l'image à chaque démarrage : changement plus profond (séquence de boot du guest, format des snapshots), qui mérite sa propre spécification.
- Démarrage de plusieurs Workshops depuis un même snapshot modèle.
- `rootfs.ext4` reproductible à l'octet près : utile, mais superflu dès que le build est indexé par sa source.

---

## 3. Décision d'architecture

### 3.1. Une clé de source

La **clé de source** d'une image est l'empreinte de tout ce qui détermine son contenu :

- l'URL du dépôt, normalisée ;
- le **commit** vers lequel la révision se résout. `HEAD`, valeur par défaut de `DevcontainerSource.revision`, n'est pas une clé : elle doit être résolue (`git ls-remote`) avant toute recherche ;
- le chemin du `devcontainer.json` ;
- une version des injections propres à `image-builder` (sshd, ttyd, code-server, init, configuration du proxy), incrémentée quand elles changent, pour invalider les anciennes images ;
- ce qui entre dans le rootfs depuis le cluster : le bundle de CA d'entreprise (spec 15), l'architecture du guest.

### 3.2. L'image devient une ressource à part entière

Nouvelle ressource personnalisée `WorkshopImage`, nommée par la clé de source, avec une phase (`Building`, `Ready`, `Failed`) et le digest produit. La boucle de réconciliation passe de « pas de digest → je construis » à :

1. résoudre la révision et calculer la clé de source ;
2. lire ou créer la `WorkshopImage` de cette clé ;
3. `Ready` → recopier son digest dans `status.imageDigest` et passer à `Provisioning` ;
4. `Building` → attendre (`requeue`) ;
5. absente → la créer, et avec elle l'unique Job de build, **propriété de l'image et non d'un Workshop**.

La concurrence est réglée par construction : la création de la ressource est l'unique point de décision, et le serveur d'API Kubernetes garantit qu'un seul des Workshops concurrents la crée.

Le PVC et S3 continuent de stocker les fichiers par digest de sortie (spec 13). La passe d'éviction (`crates/controller/src/eviction.rs`) gagne une règle simple : une entrée est évinçable quand aucune `WorkshopImage` ne la référence, ou quand celle qui la référence n'a servi à aucun Workshop depuis un délai configurable.

### 3.3. Des artefacts de registre nommés par la clé

Pousser vers `<registre>/atelier-images/<clé de source>` et utiliser un `ENVBUILDER_CACHE_REPO` commun. Indépendamment de §3.2, ce seul changement permet à une révision **différente** du même dépôt de réutiliser les couches inchangées.

---

## 4. Risques et questions ouvertes

- **Révisions mobiles.** Un Workshop créé sur `main` doit-il garder son image jusqu'à sa recréation, ou être reconstruit quand la branche avance ? Proposition : la clé est résolue une fois, à la création du Workshop, et consignée dans son statut ; un rafraîchissement est une action explicite.
- **Egress au moment du build.** Le build passe aujourd'hui par `spec.egressAllowlist` du Workshop (constaté pendant l'essai : avec une liste vide, la microVM builder ne joint pas `github.com` et le build échoue). Avec une image partagée, la liste de quel Workshop s'applique ? Une politique d'egress de build portée par l'image répondrait aussi au cas d'un environnement qui a besoin du réseau pour se construire et d'aucun pour s'exécuter.
- **Dépôts privés.** Les identifiants de build sont résolus par Workshop (`resolve_git_credentials`). Une image construite avec les accès d'un groupe ne doit pas être servie à un autre : le groupe propriétaire entre dans la clé de source, ou la lecture d'une `WorkshopImage` est soumise à la même vérification d'accès que le dépôt.
- **Clone de l'espace de travail.** `ensure_workspace_clone` grave le dépôt cible dans l'image et installe un rafraîchissement au démarrage. Cela reste correct avec une image indexée par commit, mais doit être revérifié.
- **Build en échec.** Une `WorkshopImage` en `Failed` ne doit pas bloquer définitivement tous les Workshops de cette source : prévoir une nouvelle tentative bornée, ou une expiration de l'état d'échec.

---

## 5. Suite envisagée, hors de cette spec

1. Rootfs en lecture seule et couche inscriptible par VM : le démarrage cesse de dépendre de la taille de l'image, et la taille de la couche devient le quota disque du Workshop.
2. Snapshot modèle par image, restauré dans chaque nouveau Workshop.
3. Recalage de l'horloge du guest à la reprise : constaté pendant le même essai, après une suspension de 40 s l'horloge du guest retardait d'environ 45 s sur celle de l'hôte.
