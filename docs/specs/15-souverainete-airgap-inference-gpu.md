# Spécification Technique : Déploiement Souverain, Inférence GPU Locale (vLLM) & CAs Privées (Air-Gap)

> **Statut** : Document de Cadrage Technique (Jalon M11)  
> **Date** : 2026-09-05  
> **Auteur** : Équipe Atelier  
> **Principes directeurs** : Conforme à [`00-architecture-principles-substitutability.md`](00-architecture-principles-substitutability.md), étend [`03-litellm-proxy.md`](03-litellm-proxy.md) et s'articule avec [`14-devex-cli-simulateurs-hitl.md`](14-devex-cli-simulateurs-hitl.md).

---

## 1. Contexte & Objectifs

Dans les environnements d'entreprise régulés (banque, défense, santé, OIV) ou les installations bare-metal souveraines :
1. **L'inférence LLM doit pouvoir être 100% locale** : Aucune donnée ne doit transiter vers un fournisseur d'API cloud (OpenAI, Anthropic). Sur une machine ou un cluster équipé de GPUs (NVIDIA/AMD), Atelier doit fournir un backend d'inférence local prêt à l'emploi.
2. **Le réseau peut être déconnecté (Air-Gap)** : Les images, manifestes et dépendances doivent pouvoir être ingérés depuis des registres d'entreprise privés (Harbor, Nexus).
3. **Les certificats d'autorité d'entreprise (PKI interne) doivent être reconnus** : Les outils de développement in-VM (`git`, `npm`, `pip`, `cargo`, `curl`) et `net-proxy` doivent valider les certificats internes signés par la CA d'entreprise sans désactiver la sécurité TLS.

---

## 2. Architecture Globale

```mermaid
flowchart TD
    subgraph Node["Nœud Single-Node / Cluster avec Accélérateur GPU"]
        subgraph LocalGPU["Inférence GPU Locale (Optionnelle)"]
            GPU_HARDWARE[("GPU NVIDIA / Passthrough")]
            VLLM_SERVICE["Pod vLLM / Backend OpenAI-compatible
(Activé via gpu.enabled=true)"]
            GPU_HARDWARE --- VLLM_SERVICE
        end

        subgraph AtelierControlPlane["Control Plane Atelier"]
            LITELLM["LiteLLM Proxy
(Routage par défaut vers vLLM local)"]
            API["api-server (Axum)"]
            CTRL["controller (K8s Operator)"]
            NETPROXY["net-proxy
(Truststore CA d'entreprise)"]
        end

        subgraph GuestVM["MicroVM Firecracker (Workshop)"]
            AGENT["Agent IA (Claude Code / OpenCode)"]
            CA_STORE["/etc/ssl/certs/ (CA injectée)"]
        end

        subgraph CorpNetwork["Réseau d'Entreprise Privé"]
            CORP_CA[("PKI d'Entreprise (CA Racine)")]
            CORP_HARBOR[("Harbor Interne (Images)")]
            CORP_MIRROR[("Nexus / Artifactory (Miroirs TLS)")]
        end
    end

    VLLM_SERVICE -->|"API OpenAI /v1"| LITELLM
    LITELLM -->|"Endpoint LLM souverain"| AGENT
    CORP_CA -.->|"Injectée dans Helm"| NETPROXY
    CORP_CA -.->|"Injectée dans rootfs"| CA_STORE
    NETPROXY -->|"Valide avec CA privée"| CORP_MIRROR
    CTRL -->|"Pull images signées"| CORP_HARBOR
    AGENT -->|"Trafic réseau contrôlé"| NETPROXY
```

---

## 3. Spécification Détaillée

### 3.1. Backend vLLM Local dans LiteLLM

Pour les déploiements mono-machine (`atelier server install --enable-gpu`) ou les clusters Kubernetes disposant de ressources GPU :

1. **Composant Helm Dédié (`infra.vllm`)** :
   - Déclaré dans `charts/atelier/values.yaml` sous la section `gpu` :
     ```yaml
     gpu:
       enabled: false # Activable via flag CLI ou values
       resources:
         nvidia.com/gpu: 1
       model: "Qwen/Qwen2.5-Coder-7B-Instruct"
       maxModelLen: 16384
       tensorParallelSize: 1
       storage:
         pvcSize: "50Gi"
     ```
2. **Câblage Automatique dans LiteLLM** :
   - Lorsque `gpu.enabled: true`, le template de configuration de LiteLLM (`charts/atelier/templates/litellm-configmap.yaml`) injecte automatiquement le endpoint vLLM local comme modèle principal :
     ```yaml
     model_list:
       - model_name: "default"
         litellm_params:
           model: "openai/qwen2.5-coder"
           api_base: "http://atelier-vllm.{{ .Release.Namespace }}.svc:8000/v1"
           api_key: "local-bypass"
     ```
   - L'agent de code in-VM (`Claude Code`, `OpenCode`, etc.) consomme le proxy LiteLLM sur `http://169.254.0.1:4000` sans changer sa configuration standard.
   - Les budgets et virtual keys continuent de s'appliquer normalement.

