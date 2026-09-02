# Spécification Technique : Installation Single-Node Low-Cost (`curl | bash`)

> **Statut** : Proposé (implémentation en cours dans le même tour) — rédigé avant le script, conformément à la démarche déjà suivie pour les specs 08/09.
> **Principe Cadre** : Ne remplace pas [`02-helm-deployment-admin-doc.md`](02-helm-deployment-admin-doc.md)/[`docs/admin-guide.md`](../admin-guide.md), qui restent la référence pour un déploiement multi-nœud/production — cette spec ajoute un **chemin d'installation alternatif**, pas une seconde architecture.
> **Date** : 2026-09-02
> **Auteur** : Équipe Atelier

---

## 1. Constat & Vision

Le chart Helm `charts/atelier` (Jalon M6) est complet mais suppose un opérateur qui sait déjà : monter un cluster Kubernetes, choisir/installer un Ingress Controller, poser `cert-manager`, générer des secrets forts pour remplacer les `"change-me-*"` de `values.yaml`, et — le plus délicat — initialiser/desceller OpenBao à la main (`bao operator init`/`unseal`, une cérémonie manuelle documentée en détail dans `docs/admin-guide.md` mais qui n'a aucun sens pour un premier essai).

Cette spec ajoute un script unique, exécutable en `curl -fsSL <url> | bash`, qui automatise l'intégralité de ce chemin pour le cas d'usage **single-node, coût minimal** : un seul serveur (bare-metal ou VM avec accès KVM réel — voir §2), pas de haute disponibilité, TLS automatique via Let's Encrypt sur un domaine fourni par l'opérateur. C'est un point d'entrée pour découvrir Atelier ou l'exploiter à petite échelle, **pas** un remplacement du chemin multi-nœud existant.

---

## 2. Garde-fou non négociable : `/dev/kvm`

Chaque Workshop est une microVM Firecracker : sans accès matériel à KVM, aucun Workshop ne peut jamais démarrer (il reste `Pending` indéfiniment — voir `docs/admin-guide.md`, section 1.1). La plupart des VPS "low cost" grand public (DigitalOcean Droplets, la majorité des offres Hetzner Cloud/AWS EC2 standard, etc.) tournent elles-mêmes dans un hyperviseur qui n'expose PAS la virtualisation imbriquée à l'invité — installer Atelier dessus produirait un système qui semble fonctionner (Dashboard accessible, API répond) mais dont la fonctionnalité centrale ne marche jamais, sans qu'aucune erreur claire ne le dise avant qu'un Workshop reste bloqué.

Décision actée : le script **vérifie `/dev/kvm` en tout premier**, avant toute autre action, et s'arrête avec un message explicite s'il est absent — jamais un mode dégradé silencieux. Conviennent : un serveur bare-metal avec virtualisation activée au BIOS, ou une instance cloud explicitement "metal"/nested-virt (voir `docs/admin-guide.md`, section 1.1, pour la liste par fournisseur — reprise telle quelle ici, aucune raison de la dupliquer).

---

## 3. Choix de conception

### 3.1. Runtime Kubernetes : k3s, installé par le script

k3s (Rancher) est le runtime single-node le plus léger qui reste un Kubernetes conforme (CRD `apiextensions.k8s.io/v1`, `batch/v1` Job — les deux prérequis déjà documentés). Son propre script d'installation (`https://get.k3s.io`) est lui-même un `curl | sh` bien établi, embarqué tel quel plutôt que réimplémenté.

**k3s embarque Traefik par défaut — désactivé à l'installation** (`--disable=traefik`) : le chart `atelier` documente et teste ses annotations par défaut pour **ingress-nginx** (`docs/admin-guide.md`, section 3), jamais Traefik. Réutiliser le choix déjà validé évite d'introduire un second jeu d'annotations non testé.

### 3.2. Ingress + TLS : ingress-nginx + cert-manager, domaine fourni par l'opérateur

Le script installe `ingress-nginx` et `cert-manager` via leurs charts Helm officiels (dépôts publics, pas de fork), puis crée un `ClusterIssuer` Let's Encrypt nommé **`letsencrypt-prod`** — exactement le nom déjà attendu par défaut dans `values.yaml` (`tls.certManager.issuer`), pour n'avoir à surcharger que le strict nécessaire dans les valeurs générées.

Domaine fourni par l'opérateur (pas de heuristique `sslip.io`) : le script demande un domaine de base (ex. `atelier.exemple.com`) et dérive les 4 sous-domaines exactement comme `values.yaml` les attend (`auth.<base>`, `git.<base>`, `app.<base>`, `api.<base>`). **Prérequis explicite, vérifié avant d'aller plus loin** : les 4 enregistrements DNS (ou un wildcard `*.<base>`) doivent déjà pointer vers l'IP publique du serveur — un défi HTTP-01 échoue sinon, et le script le dit clairement plutôt que de laisser `cert-manager` échouer silencieusement en tâche de fond.

### 3.3. OpenBao : `devMode: true` par défaut, avec un avertissement explicite et irrévocable

`openbao.devMode: false` (le défaut du chart, pensé pour la production) exige une cérémonie manuelle (`bao operator init`/`unseal` + Secret pré-créé) que ce script, pensé pour un premier démarrage sans intervention, ne peut pas automatiser sans se substituer à une décision opérationnelle qui doit rester consciente (où sont gardées les clés de descellement ?). `devMode: true` supprime cette cérémonie — au prix, RÉEL et documenté dans le code du chart lui-même (`openbao-statefulset.yaml`, la persistance est explicitement absente en `devMode`), d'un OpenBao **sans persistance** : un redémarrage du pod perd tous les secrets (identifiants git/session des Workshops déjà provisionnés, clés virtuelles LiteLLM).

Retenu comme défaut du mode "low-cost" malgré cette limite, à condition que le script l'affiche en toutes lettres avant de continuer (et l'inscrive dans le résumé final) : c'est le compromis attendu d'un premier essai à un seul nœud, pas un défaut caché. Un flag (`--openbao-production`) bascule sur `devMode: false` pour l'opérateur qui veut la persistance dès le départ, au prix de devoir lui-même dérouler `bao operator init`/`unseal` (renvoyé vers `docs/admin-guide.md`, jamais réexpliqué ici).

