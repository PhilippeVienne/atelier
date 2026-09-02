# Identite et secrets : OIDC + OpenBao

> Retour a la [vue d'ensemble](../ARCHITECTURE.md). Etat d'avancement et
> preuves de test : voir [`PROGRESS.md`](../PROGRESS.md).

**L'utilisateur humain** proprietaire d'un Workshop
(`WorkshopSpec.owner_subject`) est authentifie via un JWT **OIDC
generique** : `api-server` valide la signature (JWKS, cache rafraichi en
tache de fond) et l'audience du token aupres de l'issuer configure
(`ATELIER_OIDC_ISSUER_URL`), sans dependre d'un fournisseur d'identite
particulier — Keycloak sert de reference en developpement, mais tout
IdP conforme OIDC/OAuth2 (RFC 7517 JWKS, RFC 7636 PKCE) convient. L'agent
IA execute dans la microVM, lui, n'a **aucune** identite propre aupres de
ce fournisseur : il herite du contexte du Workshop qui l'heberge.

Les secrets destines aux environnements (credentials/tokens injectes par
`identity-proxy`) sont stockes dans [OpenBao](https://openbao.org/) —
deliberement separe des Secrets Kubernetes du cluster sous-jacent, qui
restent geres par les mecanismes k8s standards pour le control plane
lui-meme. Un secret stocke la est souvent lui-meme l'identite de sortie de
l'environnement (ex: une cle d'API presentee a un service externe) :
**seul** `identity-proxy` peut la recuperer et l'utiliser — l'agent dans la
microVM n'y a jamais acces directement, meme indirectement via les
variables d'environnement ou le systeme de fichiers de la VM.

## Pont d'identite vers OpenBao : auth Kubernetes, pas OIDC

`identity-proxy` s'authentifie aupres d'OpenBao via la **methode d'auth
Kubernetes** d'OpenBao, pas via une federation JWT/OIDC avec le
fournisseur d'identite humain. Le pod parent de chaque Workshop recoit son
propre ServiceAccount Kubernetes (`<name>-parent`) ; `identity-proxy`
presente le token projete de ce ServiceAccount, qu'OpenBao verifie en
direct aupres de l'API Kubernetes (TokenReview) — aucun secret a
distribuer ou stocker pour amorcer cette confiance, et aucune entite
dediee a provisionner cote fournisseur d'identite pour chaque
environnement (une precedente version provisionnait une entite machine
par Workshop cote fournisseur d'identite ; ce mecanisme a ete retire, la
seule identite du Workshop pertinente pour OpenBao est desormais son
ServiceAccount Kubernetes).

Le `controller` provisionne, par Workshop, une policy OpenBao et un role
`auth/kubernetes/role/workshop-<name>` scopant l'acces au chemin KV
`secret/{data,metadata}/workshops/<name>/*` au seul ServiceAccount de ce
Workshop (`crates/controller/src/openbao.rs::ensure_workshop_role`), ce
qui borne le rayon d'action d'un Workshop compromis aux seuls secrets qui
lui ont ete explicitement destines. Un role cluster-wide distinct
(`ensure_api_server_role`) couvre `api-server`, qui a besoin de lire tous
les Workshops.

> **Pourquoi pas une federation OIDC → OpenBao ?** Ce serait plus
> coherent conceptuellement ("un seul IdP pour tout"), mais demanderait de
> configurer un Resource Server OAuth2 cote IdP et un backend JWT/OIDC
> cote OpenBao (JWKS, client credentials grant) — une integration
> nettement plus lourde et une surface de panne plus grande que l'auth
> Kubernetes, deja standard et deja necessaire pour tout le reste
> (ServiceAccounts, RBAC).

## `identity-proxy` : injection de credentials

`identity-proxy` (`crates/identity-proxy`) est un proxy HTTP explicite, sur
le meme modele que `net-proxy`, mais **jamais joint directement par la
VM** : `net-proxy` reste le seul point d'entree reseau que la VM peut
atteindre (voir [`network-security.md`](network-security.md)). Quand
`ATELIER_IDENTITY_PROXY_ADDR` est configure cote `net-proxy`, celui-ci lui
chaine, apres avoir deja tranche l'allowlist, *tout* le trafic egress
autorise — pas seulement les requetes qui ont effectivement besoin d'une
identite injectee. `identity-proxy` decide alors, requete par requete, s'il
doit injecter un credential (regle correspondante) ou simplement relayer
tel quel vers la destination finale ; il ne fait jamais lui-meme
l'arbitrage allowlist (deja tranche en amont par `net-proxy`) et ne se
reconnecte jamais vers `net-proxy` en aval (ce serait une boucle) — il se
connecte toujours directement a la destination.

- **Regles d'injection** (`ATELIER_IDENTITY_INJECTION_RULES`, JSON) :
  chaque regle associe un hote (correspondance exacte ou wildcard
  `*.domaine`) a un en-tete a poser (ex: `Authorization`), un prefixe
  (ex: `Bearer `) et un champ d'un secret KV v2 OpenBao
  (`secret/workshops/<name>/<secret_path>`). Alimentees par le
  `controller` a partir de `Workshop.spec.identity_injection_rules`, en
  y ajoutant a la volee la regle Git calculee (`crate::git_identity`,
  quand `ATELIER_GIT_HOST_SERVICE` est configure) et la regle LiteLLM
  (Virtual Key) — voir `ensure_parent_pod` dans
  `crates/controller/src/reconcile.rs`.
- **Cache de secrets** rafraichi en tache de fond (login OpenBao +
  relecture des champs references par les regles) toutes les 5 minutes,
  avant expiration du token client OpenBao (TTL 15 min cote serveur). Les
  valeurs ne sont jamais journalisees.
- **Limite structurelle** : un `CONNECT` (HTTPS) est un tunnel TCP opaque,
  chiffre bout-a-bout entre l'agent et la destination — `identity-proxy` ne
  peut donc **pas** y injecter d'en-tete sans devenir un MITM TLS actif, ce
  qui n'est pas fait. L'injection ne fonctionne aujourd'hui que pour les
  requetes HTTP en clair relayees en forme absolue. Cote `net-proxy`, le
  chainage respecte cette distinction : un `CONNECT` original est rejoue
  en `CONNECT` vers `identity-proxy` (son propre handler de tunnel), une
  requete HTTP en clair lui est envoyee telle quelle en forme absolue,
  **jamais enveloppee dans un `CONNECT`** — sinon `identity-proxy` la
  traiterait en tunnel opaque et ne la reinterpreterait jamais pour y
  injecter un en-tete.

`ATELIER_IDENTITY_PROXY_ADDR` sert donc une double fin cote `net-proxy` :
adressage explicite par le nom d'alias `identity-proxy`
(`crates/net-proxy/src/internal.rs`, hors allowlist) **et** saut
obligatoire pour tout l'egress autorise des lors qu'il est configure — voir
[`network-security.md`](network-security.md) pour le detail des deux
mecanismes.
