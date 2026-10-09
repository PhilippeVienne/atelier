# Spécification Technique : Sessions pour Apprenants (besoins d'un produit de formation)

> **Statut** : Proposé (rédigé avant implémentation, conformément à la démarche déjà suivie pour les specs 08 à 13 et 18)
> **Principe cadre** : Conforme à [`00-architecture-principles-substitutability.md`](00-architecture-principles-substitutability.md). Complète [`18-reutilisation-images-par-source.md`](18-reutilisation-images-par-source.md), dont elle reprend deux questions ouvertes (egress de build, clone de l'espace de travail).
> **Date** : 2026-10-06
> **Auteur** : Équipe Atelier

---

## 1. Constat, vérifié empiriquement

Atelier a été essayé comme plan d'exécution d'un autre produit, [Mentor](https://github.com/PhilippeVienne/mentor), une plateforme de formation : chaque apprenant·e y reçoit un environnement (un dossier devcontainer) dans lequel un serveur lance des commandes et **vérifie** le résultat (code de sortie, sortie standard, fichiers). Deux passages ont eu lieu le 2026-10-06 sur `kind-atelier-dev` (controller et api-server lancés depuis les sources, commit `9eece24`, noyau invité 5.10.223). Un troisième passage, le 2026-10-09, a vérifié dans l'invité le dimensionnement du disque (§3.1, tâche 14.1) et mis au jour le constat 16. Le détail, les commandes et les journaux sont dans le dépôt de Mentor : [`doc/atelier-lab-validation.md`](https://github.com/PhilippeVienne/mentor/blob/main/doc/atelier-lab-validation.md).

**Ce qui marche.** Des commandes lancées par `exec_in_workshop` tournent sous l'uid 1000 dans une microVM, sans capacité, sans `sudo`, sans accès aux autres Workshops, au nœud ni aux services du cluster. Au second passage, cinq environnements de Mentor ont démarré sans modification et 27 labos sur 34 y ont été rejoués, étape par étape (vérifications en échec, solution, vérifications réussies) :

| Environnement (invité) | Atteint `Running` | Labos réussis | Étapes réussies |
| :--- | :--- | :--- | :--- |
| `git-basics` (1 CPU, 512 Mio) | oui | 7 sur 7 | 31 sur 31 |
| `python` (1 CPU, 512 Mio) | oui | 6 sur 6 | 31 sur 31 |
| `aws-cloud-practitioner` (1 CPU, 512 Mio, émulateur AWS local) | oui | 12 sur 12 | 59 sur 59 |
| `docker-hello` (2 CPU, 2 Gio, Docker dans l'invité) | oui | 1 sur 4 | 8 sur 19 |
| `docker-advanced` (2 CPU, 2 Gio, Docker dans l'invité) | oui | 1 sur 5 | 10 sur 26 |

**Ce qui a dû être contourné ou reste bloquant.** Chaque ligne est un constat, avec sa source : *observé* (une commande l'a montré) ou *lu* (dans le code, sans exécution).

| # | Constat | Source |
| :--- | :--- | :--- |
| 1 | Le disque d'une session vaut « image + 512 Mio » : 276 à 369 Mio libres selon l'image. `resources.disk` existe dans la CRD (`crates/common/src/crd.rs`) mais le controller ne le lit pas. Les deux cours Docker échouent surtout là-dessus (une image `postgres` ne se dépaquette pas). | observé (`dd` s'arrête à 352 Mio) ; lu pour `resources.disk` |
| 2 | Trois ports sont pris dans l'invité par les services injectés : 8080 (`code-server`), 7681 (`ttyd`), 2222 (`sshd`), codés dans `crates/image-builder/src/main.rs`. Un labo qui publie un service sur 8080 échoue (`failed to bind host port 0.0.0.0:8080`). | observé |
| 3 | Une image qui contient systemd sans `/sbin/init` (cas de toute image Debian où `openssh-server` est installé sans `systemd-sysv`) ne démarre jamais : le Workshop reste en `Provisioning`, sans erreur nulle part. | observé (plus de 10 minutes) |
| 4 | Sur une image sans systemd, `ttyd` et `code-server` tournent en **root** ; avec systemd, sous l'uid 1000. | observé (`ps` dans l'invité) |
| 5 | Les scripts de démarrage injectés ont besoin de `curl` et de `bash` dans l'image ; rien ne le vérifie au build. | lu (`inject_sshd`, `inject_terminal_and_ide`) |
| 6 | L'exec est taillé pour un agent : compte fixe `vscode` (`crates/api-server/src/exec.rs`), dossier de départ fixe, pas d'environnement, entrée standard jamais fermée (`cat` reste bloqué), plafond de durée global (20 minutes) et non par appel, sortie non bornée, lecture du résultat par sondage toutes les 300 ms. | observé, et lu pour les constantes |
| 7 | Résultat d'exec : la sortie d'erreur est perdue quand elle arrive dans le même intervalle de sondage que la sortie standard ; une sortie contenant un octet nul revient vide ; une commande tuée par un signal revient sans code de sortie, statut `Completed` ; un processus d'arrière-plan qui garde la sortie ouverte tient l'exec ouvert au-delà du plafond (602 s constatées). | observé |
| 8 | Le dépôt source entier est cloné dans l'image (`ensure_workspace_clone`), sous `/workspaces/<dépôt>`, lisible par la personne dans la session, et c'est là que s'ouvrent l'exec et VS Code. Pour un produit de formation, le dépôt contient les solutions. | observé |
| 9 | Une seule liste d'egress sert au build et à l'exécution. Mettre `spec.egressAllowlist` à `[]` sur un Workshop en marche n'a aucun effet ; après une suspension puis une reprise (environ 60 s), tout est bien coupé. | observé |
| 10 | Les champs `remoteUser`, `workspaceFolder`, `containerEnv`, `postStartCommand` et `hostRequirements` du `devcontainer.json` sont ignorés. Le dossier de travail déclaré par l'image appartient à root. | observé |
| 11 | Le noyau invité n'a ni `CONFIG_NF_TABLES` ni `CONFIG_IP_NF_RAW` : Docker dans l'invité ne démarre qu'avec les iptables historiques et `DOCKER_INSECURE_NO_IPTABLES_RAW=1`. Manquent aussi `CONFIG_NETFILTER_XT_MARK`, `CONFIG_NETFILTER_XT_MATCH_COMMENT`, `CONFIG_IP_SET`, `CONFIG_IP_VS`, `CONFIG_VXLAN` (non essayés). | observé pour les deux premiers |
| 12 | La console de l'invité n'est journalisée qu'au niveau debug et rien ne permet de la demander pour un Workshop : les échecs de Docker n'ont pu être diagnostiqués qu'avec un patch local. | observé |
| 13 | Le cloisonnement s'arrête à l'API : tout est dans l'espace de noms `default`, sans `NetworkPolicy`, et `GET /v1/workshops` rend les Workshops de tous les groupes, spec comprise, à qui porte le rôle `admin`. Le port de redirection d'un Workshop a répondu à une requête HTTP venue d'un pod sans rapport ; l'ouverture d'un tunnel par ce chemin n'a pas été essayée. | observé |
| 14 | L'api-server écoute sur `0.0.0.0:8080`, codé en dur (`crates/api-server/src/main.rs`) : il ne démarre pas sur une machine où ce port est pris. | observé |
| 15 | Après une reprise, l'horloge de l'invité retarde d'environ 46 s (déjà relevé par la spec 18, §5). | observé |
| 16 | **Une mise en veille perd tout ce qui a été écrit sur le disque racine.** À la reprise, la mémoire vient de l'instantané mais le disque est recopié depuis le cache d'images (`restore_persisted`, `crates/firecracker/src/vm.rs`). Un fichier de 200 Mio écrit et synchronisé avant la suspension se relit correctement tant qu'il est dans le cache de pages de l'invité, puis comme des zéros en lecture directe ; une fois le cache évincé, `ls` rend `Bad message` et le noyau journalise `EXT4-fs error … Directory block failed checksum`. Le système de fichiers reste monté en écriture. Indépendant de la taille du disque. | observé (troisième passage, 2026-10-09) |

Mesures utiles : image prête → `Running` et reprise, 15,5 à 16,6 s (12 mesures) ; un aller-retour d'exec, 0,34 s en médiane sur 25, dont 0,03 s pour l'appel et le reste pour le sondage ; dix exec de `sleep 1` en parallèle, 1,3 s ; mémoire du superviseur et de la VM, 268 à 379 Mo pour un invité de 768 Mio, 969 Mo pour 2 Gio avec Docker.

---

## 2. Objectifs / non-objectifs

**Dans le périmètre** : ce qui sépare aujourd'hui « un bac à sable pour un agent » d'« une session pour une personne dont un serveur vérifie le travail ».

1. Une session a la taille de disque qu'elle demande.
2. Une image qui ne peut pas démarrer échoue au build, avec un message.
3. Les services injectés ne tournent jamais en root et ne confisquent pas de port courant.
4. Un appel « exécuter une commande » rend un résultat fidèle et borné, sans que l'appelant ait à envelopper chaque commande.
5. Le contenu du dépôt source n'entre dans l'invité que si on le demande.
6. Le réseau du build et celui de l'exécution se règlent séparément.

**Hors périmètre** : la réutilisation des images (spec 18) ; le démarrage depuis un snapshot modèle (spec 18, §5) ; le découpage d'un socle commun à plusieurs produits, que cette spec prépare sans le décider.

---

## 3. Décisions proposées

### 3.1. Disque de session dimensionné

Le controller lit `resources.disk` et le superviseur donne à l'invité un système de fichiers inscriptible de cette taille, au lieu de « image + 512 Mio ». La valeur est plafonnée par une limite de cluster. Si la couche inscriptible par VM de la spec 18 (§5) est retenue, sa taille est ce quota ; sinon, l'image ext4 est agrandie à la création du Workshop.

### 3.2. Un invité qui démarre, ou un build qui échoue

À la fin du build, `image-builder` vérifie ce dont le démarrage dépend et échoue avec un message qui nomme le manque :

- systemd présent **et** `/sbin/init` absent : erreur (aujourd'hui, non-démarrage silencieux) ;
- `bash` ou `curl` absents : erreur, ou injection d'un binaire statique.

L'init propre à Atelier (images sans systemd) lance `ttyd` et `code-server` sous le compte de la session, jamais en root.

### 3.3. Ports et services injectés

- Les ports de `sshd`, `ttyd` et `code-server` quittent les valeurs courantes (8080 en premier lieu) pour une plage réservée et documentée.
- L'injection du terminal web et de l'IDE devient optionnelle par Workshop : un appelant qui n'utilise que l'exec n'a pas à les porter.

### 3.4. Un appel « exécuter une commande »

Un appel de l'API (REST, à côté de l'outil MCP) dont les paramètres sont : la commande, le compte, le dossier de travail, l'environnement, une durée maximale **par appel**, une taille maximale de sortie. Il ferme l'entrée standard, attend la fin de la commande et rend en une réponse : le code de sortie **ou** le signal, la sortie standard et la sortie d'erreur séparées et intactes (octets nuls compris), et un indicateur de troncature. Il ne reste pas ouvert quand un processus d'arrière-plan garde un descripteur hérité.

Les défauts du constat 7 sont à corriger aussi pour l'outil MCP existant, indépendamment de ce nouvel appel.

### 3.5. Le dépôt source dans l'invité

`ensure_workspace_clone` devient optionnel (`spec.source.cloneWorkspace`, vrai par défaut pour ne rien changer aux usages actuels). À faux, rien du dépôt n'entre dans l'image hors ce que le build y a copié, et l'exec comme VS Code s'ouvrent sur le dossier de travail déclaré.

Les champs du `devcontainer.json` qui décrivent la session sont appliqués : `remoteUser` (au lieu du compte fixe `vscode`), `workspaceFolder` (créé et donné à ce compte), `containerEnv`, `postStartCommand`, et `hostRequirements` comme valeurs par défaut de `resources`.

### 3.6. Egress de build et egress d'exécution

Deux listes : celle du build, portée par l'image (elle rejoint la question ouverte de la spec 18, §4), et celle de l'exécution, portée par le Workshop. Une modification de la seconde s'applique à un Workshop en marche sans suspension. Constaté au passage : une liste étroite suffit au build (hôtes de Docker Hub, `deb.debian.org`, `download.docker.com`, PyPI), `*` n'est pas nécessaire.

### 3.7. Noyau invité, console, écoute de l'API

- Reconstruire le noyau invité avec `CONFIG_NF_TABLES` et `CONFIG_IP_NF_RAW` (et évaluer les autres options du constat 11).
- Exposer la console de l'invité par Workshop, sur demande.
- Rendre l'adresse d'écoute de l'api-server configurable.

### 3.8. Le disque survit à la mise en veille

Un instantané ne vaut que avec le disque qu'il décrit (constat 16). `snapshot_and_publish` (`crates/vm-supervisor/src/main.rs`) enregistre le disque racine de la microVM avec `snapshot.state` et `snapshot.mem`, et `restore_persisted` repart de cette copie, jamais du cache d'images. Sans cela, toute reprise d'un Workshop qui a écrit sur son disque le corrompt en silence : c'était le défaut le plus grave relevé par ces essais, et il rendait aussi dangereux le contournement du constat 9 (suspendre et reprendre pour appliquer une liste d'egress).

Réalisé par la tâche 14.11, décrit dans [`architecture/snapshot-restore.md`](../architecture/snapshot-restore.md) :

- le disque est copié vers `snapshot.rootfs` **pendant que la microVM est figée**, en conservant les trous du fichier ;
- la reprise remet ce disque dans le jail sans le vérifier ni l'agrandir ;
- un instantané sans son disque (pris avant cette tâche, ou dont la publication a été interrompue) n'est plus repris : le Workshop redémarre à froid, ce que le superviseur journalise.

Vérifié dans l'invité le 2026-10-09 (quatrième passage), sur un Workshop de 4 Gio et sur un Workshop Docker de 6 Gio, deux suspensions de suite : fichiers de 200 Mio relus à l'identique en lecture directe une fois le cache de pages évincé, répertoires et petits fichiers intacts, même `boot_id` et processus d'arrière-plan toujours vivants, conteneurs Docker toujours en marche, aucune erreur ext4. `snapshot.rootfs` occupe ce que l'invité a écrit (1,2 Gio pour 4 Gio apparents, 2,1 Gio pour 6). Les fichiers sont dans le cache 6 à 21 s après la demande ; la reprise prend 16 à 18 s.

Reste ouvert (tâche 14.13) : avec l'offload S3, `snapshot_and_publish` ne répond au controller qu'après le téléversement, soit 40 à 50 s après la demande, alors que `request_snapshot` n'attend que 30 s. Le controller journalise alors « suspension sans snapshot » et `status.snapshotDigest` reste vide, bien que l'instantané soit complet et que la reprise fonctionne ; le pod peut aussi être supprimé avant la fin du téléversement. Le défaut existait déjà pour une mémoire de plusieurs Gio, le disque (envoyé à sa taille apparente, zéros compris) le rend systématique. L'ordre de publication, local comme S3, garantit qu'une interruption laisse un instantané incomplet et ignoré, jamais un mélange.

---

## 4. Risques et questions ouvertes

- **Cloisonnement entre organisations** (constat 13). Un espace de noms et une `NetworkPolicy` par groupe propriétaire, et une liste filtrée par groupe y compris pour le rôle `admin`, changent le modèle de déploiement : à instruire à part, avec le découpage d'un socle.
- **Canal d'exec.** L'exec passe par SSH avec une clé lue dans OpenBao, et les sessions MCP expirent en quelques minutes : un appelant qui vérifie des dizaines d'étapes doit se réauthentifier. Un canal vsock vers un agent de l'invité réglerait aussi l'entrée standard et les descripteurs hérités ; à comparer au coût de maintenir deux chemins.
- **Compatibilité.** Changer les ports injectés (§3.3) et le compte de session (§3.5) touche les Workshops existants et le tableau de bord : prévoir des valeurs par défaut inchangées et une bascule par Workshop.
- **Durée de vie.** Rien n'arrête un Workshop inactif. Un appelant peut piloter la suspension et la suppression ; l'horloge de l'invité après reprise (constat 15), elle, ne peut être corrigée que par Atelier.
- **Ce qui n'a pas été essayé** : une session interactive réelle de terminal ou de VS Code, un grand nombre de Workshops simultanés, un dépôt privé, un build avec une liste d'egress réellement vide, une installation par le chart Helm.