### 3.4. Secrets : générés aléatoirement, jamais laissés à `"change-me-*"`

Le script génère (`openssl rand -hex 24`, déjà disponible sur toute distribution ciblée) : le mot de passe admin PostgreSQL, le mot de passe admin Keycloak, la clé maître LiteLLM. Écrits dans un fichier de valeurs Helm généré à la volée (jamais committé, jamais loggé en clair sur la sortie standard) et, séparément, dans `/root/atelier-credentials.txt` (`chmod 600`) pour que l'opérateur puisse se connecter au Dashboard/Keycloak après coup — un `helm install` qui réussirait avec les placeholders du dépôt public serait une installation dont n'importe qui connaissant le code source connaît déjà les mots de passe.

### 3.5. Stockage : `local-path` (provisioner embarqué de k3s)

k3s embarque son propre `StorageClass` par défaut (`local-path`, `local-path-provisioner`) — suffisant pour un seul nœud (pas de réplication de toute façon possible avec un seul nœud), évite d'installer et de faire fonctionner un CSI plus lourd (Longhorn, Rook/Ceph) hors de propos pour ce cas d'usage. `global.storageClassName` n'a pas besoin d'être renseigné explicitement : `local-path` est déjà le `StorageClass` par défaut du cluster, et un `storageClassName: ""` (déjà la valeur par défaut du chart) délègue au `StorageClass` par défaut du cluster — comportement Kubernetes standard, aucune surcharge nécessaire.

### 3.6. Dimensionnement : les valeurs par défaut du chart, inchangées

Les `resources.requests/limits` déjà présents dans `values.yaml` (issus de constats empiriques réels sur `kind-atelier-dev`, voir `docs/admin-guide.md` section 6.1) restent la base : ce ne sont pas des valeurs "cluster HA", déjà pensées pour un usage économe. Le mode low-cost ne les réduit donc PAS davantage (le risque de `OOMKilled` documenté empiriquement l'emporte sur l'économie marginale) — sa contribution porte sur l'orchestration (k3s, ingress, secrets), pas sur un second jeu de tailles non vérifié.

### 3.7. Distribution du chart : clone shallow du dépôt, pas de chart publié

