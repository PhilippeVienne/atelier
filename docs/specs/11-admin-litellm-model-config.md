# Spécification Technique : Configuration des Modèles LiteLLM par un Admin

> **Statut** : Proposé (rédigé avant implémentation, conformément à la démarche déjà suivie pour les specs 08/09/10)
> **Principe cadre** : Étend [`03-litellm-proxy.md`](03-litellm-proxy.md) (cycle de vie des Virtual Keys) sans le remplacer — cette spec couvre la face manquante : comment un **modèle/provider** arrive dans `model_list`, pas comment une clé de Workshop en est dérivée.
> **Date** : 2026-09-03
> **Auteur** : Équipe Atelier

---

## 1. Constat

Le déploiement Helm de production (`charts/atelier/templates/infra/litellm-deployment.yaml`) lance LiteLLM **sans aucun modèle configuré** : ni `--config`, ni `ConfigMap`, ni volume monté — seul un Secret (`LITELLM_MASTER_KEY`, `DATABASE_URL`) est injecté. Le seul endroit du dépôt où un `model_list` existe réellement est `deploy/dev/llm-proxy/config.yaml`, monté par un `Deployment` de **développement distinct** (`deploy/dev/llm-proxy/dev-deployment.yaml`), jamais référencé par `charts/atelier/`.

Conséquence concrète : sur une instance Atelier installée via le chart (ou via `scripts/install.sh`, spec 10), LiteLLM démarre joignable mais **incapable de servir la moindre requête** — `POST /v1/chat/completions` échoue systématiquement, faute de `model_list`. Rien dans le chart, la documentation admin ou le Dashboard ne dit à l'opérateur comment corriger ça après l'installation.

Une ébauche de console d'administration existe déjà : `dashboard/app/admin/llm/page.tsx`, accessible aux utilisateurs du rôle Keycloak `admin` (garde cosmétique côté page, autorisation réelle appliquée par `atelier-api-server` — `ADMIN_ROLE`, voir `crates/api-server/src/routes.rs:104-105,417-421,463-467`). Elle est **lecture seule** : elle affiche les modèles déjà servis (`GET /model/info` via `LlmBudgetClient::overview()`, `crates/api-server/src/llm_budget.rs:251-...`) et la dépense, mais n'offre aucun moyen d'en ajouter, modifier ou retirer un.

Cette spec couvre l'ajout de cette capacité manquante : **un admin doit pouvoir déclarer un provider/modèle LiteLLM (et ses identifiants) depuis le Dashboard, sans `kubectl`, sans redéployer, sans toucher à un fichier YAML.**

---

## 2. Objectifs / non-objectifs

**Dans le périmètre :**
- Lister, ajouter, modifier, supprimer une entrée de `model_list` (alias exposé, modèle réel, `api_base`, identifiants du provider) depuis le Dashboard.
- Réserver cette capacité au rôle `admin`, avec la même exécution du contrôle côté serveur (`atelier-api-server`) que le reste de la console — jamais une confiance dans un masquage côté navigateur.
- Ne jamais réafficher un identifiant provider une fois enregistré (write-only, comme un mot de passe).

**Hors périmètre (déjà couvert ailleurs ou explicitement reporté) :**
- Cycle de vie des Virtual Keys par Workshop : déjà spécifié et implémenté (spec 03, `crates/controller/src/litellm.rs`), non touché ici.
- Budgets/plafonds par Workshop : déjà couverts par `WorkshopSpec.resources.maxLlmBudgetUsd` et la page `/admin/llm` existante (panneau "Dépense").
- Un gestionnaire de secrets générique pour Atelier : voir §4.2 pour la décision de ne PAS introduire OpenBao ici.

---

## 3. Où vit la logique : extension, pas nouveau service

