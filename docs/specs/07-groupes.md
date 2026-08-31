# Groupes : un Workshop appartient à un groupe

> Décision de Philippe (2026-08-31) : **un Workshop est à un groupe**, pas à
> un individu. Ce document fixe le modèle et la trajectoire ; il est écrit
> avant le code parce que le changement touche le CRD, l'autorisation de
> l'api-server, le RLS PostgreSQL et le pm-engine — 49 points en Rust et
> 16 en SQL. Le découvrir en chemin coûterait bien plus cher que de le poser.

## 1. Le modèle

Un Workshop porte **deux** identités, qui répondent à deux questions
différentes et ne doivent pas être confondues :

| champ | question | usage |
|---|---|---|
| `ownerGroup` | **qui y a accès ?** | autorisation, RLS, visibilité |
| `ownerSubject` | **qui l'a créé ?** | audit, traçabilité |

`ownerSubject` ne disparaît pas : savoir qu'un environnement a été provisionné
par telle personne reste utile, y compris longtemps après son départ du
groupe. Mais il cesse d'être la frontière d'accès.

**Conséquence directe et voulue** : un membre du groupe peut reprendre le
Workshop d'un collègue absent — c'est précisément ce que « appartient à un
groupe » veut dire, et c'est ce qui manquait.

## 2. Source de vérité

Les groupes viennent de **Keycloak** (`groups` de l'utilisateur), propagés
dans le jeton par un *group membership mapper*. Atelier ne tient aucun
annuaire propre.

État de départ, vérifié le 2026-08-31 : le realm de dev n'a **aucun groupe**
(`"groups": []`) et **aucun mapper** — le claim `groups` n'est donc jamais
émis, alors que `Claims.groups` existe déjà côté api-server et attend une
valeur qui n'arrive pas. Il faut créer les deux.

Le nom du groupe est utilisé tel quel (`/nom` normalisé en `nom`) : pas
d'identifiant technique, pour qu'un `kubectl get workshop -o yaml` reste
lisible.

## 3. Choix d'un groupe à la création

Un utilisateur peut appartenir à plusieurs groupes. À la création :

- **un seul groupe** → il est retenu implicitement ;
- **plusieurs** → le groupe doit être **explicite** dans la requête, sinon
  `400`. Deviner reviendrait à placer un environnement — et sa dépense — dans
  un groupe au hasard.
- **aucun** → `403`. Sans groupe, il n'existe pas de périmètre auquel
  rattacher le Workshop.

L'api-server vérifie toujours que le groupe demandé est bien l'un de ceux du
jeton : un client ne choisit pas son périmètre, il choisit **parmi** les
siens.

## 4. Le tenant du RLS devient le groupe

Le RLS PostgreSQL (`app.current_tenant`, tables `session_logs`,
`exec_commands`, `project_memories`) est déjà écrit autour d'**une** chaîne de
tenant. Le changement est donc une **substitution de valeur**, pas une
refonte : le tenant passe du sujet au groupe.

C'est ce qui rend le modèle tenable : sans cela, deux barrières
indépendantes cohabiteraient (autorisation par groupe côté API, isolation par
individu côté base) et divergeraient au premier oubli.

⚠️ **Migration des données existantes** : les lignes déjà écrites portent un
sujet en `owner_subject`. Elles deviendront invisibles une fois le tenant
passé au groupe. En développement c'est sans conséquence ; sur une instance
réelle il faut une migration explicite, décidée avec les groupes cibles — ce
n'est pas automatisable sans connaître l'organisation.

## 5. Budget LLM

Le plafond reste **par Workshop** (`maxLlmBudgetUsd`), pas par groupe. Un
budget de groupe supposerait un compteur agrégé et une politique de partage
(qui consomme en premier ? que se passe-t-il à l'épuisement ?) qui n'a pas
été tranchée. LiteLLM sait le faire (budgets d'équipe), mais c'est une
décision de produit distincte.

En revanche la **console d'administration** gagne à afficher la dépense par
groupe. ✅ *Fait le 2026-08-31* : `metadata.owner` des Virtual Keys porte
désormais le groupe et non le créateur — la dépense d'un Workshop est celle
du groupe qui le porte. L'agrégation par équipe dans la console reste à
écrire, mais la donnée est là.

## 6. Trajectoire

Découpage tel qu'il sera implémenté, chaque étape étant vérifiable seule :

1. **Keycloak** : créer les groupes, ajouter le mapper, y placer les
   utilisateurs et le compte de service du PM. Vérifiable : le claim `groups`
   apparaît dans un jeton réel.
2. **CRD** : `WorkshopSpec.ownerGroup`, optionnel au départ pour ne pas
   invalider les Workshops existants. ✅ *Devenu obligatoire le 2026-08-31* —
   `kubectl apply` d'un Workshop sans groupe est désormais refusé par
   Kubernetes lui-même (`spec.ownerGroup: Required value`), et non plus
   seulement par l'api-server.
3. **api-server** : autorisation par groupe avec repli sur `ownerSubject`
   tant que `ownerGroup` est absent ; création qui exige et valide le groupe.
4. **pm-engine** : les Workshops du PM naissent dans le groupe du ticket.
5. **RLS** : bascule du tenant, avec la migration de données. ✅ *Fait le
   2026-08-31* — colonne `owner_subject` renommée en `tenant` sur
   `exec_commands`, `session_logs` et `audit_events`, politiques réécrites.
   Le renommage plutôt que la réutilisation est délibéré : un
   `owner_subject` contenant un nom de groupe aurait piégé le prochain
   lecteur. Vérifié en réel : une exécution lancée par le bot dans un
   Workshop du groupe `atelier-core` s'enregistre avec `tenant =
   atelier-core`, et un autre membre du groupe relit son flux.

Les étapes 1 à 3 sont indépendantes des suivantes et livrables telles quelles.
Le repli de l'étape 3 a permis de ne pas tout casser en un commit ; il a été
**retiré le 2026-08-31**, une fois `ownerGroup` obligatoire. Deux règles
d'autorisation en parallèle finissent par diverger, et c'est celle qu'on
oublie qui décide.
