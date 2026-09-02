# Mise en veille : snapshot/restore Firecracker

> Retour a la [vue d'ensemble](../ARCHITECTURE.md).

Un Workshop n'est pas seulement demarre/detruit : il peut etre **suspendu**.
Firecracker expose nativement `snapshot/create` (fige l'etat de la VM et sa
memoire) et `snapshot/load` (restaure a l'identique), ce qui permet de :

- liberer les ressources du pod parent pendant qu'un Workshop est inactif,
  sans perdre l'etat de travail de l'agent ;
- reprendre en quelques centaines de millisecondes, sans rejouer le boot du
  noyau invite ni le setup du devcontainer.

```mermaid
sequenceDiagram
    participant U as Utilisateur/api-server
    participant C as controller
    participant P as Pod parent
    participant VM as vm-supervisor

    U->>C: spec.desiredState = Suspended
    C->>VM: POST /snapshot (canal de controle HTTP)
    VM->>VM: fige la VM, publie snapshot.state/snapshot.mem sur le cache partage
    VM-->>C: snapshotDigest
    C->>P: supprime le pod (phase Suspending)
    C-->>U: status.phase = Suspended, status.snapshotDigest

    U->>C: spec.desiredState = Running
    C->>P: recree le pod (phase Resuming)
    P->>VM: snapshot present sur le cache ? restore_persisted : boot (depuis image_digest)
    VM-->>C: pod Running
    C-->>U: status.phase = Running
```

Best-effort par conception : si l'appel `POST /snapshot` echoue (pod pas
encore joignable, timeout, ...), la suspension aboutit quand meme, sans
etat fige (`ensure_suspended`/`request_snapshot`,
`crates/controller/src/reconcile.rs`) — mieux vaut honorer
`desired_state: Suspended` sans snapshot que rester bloque dessus
indefiniment.

L'API expose ce cycle via `POST /v1/workshops/:name/suspend` et `/resume`
(`crates/api-server`), typiquement utilises par le dashboard pour une mise
en veille manuelle ou une politique d'auto-suspend sur inactivite (a
definir).

Le role OpenBao du Workshop est deliberement **laisse intact** a travers ce
cycle (pas reprovisionne a chaque resume) : un Workshop suspendu reste "le
meme" Workshop du point de vue identite/secrets (voir
[`identity-secrets.md`](identity-secrets.md)).