Cohérent avec l'existant plutôt qu'une nouvelle brique : les nouveaux endpoints rejoignent `crates/api-server/src/llm_budget.rs` (`LlmBudgetClient`) et `crates/api-server/src/routes.rs`, exactement là où vivent déjà `overview()`/`spend_report()`. Le Dashboard étend `dashboard/app/admin/llm/page.tsx` (formulaire d'ajout + actions par ligne du tableau "Modèles" déjà présent) et `dashboard/lib/api-server.ts` (nouvelles fonctions à côté de `getLlmOverview`/`getSpendReport`, même convention `ApiServerError`/`call()`).

**Décision explicite : pas de BFF direct Dashboard → LiteLLM.** Le Dashboard n'a aujourd'hui aucun accès à `LITELLM_MASTER_KEY` (confirmé : `charts/atelier/templates/core/dashboard-deployment.yaml` ne référence aucune variable `LITELLM_*`). Lui donner cet accès dupliquerait une autorisation déjà appliquée par `atelier-api-server` et créerait un deuxième chemin de confiance vers un master key à portée totale sur LiteLLM (génération/suppression de N'IMPORTE QUELLE Virtual Key, pas seulement la gestion de modèles). `atelier-api-server` reste le seul service qui détient ce master key (`LlmBudgetClient::new`, déjà le cas pour `overview`/`spend`).

### 3.1. Nouveaux endpoints `atelier-api-server`

| Méthode | Route | Rôle |
|---|---|---|
| `POST` | `/v1/admin/llm/models` | Ajoute un modèle |
| `PATCH` | `/v1/admin/llm/models/{id}` | Modifie un modèle existant |
| `DELETE` | `/v1/admin/llm/models/{id}` | Retire un modèle |

Chacun : `claims.has_role(ADMIN_ROLE)` sinon `403` (identique à `admin_llm_overview`/`admin_llm_spend`, `crates/api-server/src/routes.rs:417-421,463-467`).

`LlmBudgetClient` gagne trois méthodes miroir des endpoints admin LiteLLM correspondants. **Vérifié empiriquement contre l'instance de dev réelle (`atelier-llm-proxy`, cluster `kind-atelier-dev`)** — LiteLLM ne suit effectivement pas une convention REST stricte pour ses mutations : tout est `POST`, y compris la suppression, avec le payload qui porte l'identité :

- `POST /model/new` — body `{"model_name", "litellm_params": {"model", "api_base"?, "api_key"}}`. Réponse `200` avec `model_info.id` (UUID généré par LiteLLM) — c'est cet `id` qui sert de cible à `update`/`delete` (voir §3.2).
- `POST /model/update` — même forme de body que `/model/new`, plus `"model_info": {"id": "<uuid>"}` pour cibler l'entrée. Réponse `200`, mêmes champs.
- `POST /model/delete` — body `{"id": "<uuid>"}` seul. Réponse `200` `{"message": "Model: <uuid> deleted successfully"}`.
- **Précondition bloquante découverte en testant** : ces trois routes renvoient `500 {"error": {"message": "Set 'STORE_MODEL_IN_DB=True' in your env to enable this feature."}}` tant que la variable d'environnement `STORE_MODEL_IN_DB` n'est pas positionnée à `True` sur le déploiement LiteLLM — **absente aujourd'hui à la fois de `charts/atelier/templates/infra/litellm-deployment.yaml` et de `deploy/dev/llm-proxy/dev-deployment.yaml`**. À ajouter dans les deux (nouvelle tâche, voir 6.7.1bis) avant que `6.7.2` ait le moindre effet observable.
- **Persistance vérifiée** : un modèle créé via `/model/new` (avec `STORE_MODEL_IN_DB=True`) survit à la suppression et recréation du pod (`kubectl delete pod` + rollout) — confirme l'hypothèse du §5 (avantage de l'option DB retenue en §4.2).
- Note secondaire observée, non encore expliquée : la réponse de `/model/new`/`/model/update` renvoie `litellm_params.model`/`api_key`/`api_base` sous forme de chaînes qui ressemblent à du chiffré, alors que `LITELLM_SALT_KEY` n'est positionnée nulle part sur cette instance de dev — à re-vérifier avant de conclure quoi que ce soit sur le risque "stockage en clair sans salt key" du §4.2 (peut-être un chiffrement par défaut avec `LITELLM_MASTER_KEY` en l'absence de salt dédiée).

### 3.2. `LlmModel` : ajout d'un identifiant stable

