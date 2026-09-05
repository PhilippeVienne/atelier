# Spécification Technique : Stack d'Observabilité (Traces, Métriques, Logs)

> **Statut** : Proposé (rédigé avant implémentation, conformément à la démarche déjà suivie pour les specs 08/09/10/11)
> **Principe cadre** : Conforme à [`00-architecture-principles-substitutability.md`](00-architecture-principles-substitutability.md). N'introduit aucune nouvelle dépendance métier : toute la valeur vient de ce qui est déjà instrumenté (voir §1) et n'attend qu'un backend pour être exploitable.
> **Date** : 2026-09-05
> **Auteur** : Équipe Atelier

---

## 1. Constat, vérifié empiriquement

`crates/common/src/telemetry.rs` initialise déjà, dans chaque binaire Rust du projet (`api-server`, `controller`, `net-proxy`, `identity-proxy`, `mcp-gateway`, `vm-supervisor`, `image-builder`), un exporteur OTLP conditionné à la présence de `OTEL_EXPORTER_OTLP_ENDPOINT` — absent, on retombe sur un simple `tracing_subscriber::fmt` vers stdout.

Deux faits vérifiés en conditions réelles avant d'écrire une ligne de code de cette spec, précisément parce que la documentation existante (le commentaire de tête de `telemetry.rs`) laissait croire à une instrumentation déjà fonctionnelle :

1. **`OTEL_EXPORTER_OTLP_ENDPOINT` n'est positionnée nulle part** — ni dans `charts/atelier/`, ni dans `deploy/dev/local-stack.sh`. Chaque binaire tourne donc aujourd'hui, partout, en mode `fmt` seul : aucun export OTLP n'a jamais lieu, même sur l'instance de dev.
2. **Ça dépend du service — nuance trouvée après re-vérification.** En pointant `OTEL_EXPORTER_OTLP_ENDPOINT` vers une instance réelle de `grafana/otel-lgtm` (voir §3) :
   - `atelier-api-server` : plusieurs requêtes HTTP réelles (`/health/readiness`) produisent **zéro trace** dans Tempo. Cause : aucune route n'est instrumentée par un `TraceLayer`/`#[tracing::instrument]` créant un span autour d'une requête — `tracing-opentelemetry` n'exporte que des **spans**, jamais un `tracing::info!`/`warn!` isolé sans span actif englobant.
   - `atelier-controller` : **fonctionne déjà**. Une vraie réconciliation contre le cluster de dev produit des traces réelles et hiérarchisées, `service.name=atelier-controller`, retrouvées dans Tempo — la boucle de réconciliation est instrumentée depuis le commit `a51b1c9` (voir §4.2). Une première lecture rapide du code (recherche de `#[tracing::instrument]` sans arguments) avait manqué cette instrumentation, qui utilise partout la forme `#[tracing::instrument(skip_all, ...)]` — corrigé ici après re-vérification empirique plutôt que laissé tel quel.

   Le tuyau fonctionne réellement dès qu'un span existe pour s'y déverser ; `api-server` est le seul composant où il manque encore.

Conséquence : ce n'est pas seulement « pas de collecteur/backend/dashboard déployé » (constat déjà connu, `docs/PROGRESS.md` le classait en *Backlog*) — c'est que **même en déployant un backend aujourd'hui, l'onglet traces resterait vide**. Cette spec couvre donc les deux moitiés du problème : l'infrastructure de collecte ET l'instrumentation applicative qui doit réellement produire des spans.

Aucune instrumentation n'existe côté `services/pm-engine` (Python) — `opentelemetry` n'apparaît que dans `uv.lock` comme dépendance transitive, jamais importée. Hors périmètre de cette spec (voir §2).

---

## 2. Objectifs / non-objectifs

**Dans le périmètre :**
- Un backend unique auto-hébergé (traces + métriques + logs + UI), déployé par le chart et par `deploy/dev/local-stack.sh`.
- `OTEL_EXPORTER_OTLP_ENDPOINT` positionnée sur tous les Deployments/process Rust existants.
- Au moins un span par requête HTTP entrante sur `api-server` et un span par itération de la boucle de réconciliation du `controller` — le strict nécessaire pour qu'une trace existe et relie une requête à ce qu'elle a déclenché, pas une instrumentation exhaustive fonction par fonction.
- Des métriques minimales (compteur de requêtes, histogramme de latence) sur `api-server`, seul composant qui sert un trafic HTTP externe significatif.