Aucun chart `atelier` n'est aujourd'hui publié dans un registre OCI (voir `docs/DEPLOYMENT.md`, qui ne documente que la publication d'images conteneur sur GHCR, jamais celle du chart). Le script clone le dépôt (`git clone --depth 1`) dans `/opt/atelier/src` et installe directement `charts/atelier` depuis ce chemin local — cohérent avec le fait que le dépôt est déjà public, sans exiger de nouvelle infrastructure de publication pour cette spec.

---

## 4. Ce que le script fait, dans l'ordre

1. Vérifie l'exécution en root (ou `sudo`), l'architecture (x86_64/arm64), et surtout `/dev/kvm` (§2) — s'arrête sinon.
2. Demande (arguments `--domain`/`--email`, ou invite interactive si absents) le domaine de base et l'e-mail Let's Encrypt ; vérifie par une résolution DNS que `auth.<domaine>` pointe déjà vers l'IP publique détectée du serveur — avertit sans bloquer si la résolution échoue (un DNS qui vient d'être posé peut ne pas encore avoir propagé), mais l'affiche clairement.
3. Installe k3s (`curl -sfL https://get.k3s.io | sh -s - --disable=traefik`) s'il n'est pas déjà présent ; exporte `KUBECONFIG`.
4. Installe `helm` (binaire officiel) s'il n'est pas déjà présent.
5. Installe `ingress-nginx` et `cert-manager` via leurs charts Helm officiels ; crée le `ClusterIssuer` `letsencrypt-prod`.
6. Clone (ou met à jour) le dépôt Atelier dans `/opt/atelier/src`.
7. Génère les secrets aléatoires (§3.4), rend un fichier de valeurs Helm `/opt/atelier/values-generated.yaml` (domaines dérivés, secrets, `devMode` selon le flag).
8. `helm upgrade --install atelier ./charts/atelier -n atelier-system --create-namespace -f values-generated.yaml` — `upgrade --install` plutôt que `install` : le script est **idempotent**, une deuxième exécution met à jour une installation existante au lieu d'échouer sur "already exists".
9. Attend que les pods se stabilisent (même repère que `docs/admin-guide.md`, section 6 : un `CrashLoopBackOff` transitoire de quelques minutes est normal le temps que les Jobs d'initialisation s'exécutent).
10. Affiche un résumé final : les 4 URL, l'emplacement des identifiants générés, et l'avertissement OpenBao (§3.3) si `devMode: true`.

---

## 5. Hors périmètre (assumé)

- **Pas de haute disponibilité** — un seul nœud, par construction. Un opérateur qui a besoin de HA suit le chemin multi-nœud déjà documenté, pas celui-ci.
- **Pas de sauvegarde automatisée** — `docs/admin-guide.md` documente déjà les procédures de backup/restore PostgreSQL ; ce script ne les déclenche pas automatiquement.
- **Pas de support Windows/macOS pour le serveur cible** — Linux avec systemd uniquement (prérequis de k3s lui-même).
- **Pas de dry-run/désinstallation automatisée dans cette première version** — `helm uninstall`/`k3s-uninstall.sh` (fourni par k3s lui-même) restent les commandes manuelles de retour arrière, non enveloppées ici.

## 6. Vérification

- **`shellcheck` (0.10.0), niveau `style` inclus** : aucun avertissement sur `scripts/install.sh`, exécuté réellement dans cette session (binaire statique téléchargé sans droits root, pas supposé).
- **Les 11 images GHCR référencées par le chart** (`atelier-controller`, `atelier-api-server`, `atelier-dashboard`, `atelier-pm-engine`, `atelier-kvm-device-plugin`, `atelier-net-proxy`, `atelier-identity-proxy`, `atelier-mcp-gateway`, `atelier-image-builder`, `atelier-builder-vm-init`, `atelier-vm-supervisor`) vérifiées **réellement publiques et taguées `:latest`** via l'API GHCR (jeton anonyme, `GET /v2/philippevienne/<image>/tags/list` → `200` pour chacune) — un `401` brut sans jeton ne suffit pas à conclure (GHCR l'impose même pour les images publiques), d'où cette double vérification.
- **Versions de charts `ingress-nginx`/`cert-manager` épinglées** après consultation réelle des index Helm (`charts.jetstack.io`/`kubernetes.github.io/ingress-nginx`), pas des versions supposées ou une ancienne valeur recopiée d'ailleurs.
- **Bug réel trouvé et corrigé avant toute publication** : les invites interactives (`read -p`) auraient lu depuis un `stdin` déjà occupé par le flux du script sous `curl | bash` (stdin = le script lui-même, pas le clavier) — corrigé en lisant depuis `/dev/tty` explicitement (même pattern que `rustup`), avec repli sur une erreur claire si aucun terminal de contrôle n'est disponible.
- **Aucune exécution de bout en bout sur un serveur frais n'a été faite** (l'environnement de développement de cette session héberge déjà un cluster `kind` actif et ne doit pas être perturbé par l'installation d'un second runtime Kubernetes) : à exécuter contre une vraie VM/un vrai serveur bare-metal avant de le considérer pleinement validé — voir `docs/PROGRESS.md` pour le suivi de cette limite assumée.