`crates/api-server/src/llm_budget.rs:167-172` expose aujourd'hui `LlmModel { name, target, api_base }` — sans identifiant stable, `update`/`delete` n'ont rien à cibler (`model_name` n'est pas garanti unique côté LiteLLM : le wildcard `"*"` et un alias explicite peuvent coexister sans distinction possible autrement). LiteLLM assigne un `id` interne à chaque entrée de `model_list` créée dynamiquement (`ModelInfoEntry`, déjà partiellement lu par `overview()` mais l'`id` n'est pas encore extrait) — le faire remonter dans `LlmModel.id` est un préalable à `update`/`delete`, pas une option.

---

## 4. Identifiants fournisseur (clé API DeepSeek/Anthropic/...)

### 4.1. Formulaire Dashboard

Champs : alias exposé (`model_name`), modèle réel + provider (`litellm_params.model`, ex. `anthropic/claude-3-5-sonnet-20241022`), `api_base` (optionnel), `api_key` (saisie une fois, jamais réaffichée — le formulaire d'édition montre un placeholder `••••••••` et un bouton "remplacer", jamais la valeur existante en clair, cohérente avec la convention déjà énoncée par la page actuelle : *"Les jetons eux-mêmes ne sont jamais exposés ici"*, `dashboard/app/admin/llm/page.tsx:244-246`).

### 4.2. Stockage : dans LiteLLM lui-même, pas OpenBao — décision explicite

Deux options existaient :
1. **LiteLLM stocke lui-même l'identifiant**, chiffré au repos dans sa propre base (`atelier_litellm`, déjà provisionnée) via `POST /model/new` — c'est le mécanisme natif de LiteLLM pour toute entrée de `model_list` créée dynamiquement (par opposition à une entrée statique du `config.yaml`, qui référence `os.environ/VARNAME`).
2. Écrire l'identifiant dans OpenBao (convention déjà en place pour les credentials Git/session des Workshops, `crates/controller/src/openbao.rs`) et le référencer depuis LiteLLM.

**Retenu : l'option 1.** `atelier-api-server` n'a aujourd'hui aucun module OpenBao (seul `crates/controller` en a un) — l'option 2 imposerait de dupliquer ce client dans une crate qui ne le connaît pas, pour un gain de sécurité marginal : LiteLLM chiffre déjà ces valeurs au repos avec sa propre clé (`LITELLM_SALT_KEY`, variable d'environnement LiteLLM dédiée à cet usage, absente de `charts/atelier/values.yaml` aujourd'hui — **à ajouter** dans `litellm-deployment.yaml`/`values.yaml`, générée aléatoirement par `scripts/install.sh` comme `LITELLM_MASTER_KEY` l'est déjà, spec 10 §3.4). Sans cette clé, LiteLLM stocke ces identifiants **en clair** dans `atelier_litellm` — un garde-fou de démarrage doit vérifier sa présence avant d'activer ces trois endpoints (même pattern que les variables `required` déjà vérifiées par `pm_engine.main._lifespan`).

### 4.3. Journalisation

Toute création/modification/suppression de modèle passe par le pipeline d'audit déjà en place côté `atelier-api-server` (si un tel pipeline existe pour d'autres actions admin — **à vérifier avant implémentation**, sinon en ajouter un minimal : qui, quand, quel alias, jamais la valeur de la clé).

---

## 5. Risques identifiés, non résolus par cette spec

- **Redémarrage du pod LiteLLM** : les modèles ajoutés dynamiquement (DB) survivent à un redémarrage (contrairement à un `model_list` de `config.yaml`, statique) — c'est un AVANTAGE de l'option 1 retenue en §4.2, à vérifier empiriquement plutôt qu'assumé.
- **Double source de vérité en dev** : le cluster de dev garde son `config.yaml` statique (`deploy/dev/llm-proxy/`) — les modèles qui y sont déclarés n'apparaîtront pas comme "gérés" par cette nouvelle UI (pas d'`id` LiteLLM dynamique). Documenter cette distinction dans la page elle-même plutôt que de la laisser surprendre un admin de dev.
- **Un seul rôle `admin`, pas de granularité** : quiconque a ce rôle peut créer un modèle pointant vers n'importe quel `api_base` arbitraire (exfiltration potentielle de tout trafic LLM). Accepté ici au même niveau de confiance que le reste de la console (`admin_llm_overview` expose déjà toutes les clés et toute la dépense à ce rôle) — pas un nouveau risque introduit par cette spec, mais à garder en tête si le rôle `admin` s'élargit un jour.