**Hors périmètre (reporté, à traiter dans une spec ultérieure si besoin s'en fait sentir) :**
- Instrumentation de `services/pm-engine` (écosystème Python distinct, `opentelemetry-instrumentation-fastapi` existe mais n'a jamais été évalué).
- Agrégation des logs de conteneurs (`kubectl logs`/stdout) vers Loki : exigerait un agent par nœud (DaemonSet, type Alloy/Promtail) en plus du collecteur — une brique de plus que ce que le constat du §1 justifie pour un premier lot. `kubectl logs` reste la voie d'accès aux logs.
- Alerting/pagination (Alertmanager ou équivalent).
- Tableaux de bord Grafana pré-provisionnés : Grafana expose déjà les datasources (Prometheus/Tempo/Loki) auto-configurées par l'image retenue (§3) ; la création de dashboards custom est laissée à l'exploration manuelle pour ce premier lot.
- Rétention/sauvegarde des données de télémétrie au-delà du volume éphémère du pod (voir risque §5).

---

## 3. Choix d'architecture : un seul conteneur, pas quatre services

Trois briques sont nécessaires : un récepteur OTLP, un stockage (traces/métriques/logs), une UI de consultation (Grafana). La littérature standard assemble ça avec quatre services distincts (Collector, Tempo, Prometheus/Mimir, Loki) plus Grafana — cinq pods, cinq Services, cinq soucis de ressources sur un cluster single-node low-cost (spec 10).

**Retenu : `grafana/otel-lgtm`**, image tout-en-un officielle Grafana Labs packageant un collecteur OTLP + Tempo (traces) + Prometheus (métriques) + Loki (logs) + Grafana, datasources pré-provisionnées. Vérifié empiriquement (`docker run`, cette session) :
- Démarre `healthy` en ~40 s.
- ~480 Mo de RAM au repos, stable après démarrage (mesuré via `docker stats`).
- Expose `4317`/`4318` (OTLP gRPC/HTTP), `3000` (Grafana), `3200` (Tempo direct), `9090` (Prometheus direct).
- Les quatre datasources (Loki, Prometheus, Tempo, Pyroscope) sont bien auto-enregistrées dans Grafana dès le démarrage (`GET /api/datasources`), sans configuration manuelle.

C'est la même logique que `deploy/dev/llm-proxy` (une seule instance partagée plutôt qu'un service par composant) et que le choix du chart de rester monolithique (spec 02) : un seul pod, cohérent avec le public visé (single-node low-cost, spec 10), au prix d'un point de défaillance unique et d'une absence de haute disponibilité — accepté explicitement, comme pour LiteLLM et OpenBao en dev.

### 3.1. Nouveau composant chart : `observability`

Nouveau template `charts/atelier/templates/infra/observability-deployment.yaml` (Deployment + Service, même structure que `litellm-deployment.yaml`), piloté par `.Values.observability.enabled` (défaut `true`, cohérent avec `litellm.enabled`). Pas de Secret : rien de sensible n'y transite (les traces peuvent contenir des noms de Workshop/alias LLM, jamais un jeton — la même discipline que `crate::llm_budget` s'applique déjà à ce qui est journalisé).