3. **Intégration CLI Single-Node (`atelier server install`)** :
   - La commande `atelier server install --enable-gpu` inspecte la présence de `/dev/nvidia*` ou de GPU disponibles et positionne automatiquement les valeurs correspondantes dans le chart local.

### 3.2. Prise en Charge des CAs Privées d'Entreprise

Pour résoudre les blocages `SSL: CERTIFICATE_VERIFY_FAILED` fréquents en entreprise :

1. **Configuration Centralisée Helm** (tâche 11.1, **implémentée**) :
   ```yaml
   tls:
     customCaBundle: |
       -----BEGIN CERTIFICATE-----
       MIIE... (Certificat racine d'entreprise)
       -----END CERTIFICATE-----
   ```
   Monté par le chart en ConfigMap `<release>-ca-bundle` (`charts/atelier/templates/infra/ca-bundle-configmap.yaml`).
2. **Correction par rapport à la première rédaction de cette spec** : `net-proxy` ne termine **jamais** TLS sur le chemin de relais egress — vérifié dans le code (`crates/net-proxy/src/proxy.rs`, `crates/net-proxy/src/tls_sni.rs`) : que ce soit via `CONNECT` (tunnel `TcpStream` brut, octets non interprétés) ou via la détection SNI transparente (lecture du seul champ `server_name` en clair du `ClientHello`, jamais un déchiffrement), le trafic HTTPS relayé reste chiffré de bout en bout entre la microVM et sa vraie destination. Une CA d'entreprise dans `net-proxy` ne pourrait donc rien "inspecter" sans une réécriture MITM complète — hors périmètre, et contraire à la conception documentée du module. Le point réel où la CA d'entreprise doit être approuvée est donc **où TLS est réellement terminé** : côté client, à chaque appelant Rust (`reqwest`, backend `rustls`, qui ne consulte ni le magasin système ni `SSL_CERT_FILE`) et côté outillage in-VM (`git`/`npm`/`pip`/`cargo`/`curl`, §3.3 ci-dessous).
   - Mécanisme générique implémenté dans `crates/common/src/tls_client.rs::client_builder_trusting_extra_ca` (généralise l'`ATELIER_JWT_CA_PATH` pré-existant d'`api-server`) : construit un `reqwest::ClientBuilder` qui fait confiance à la CA d'entreprise en plus des CA publiques. **Piège trouvé en vérifiant** : `reqwest::Certificate::from_pem` (backend `rustls-tls-native-roots`) accepte silencieusement un PEM structurellement invalide (octets arbitraires, ou en-têtes PEM avec un corps base64 corrompu) sans jamais retourner d'erreur — validé explicitement au préalable avec `rustls_pemfile::certs`.
   - Câblé côté `api-server` : quand `tls.customCaBundle` est renseignée et qu'aucun `ATELIER_JWT_CA_PATH` explicite n'est déjà fourni via `apiServer.env`, le chart positionne automatiquement `ATELIER_JWT_CA_PATH` sur le fichier monté — couvre le cas réel `keycloak.external.enabled` avec un IdP d'entreprise derrière une CA privée. Vérifié par `helm template` (ConfigMap rendue, montage, variable positionnée, et override explicite respecté).
   - Non fait dans cette tâche, laissé à une suite : aucun autre `reqwest::Client` du workspace n'a aujourd'hui de véritable besoin de cette CA (les autres appels HTTPS sortants observés sont soit en clair intra-cluster, soit inexistants) — le mécanisme est en place et testé unitairement, mais `api-server`/JWKS reste son seul consommateur réel pour l'instant.