`.Values.observability.resources` : limite mémoire à fixer en marge du ~480 Mo mesuré, même précédent que `litellm.resources` (§ commentaire de `charts/atelier/values.yaml` sur l'OOMKilled constaté empiriquement pour LiteLLM — prévoir une marge similaire plutôt que la déduire en théorie).

### 3.2. Câblage de `OTEL_EXPORTER_OTLP_ENDPOINT`

Sur chaque Deployment existant du chart (`apiserver-deployment.yaml`, `controller-deployment.yaml`, et tout composant qui tourne en pod, `mcp-gateway`/`identity-proxy`/`net-proxy`/`vm-supervisor` restant instrumentés par la même fonction `telemetry::init` mais dans des pods Workshop, hors du chart control-plane) :

```yaml
{{- if .Values.observability.enabled }}
- name: OTEL_EXPORTER_OTLP_ENDPOINT
  value: "http://{{ include "atelier.componentName" (dict "root" $ "component" "observability") }}:4317"
{{- end }}
```

Même câblage pour `deploy/dev/local-stack.sh` (nouvelle section, même convention que le bloc LLM Proxy conditionnel) : déploie le pod `grafana/otel-lgtm`, exporte `OTEL_EXPORTER_OTLP_ENDPOINT` dans `env.sh` pointant sur un port-forward local (le controller/api-server tournent hors cluster en dev, même raison que `ATELIER_LLM_PROXY_ADDR`/`14000`).

---

## 4. Instrumentation applicative minimale

### 4.1. `api-server` : un span par requête HTTP

`tower_http::trace::TraceLayer` (dépendance déjà transitive d'`axum`, à ajouter explicitement à `Cargo.toml`), posé dans `routes::router` en tête de la chaîne de middlewares — un span par requête, avec méthode/chemin/statut en attributs. C'est le strict minimum pour qu'une requête corresponde à UNE trace exploitable dans Tempo ; le détail interne (par ex. l'appel LiteLLM déclenché par `admin_llm_create_model`) reste un span enfant à ajouter au cas par cas via `#[tracing::instrument]`, pas systématiquement partout d'un coup.

### 4.2. `controller` : déjà instrumenté — vérifié empiriquement, RAS

Correction par rapport à une première lecture trop rapide de `reconcile.rs` (une recherche de `#[tracing::instrument]` sans arguments ne remontait rien, alors que le code utilise partout la forme `#[tracing::instrument(skip_all, ...)]`) : la boucle de réconciliation **est déjà instrumentée**, depuis le commit `a51b1c9` (« Imposer OpenTelemetry comme convention d'observabilité », 2026-08-18) — cinq fonctions de `crates/controller/src/reconcile.rs` portent `#[tracing::instrument(skip_all, ...)]`, dont deux avec `fields(workshop = %workshop.name_any())`.

Reverifié bout en bout dans le cadre de cette session (pas seulement relu dans le code) : `atelier-controller` lancé en local avec `OTEL_EXPORTER_OTLP_ENDPOINT` pointée vers une instance réelle de `grafana/otel-lgtm`, une vraie réconciliation d'un Workshop existant sur le cluster de dev déclenchée — **cinq traces `service.name=atelier-controller`, span racine `reconciling object`, correctement hiérarchisées**, retrouvées dans Tempo (`GET /api/ds/query`). Aucune tâche d'implémentation nécessaire ici : seul le câblage de `OTEL_EXPORTER_OTLP_ENDPOINT` (§3.2) manquait pour que ce qui existe déjà produise des traces exploitables.

### 4.3. Métriques : compteur + histogramme sur `api-server`

`telemetry.rs` n'initialise aujourd'hui qu'un `TracerProvider` (traces), jamais de `MeterProvider` (métriques) — à ajouter, même garde `OTEL_EXPORTER_OTLP_ENDPOINT` absent/présent que pour les traces. Une seule paire de métriques pour ce premier lot : nombre de requêtes et latence, par route et code de statut — ce que `tower_http::metrics` ou une couche `axum` équivalente expose déjà, pas une métrique métier par endpoint à concevoir une par une.

---

## 5. Risques identifiés, non résolus par cette spec

- **Volume éphémère** : sans `persistentVolumeClaim`, un redémarrage du pod `observability` perd tout l'historique (traces/métriques/logs). Acceptable pour un premier lot orienté diagnostic à chaud, pas pour un usage d'audit à long terme — à revisiter si le besoin apparaît.
- **Un seul pod, aucune haute disponibilité** : cohérent avec le reste du chart (LiteLLM, OpenBao) mais veut dire qu'un pic de charge sur `observability` peut ralentir l'export de traces des autres composants (l'exporteur OTLP de `telemetry.rs` utilise un `BatchSpanProcessor`, qui absorbe les latences courtes mais pas une indisponibilité prolongée).
- **~480 Mo de RAM** est significatif à l'échelle d'une install single-node low-cost (spec 10) qui tourne déjà LiteLLM (1.5 Gi de limite) + Postgres + Keycloak + Forgejo + OpenBao sur une seule machine — à chiffrer contre le total avant de fixer `observability.resources.limits` dans les valeurs par défaut du chart.
- **Instrumentation minimale volontaire (§4)** : un span par requête HTTP et par réconciliation ne donne pas une visibilité fine sur CE qui, à l'intérieur, a été lent — juste QUE cette requête/réconciliation l'a été. Suffisant pour prioriser où creuser ensuite, pas pour un diagnostic de performance complet dès ce premier lot.