3. **Propagation dans les MicroVMs** (tâche 11.2, **implémentée**) :
   - Le controller retransmet le nom de la `ConfigMap` (`ctx.ca_bundle_configmap`, `ATELIER_CA_BUNDLE_CONFIGMAP`) au Job `image-builder` qu'il crée, monté à `/etc/atelier/ca/ca.crt` (`ensure_image_build_job`, `crates/controller/src/reconcile.rs`) et transmis par `ATELIER_CA_BUNDLE_PATH`.
   - **Correction par rapport à la première rédaction** : `update-ca-certificates` n'est jamais invoqué — ce binaire n'existe pas forcément dans l'image de base cible, et l'exécuter en `chroot` depuis le conteneur `image-builder` (architecture/libc potentiellement différentes) est fragile. À la place, `crates/image-builder/src/main.rs::append_pem_to_bundle_file` ajoute directement la CA au bundle système déjà présent (`/etc/ssl/certs/ca-certificates.crt`, paquet `ca-certificates`) — un **ajout, jamais un remplacement** : remplacer casserait tout accès HTTPS public (PyPI, npm, GitHub...) pour les mécanismes qui font confiance à ce seul fichier. Ce mécanisme sert à DEUX endroits distincts :
     - `trust_enterprise_ca_bundle_for_this_process` : le PROPRE magasin de confiance du conteneur `image-builder` — nécessaire pour son `git clone` (`ensure_workspace_clone`, tourne sur ce pod, PAS relayé/déchiffré par `net-proxy`, voir la correction de la tâche 11.1).
     - `inject_enterprise_ca_bundle` : le rootfs PRODUIT, pour `git`/`curl`/`pip`/`cargo` (OpenSSL) IN-VM au runtime du Workshop, via `GIT_SSL_CAINFO`/`CURL_CA_BUNDLE`/`REQUESTS_CA_BUNDLE`/`PIP_CERT`/`SSL_CERT_FILE` pointés sur le bundle combiné.
   - **Cas particulier Node.js/`npm`** : Node ne consulte JAMAIS le magasin système, même après cet ajout — `NODE_EXTRA_CA_CERTS` (mécanisme additif propre à Node, documenté officiellement) pointe sur une copie brute dédiée (`/usr/local/share/ca-certificates/atelier-ca.crt`), déposée dans tous les cas.
   - **Limite assumée et documentée** : seules les images de base Debian/Ubuntu (bundle système à `/etc/ssl/certs/ca-certificates.crt`) bénéficient de la couverture complète — une image RHEL/Fedora (`/etc/pki/tls/certs/ca-bundle.crt`) ne reçoit que `NODE_EXTRA_CA_CERTS`, jamais testée faute d'image de ce type dans ce workspace. `envbuilder` (microVM builder jetable, `crates/builder-vm-init`) — utilisé pour RÉSOUDRE le devcontainer.json et exécuter `postCreateCommand`/features avant que ce rootfs ne soit exporté — n'est PAS couvert par cette tâche : un devcontainer dont les features ont besoin d'un miroir npm/pip d'entreprise AU MOMENT DU BUILD échouerait encore. Laissé à une suite (nécessiterait d'injecter la CA dans le rootfs de LA microVM builder elle-même, `crates/builder-vm-init/Dockerfile`, un chantier distinct).

### 3.3. Outillage Déconnecté (Air-Gap Packaging)

1. **Préfixage Universel des Registres** :
   - Le chart supporte `.Values.global.imageRegistry` pour rediriger l'ensemble des images (`api-server`, `controller`, `vm-supervisor`, `net-proxy`, `identity-proxy`, `mcp-gateway`, `envbuilder`) vers le registre d'entreprise privé (ex: `harbor.internal.corp/atelier`).
2. **Script d'Export Autonome (`scripts/airgap-bundle.sh`)** :
   - Lit la liste complète des images requises pour une version donnée d'Atelier.
   - Utilise `skopeo copy` ou `docker save` pour générer une archive portable `atelier-images-<version>.tar.gz`.
   - Fournit la commande d'import inverse :
     ```bash
     ./scripts/airgap-bundle.sh import --registry harbor.internal.corp/atelier
     ```

---

## 4. Garde-Fous & Pièges Évités

1. **Substituabilité Respectée** :
   - Atelier n'impose pas vLLM : si l'entreprise utilise déjà Ollama, TGI, TensorRT-LLM ou un cluster GPU externe, il suffit de renseigner son URL dans LiteLLM. vLLM n'est qu'un composant optionnel de commodité.
2. **Pas d'Ordonnancement Multi-Cluster Artificiel** :
   - Chaque instance d'Atelier reste scopée à son cluster. Pour une séparation physique (cluster public vs cluster confidentiel), on déploie deux instances d'Atelier indépendantes.
3. **Zéro Baisse de Sécurité TLS** :
   - Ne jamais proposer de flag `insecure-skip-tls-verify` généralisé. L'injection de la CA racine est la seule approche propre et conforme aux politiques de sécurité d'entreprise.

---

## 5. Phasage Opérationnel

| Tâche | Description | Composants |
| :--- | :--- | :--- |
| **11.1** | Support de `customCaBundle` dans Helm et injection dans `net-proxy` | `charts/atelier`, `crates/net-proxy` |
| **11.2** | Injection automatique de la CA d'entreprise dans le rootfs des microVMs | `crates/image-builder`, `crates/guest-init` |
| **11.3** | Composant Helm optionnel `gpu.vllm` et templating de route par défaut LiteLLM | `charts/atelier`, `templates/litellm` |
| **11.4** | Détection GPU et flag `--enable-gpu` dans `atelier server install` | `crates/cli` |
| **11.5** | Script d'export/import d'images conteneurs déconnecté (`airgap-bundle.sh`) | `scripts/` |
