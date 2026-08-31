# Pieges connus (a ne pas re-decouvrir)

> Enseignements durables tires de sessions de developpement reelles : chaque
> entree correspond a un bug ou un comportement contre-intuitif qui a
> reellement coute du temps, avec ce qu'il faut savoir pour ne pas y
> retomber.
>
> **A lire avant d'attaquer un composant que l'on ne connait pas.** Les
> recits complets de diagnostic sont dans
> [`../archive/PROGRESS-2026-08.md`](../archive/PROGRESS-2026-08.md).
>
> Convention : on n'ajoute ici que ce qui reste vrai apres la session — un
> piege structurel, pas le journal d'un incident.

---

- **Un `Option<String>` de `status` sans `skip_serializing_if` est EFFACE par
  un JSON merge patch.** `image_digest`/`snapshot_digest` sont ecrits par
  `image-builder`/`vm-supervisor`, mais le controller patche `status` en
  entier : un `None` de son cote partait en `"imageDigest": null`, ce que
  l'API Kubernetes interprete comme une suppression. Le Workshop restait
  alors bloque en `BuildingImage` alors que son image existait dans le cache
  (environ une fois sur trois builds simultanes). Regle generale : tout champ
  de `status` ecrit par un AUTRE composant doit porter
  `skip_serializing_if = "Option::is_none"`, et les chemins de reconciliation
  qui ne le calculent pas doivent le reporter tel quel.
- **Un test qui suffixe son namespace/ServiceAccount en `-test` ne s'isole pas
  forcement.** `ensure_api_server_role` ecrit sur un nom de role OpenBao
  CONSTANT (`API_SERVER_ROLE`), pas derive de ses arguments : un test croyant
  s'isoler reecrivait en fait les bindings du role dont depend l'`api-server`
  reel, qui echouait ensuite sur "service account name not authorized"
  jusqu'au redemarrage du controller — et le symptome apparaissait des heures
  plus tard, sans lien apparent avec le test. Sur une ressource partagee dont
  le nom est fixe, provisionner les valeurs de PRODUCTION (idempotent) plutot
  que des variantes de test.
- **Un alias interne de `net-proxy` (`llm-proxy`, `mcp-gateway`, `registry`,
  `git.atelier.internal`) n'existe dans aucun DNS reel.** Il n'etait joignable
  que par un client honorant `HTTP_PROXY` — le proxy resout alors l'alias sur
  l'en-tete `Host`. Tout client qui resout lui-meme son nom d'hote echouait :
  c'est le cas de Node.js, qui ignore `HTTP_PROXY` par defaut, donc de Claude
  Code. Le resolveur de `net-proxy` repond desormais lui-meme pour ces alias
  (`crates/net-proxy/src/dns.rs`). Devant un composant qui n'atteint pas un
  service interne alors que `curl` y arrive depuis le meme guest, verifier
  `getent hosts <alias>` avant toute autre piste.
- **La Virtual Key LiteLLM par Workshop est provisionnee, plafonnee... et
  jamais utilisee.** Le controller cree bien une cle dediee
  (`atelier-wks-<nom>`), lui pose le budget de
  `spec.resources.maxLlmBudgetUsd`, l'ecrit dans OpenBao et genere la regle
  d'injection `identity-proxy` correspondante. Mais `net-proxy` route l'alias
  `llm-proxy` **directement vers LiteLLM**, alors que l'alias Git
  (`git.atelier.internal`) pointe, lui, vers `identity-proxy`. La requete ne
  traverse donc jamais l'injecteur : le guest continue d'envoyer le jeton
  statique partage de son `/etc/environment`, et toute la depense de l'agent
  est facturee a ce jeton commun. **Mesure a l'appui** (2026-08-31) : apres
  un appel Claude Code reussi dans un Workshop, `atelier-wks-<nom>` affiche
  `spend = 0.000000` pour un `max_budget` de 5 $. Consequence : le plafond
  par Workshop ne contraint rien, et la consommation par Workshop n'est pas
  attribuable. Le correctif suit le schema Git (alias -> identity-proxy +
  `hostAlias` vers le vrai service), avec une subtilite : le guest appelle
  `http://llm-proxy` sur le port 80, quand le Service LiteLLM ecoute sur
  4000.
  **A ne pas conclure trop vite** : l'absence de cles `atelier-wks-*` dans
  `/key/list` ne prouve rien — le nettoyage d'un Workshop les revoque, alors
  que les cles `atelier-build-*` survivent. Verifier sur un Workshop VIVANT.
- **Un flux SSE ne doit pas se fermer sur « le travail est fini », mais sur
  « j'ai emis l'evenement final ».** `stream_handler` (`api-server`)
  s'arretait des que la commande etait terminee, quel que soit l'evenement
  qu'il venait d'emettre. Les branches etant ordonnees stdout -> stderr ->
  status, un sondage trouvant a la fois de la sortie neuve ET une commande
  finie — le cas courant — envoyait la sortie puis fermait, **sans jamais
  emettre `status`**. Cote client, `exitCode` restait `null` : `pm-engine`
  comparait `None != 0` et concluait a l'echec de TOUTE execution, tests
  verts compris, consommant les trois tours d'auto-correction a chaque run.
  **Ce qui l'a rendu visible** : faire figurer le code de sortie dans la
  trace (`exit code None` saute alors aux yeux). Un `None` silencieusement
  traite comme un echec est indistinguable d'un vrai echec — toujours
  afficher la valeur brute.
- **`--permission-mode acceptEdits` n'autorise PAS les commandes `Bash`.**
  Il auto-approuve les editions de fichiers, rien de plus. En mode `--print`
  (non interactif), personne n'est la pour approuver le reste : `git add`,
  `git commit` et `git push` etaient donc refuses en silence. L'agent
  produisait un travail complet et correct qui restait en fichiers **non
  suivis** dans la microVM, et `OpenPullRequest` ouvrait une PR vide. Le
  Workshop delegue s'executant dans une microVM Firecracker jetable sans
  acces reseau hors allowlist, la frontiere de securite est la microVM et non
  l'invite d'un CLI : `DelegateToClaudeCode` utilise donc
  `bypassPermissions`. **Signature du probleme** : `git status` dans le
  Workshop montre `?? <fichiers>` avec un `git log` intact — le travail
  existe, il n'est simplement jamais entre dans l'index.
- **Un jeton fige a l'ouverture d'une session MCP expire en cours de
  session.** Une session Streamable HTTP emet plusieurs requetes HTTP au fil
  de sa vie (POST de l'appel d'outil, flux SSE, DELETE de fermeture), et un
  noeud comme `DelegateToClaudeCode` vit bien plus longtemps qu'un jeton OIDC
  (300 s par defaut chez Keycloak) : l'api-server repondait `ExpiredSignature`
  au milieu de la delegation. `pm_engine.mcp_client` pose desormais l'en-tete
  `Authorization` **par requete** via un `httpx2.Auth` adosse au
  `OidcTokenProvider` (qui cache et renouvelle deja). Allonger la duree de vie
  du jeton ne fait que deplacer la limite : c'est le rafraichissement qui
  supprime l'hypothese sur la duree des appels.
- **Un proxy HTTP ne doit pas reecrire que la PREMIERE requete d'une
  connexion.** Un client configure avec `HTTP_PROXY` (tous les Workshops)
  garde sa connexion ouverte et envoie toutes ses requetes suivantes en forme
  absolue sur la meme socket. `net-proxy` reecrivait la premiere en forme
  origine puis basculait en `copy_bidirectional` : les suivantes arrivaient
  telles quelles a `uvicorn`/LiteLLM, qui repondait `404` a partir du 2e
  echange. Symptome : Claude Code repond au premier tour puis echoue **sans
  ecrire aucun fichier** (`api_error_status: 404`, `num_turns: 2` dans
  `--output-format json`) — le PM ouvrait donc des PR vides alors que le
  Workshop etait sain. Corrige par `forward_rewriting`
  (`crates/net-proxy/src/proxy.rs`), qui boucle sur les requetes et suit le
  cadrage des corps (`Content-Length`/`chunked`).
  **Deux reflexes qui auraient fait gagner la journee** : `curl` ne reproduit
  pas le bug (une seule requete par connexion, donc toujours reecrite) — il
  faut un client qui enchaine ; et `claude --output-format json` donne le code
  d'erreur HTTP reel, la ou la sortie texte ne montre qu'un message trompeur.
- **Le message de Claude Code "There's an issue with the selected model … it
  may not exist or you may not have access to it" ne dit pas ce qu'il pretend.**
  Il s'affiche aussi quand le modele est parfaitement valide et que la panne
  reelle est ailleurs (API injoignable, DNS). Il apparait meme lors des
  executions qui reussissent. Ne jamais l'utiliser comme diagnostic : verifier
  d'abord qu'un appel HTTP direct a `$ANTHROPIC_BASE_URL` aboutit depuis le
  guest.
- **Un `curl` qui reussit ne prouve pas qu'un autre client reussira** dans la
  meme microVM : `curl` honore `HTTP_PROXY`, la plupart des runtimes
  applicatifs non. C'est precisement ce qui a masque le piege ci-dessus
  pendant toute une session — l'appel de verification passait, l'application
  echouait.
- **Un `suspend`/`resume` restaure le filesystem du guest depuis le
  snapshot** : toute modification faite a la main dans la microVM (y compris
  un `/etc/hosts` bidouille pour un test) survit au cycle et **contamine les
  mesures suivantes**. Repartir d'un Workshop neuf, ou nettoyer explicitement,
  avant de conclure qu'un correctif fonctionne.
- Toute regle `iptables` de la microVM se termine par un `DROP` : un port
  ouvert cote hote mais absent de la liste passee a
  `enable_transparent_gateway` est jete **silencieusement**. Comme c'est un
  `DROP` et non un `REJECT`, le client expire sur son timeout — le symptome
  ressemble a de la lenteur, jamais a un blocage. Devant un guest lent au
  boot, verifier la chaine AVANT de soupconner une latence.
- Pour observer un guest dont ni `ttyd` ni `sshd` ne repondent, mettre
  `RUST_LOG=atelier_firecracker=debug` sur `vm-supervisor` : la console
  serie du guest est deja drainee dans ses logs (`drain_console_pipes`).
  C'est le seul canal d'observation qui ne depend d'aucun service du guest.
- Les scripts de recuperation de credentials du devcontainer sortent en
  `exit 0` en cas d'echec comme en cas de succes (repli deliberement
  silencieux, pour ne jamais demarrer un service sans authentification) :
  `systemctl status` affiche donc `OK Finished` sur un repli. Ne jamais en
  deduire que le fetch a reussi — verifier le marqueur `stderr`.
- Une valeur qu'un composant utilise pour lui-meme ET propage a un pod doit
  etre dedoublee des lors que ce composant peut tourner hors cluster : en
  dev, le controller pointe sur un port-forward (`127.0.0.1:...`) qui ne
  designe rien depuis un pod. Corrige pour OpenBao (`pod_addr`) ; le meme
  probleme subsiste pour `ATELIER_LLM_PROXY_ADDR`.
- Ne JAMAIS construire une commande shell avec `json.dumps` : les guillemets
  doubles laissent `bash` interpreter backticks, `$(...)` et `$VAR`.
  `shlex.quote` (guillemets simples) est la seule forme sure — d'autant que
  les prompts du PM contiennent du texte issu du corps d'un ticket, entree
  non fiable.
- `atelier_mcp_session` fige son jeton OIDC a l'ouverture : une session MCP
  tenue plus de quelques minutes finit en `ExpiredSignature`. Toute boucle
  d'attente doit rouvrir une session courte a chaque iteration.
- `create_workshop` (MCP) est asynchrone et laisse `egressAllowlist` vide
  par defaut. Un appelant qui cree des Workshops doit donc (a) fournir
  l'allowlist, sans quoi le build d'image ne peut jamais aboutir, et
  (b) attendre la phase `Running` avant tout `exec_in_workshop`.
- Le nom du modele par defaut de Claude Code change a chaque version du CLI
  et LiteLLM ne le connait pas : Claude Code sort alors en erreur **sans
  ecrire aucun fichier**, ce qui se traduit par des PR vides et aucun
  message explicite. Epingler le modele cote appelant (`--model`) et garder
  le wildcard LiteLLM en repli.
- Un LLM encadre frequemment sa reponse JSON dans un bloc markdown malgre
  une consigne "UNIQUEMENT du JSON" : retirer les delimiteurs avant
  `json.loads` plutot que durcir le prompt.
- Une microVM ne peut sortir que sur les ports 80 et 443 (redirection
  transparente) ; tout autre port de destination est jete. Pour joindre un
  service interne au cluster depuis un guest, l'exposer sur le port 80.

- `fctools` 0.6.0/0.7.0-alpha.2 ne compilent pas avec seulement les
  features `vm` + `jailed-vmm-executor` + `tokio-runtime` +
  `nix-syscall-backend` : il faut aussi `direct-process-spawner`.
- `/tmp` est `tmpfs,nodev` sur la plupart des distros : un jail Firecracker
  qui y est enracine a des device nodes inertes, et le message d'erreur
  Firecracker ("configure the ACL") est trompeur. Utiliser un
  `chroot_base_dir` sur un filesystem sans `nodev` (`/var/tmp` en dev).
- Pour un snapshot `fctools`, les ressources `Produced` (ex: fichiers de
  snapshot) doivent recevoir un `initial_path` **relatif au jail**
  (`/snapshot.state`), pas un chemin hote absolu — contrairement aux
  ressources `Moved`, qui se resolvent automatiquement.
- `Vm::shutdown()` de `fctools` renonce a `cleanup()` si la VM n'est pas
  Paused/Running au moment de l'appel (ex: rootfs de test qui s'est deja
  eteint tout seul) ; il faut appeler `cleanup()` inconditionnellement pour
  eviter de laisser trainer un jail orphelin.
- Un patch JSON merge partiel sur `status` echoue en 422 si l'objet n'a
  encore aucun statut : le tout premier write de statut doit toujours etre
  complet.
- `ENVBUILDER_GIT_CLONE_REF` n'existe pas ; la revision s'encode dans
  `ENVBUILDER_GIT_URL` sous la forme `<repo>#<revision>`.
- `ENVBUILDER_DEVCONTAINER_JSON_PATH` est relatif a
  `ENVBUILDER_DEVCONTAINER_DIR` (`.devcontainer` par defaut), pas a la
  racine du depot — source d'un bug de chemin double
  (`.devcontainer/.devcontainer/...`) avant correction.
- Compiler un binaire avec une image `rust:*-bookworm` puis le faire tourner
  dans `debian:bookworm-slim` casse sur un mismatch de version glibc ; il
  faut un build multi-stage avec la meme base pour build et run.
- `ip tuntap add <nom> mode tap` echoue avec un message trompeur
  (`argument "<nom>" is wrong: "mode"/"dev"/"name" not a valid ifname`,
  quel que soit l'ordre des arguments) des que `<nom>` depasse 15
  caracteres (IFNAMSIZ Linux = 16 octets, terminateur nul inclus) — le
  message ne mentionne jamais la longueur, seulement un mot-cle de la
  commande elle-meme, ce qui egare completement le diagnostic.
- Pour tester du code necessitant `CAP_NET_ADMIN` (creation de TAP,
  configuration d'IP) sans root ni sudo interactif :
  `unshare --net --map-root-user -- <commande>` donne un espace de noms
  reseau isole avec toutes les capacites necessaires, sans mot de passe.
  Insuffisant en revanche pour un test qui a aussi besoin d'une vraie route
  de sortie vers Internet (le namespace isole n'a que `lo`) — cf. section
  "Builder microVM".
- `setcap cap_net_admin+eip` sur un binaire ne suffit pas toujours : dans un
  environnement d'agent sandboxe, meme une copie dediee du binaire avec la
  capability posee peut echouer en `Operation not permitted` sur le **vrai**
  netns (confirme via `getcap` correct + `strace` montrant un simple appel
  RTNETLINK refuse) — la sandbox elle-meme filtre l'operation independamment
  des capabilities Linux. `docker run --privileged` (nouveau netns isole
  avec `CAP_NET_ADMIN` effectif + sortie Internet via le NAT Docker par
  defaut) contourne ce blocage, la ou `unshare --net` seul n'a pas de route
  de sortie.
- axum 0.8 a change la syntaxe des parametres de route : `:nom` (ancienne
  syntaxe) panique au demarrage avec un message qui suggere `{nom}` — pas
  une erreur de compilation, une panique runtime au premier `Router::route`
  concerne.
- `jsonwebtoken` 11 exige d'activer explicitement une feature de fournisseur
  crypto (`rust_crypto` ou `aws_lc_rs`) ; sans ca, toute operation JWK
  panique au runtime avec un message qui explique le probleme (pas d'erreur
  de compilation) — `rust_crypto` evite une dependance a un compilateur C.
- `jsonwebtoken::jwk::Jwk::from_encoding_key(&EncodingKey, Algorithm)`
  derive directement les parametres publics (n/e pour RSA, x/y pour EC)
  depuis une cle privee : pratique pour construire un JWKS de test reel
  (vraie paire de cles, vraie signature) sans dupliquer la logique
  d'encodage/decodage a la main.
- `docker run --privileged --network host --device=/dev/kvm --device=/dev/net/tun`
  est une alternative viable a un acces `sudo` interactif reel pour les
  tests necessitant a la fois `CAP_NET_ADMIN` (creation de TAP) ET une
  vraie sortie reseau vers des services de l'hote (`net-proxy`, registre
  OCI de dev) : contrairement a un `docker run --privileged` isole (NAT
  Docker, netns separe), `--network host` partage directement le netns de
  l'hote — le TAP cree dans le conteneur est immediatement visible et
  routable depuis l'hote, sans configuration NAT/forwarding supplementaire.
  Compiler avec un `CARGO_TARGET_DIR` dedie dans ce conteneur (meme piege
  glibc que plus haut).
- `sockaddr_un.sun_path` (Linux) est limite a 108 octets : un chemin de
  jail Firecracker trop long (noms de test verbeux repetes dans le chemin,
  ex: `{chroot_base_dir}/firecracker/{jail_id}/root/run/firecracker.socket`)
  fait echouer `connect()` en `ENAMETOOLONG` a chaque tentative — silencieux
  et non exploitable si le code appelant (ici `fctools` 0.7.0-alpha.2,
  `Vm::start`) avale l'erreur dans une boucle de retry sans jamais la
  remonter (voir aussi le bullet suivant). Choisir des noms de jail/dossier
  de travail courts dans les tests.
- `fctools` 0.7.0-alpha.2 : la boucle d'attente du socket API dans
  `Vm::start` (`src/vm/mod.rs:244`,
  `loop { if client.get(...).await.is_ok() { break } }`) ne cede jamais la
  main a l'executeur async si `client.get()` echoue de facon synchrone a
  chaque iteration (ex: `ENAMETOOLONG` sur le chemin du socket) — elle
  tourne alors indefiniment a 100% CPU sur un seul thread, empechant meme
  le `timeout()` englobant de se declencher (aucune erreur ni panic, juste
  un blocage total). A diagnostiquer via `gdb -p <pid> -batch -ex bt` sur le
  thread actif (pas `strace`, qui ne voit rien d'utile si le blocage est
  purement en espace utilisateur sans syscall bloquant) : la pile remonte
  directement jusqu'a la ligne fautive.
- Une image OCI convertie en `rootfs.ext4` brut (`crane export` + `mke2fs`,
  sans passer par un runtime de conteneur au boot) perd toute sa metadonnee
  `ENV`/`ENTRYPOINT`/etc. — un guest qui boote directement ce filesystem
  (init custom en PID 1, cas de `atelier-builder-vm-init`) doit re-fournir
  explicitement toute variable d'environnement dont un binaire de l'image
  a besoin (ex: `KANIKO_DIR` pour `envbuilder`), meme si le `Dockerfile`
  source la definit via `ENV`.
- `golang.org/x/net/http/httpproxy` (utilise par le client HTTP standard de
  Go, donc par `envbuilder`) exclut **inconditionnellement** `localhost` et
  les IP loopback du proxy configure via `HTTP_PROXY`/`HTTPS_PROXY` — meme
  sans `NO_PROXY`, comportement non desactivable depuis l'environnement.
  Un service passe comme `localhost:<port>` a un process qui doit y acceder
  *via* un proxy explicite (ex: guest microVM sans route par defaut) doit
  plutot recevoir une adresse non-loopback (IP reelle de l'hote/du lien
  point-a-point).
- `reboot(RebootMode::RB_POWER_OFF)` (crate `nix`) n'a aucun effet observable
  dans une microVM Firecracker minimale sans ACPI (`pci=off` dans les
  `boot_args`) : le noyau n'a pas de handler `pm_power_off` a invoquer et se
  contente d'un `halt` ("reboot: System halted"), sans que Firecracker
  detecte la fin de la VM. Avec `reboot=k` dans les `boot_args` (deja
  necessaire pour ce type de machine minimale), c'est
  `reboot(RebootMode::RB_AUTOBOOT)` (reboot standard, pas power-off) qu'il
  faut appeler : il declenche un reset via le controleur clavier i8042 que
  Firecracker intercepte lui-meme comme signal de fin de VM.
- Un `Job` Kubernetes n'est marque termine que lorsque tous ses
  `containers[]` (pas ses `initContainers[]`, sidecars natifs compris) ont
  fini avec succes : un sidecar long-vivant (ex: `net-proxy`, jamais de code
  de sortie) doit etre declare comme `initContainer` avec
  `restartPolicy: "Always"` (K8s >= 1.28/1.29, KEP-753 "sidecar containers")
  et non dans `containers[]`, sous peine d'un `Job` qui reste `Running`
  indefiniment meme apres la fin reelle du conteneur principal.
- `COPY --from=<stage> /` dans un `Dockerfile` multi-etapes aplatit
  l'integralite du filesystem de ce stage sans passer par un registre OCI :
  equivalent local a `crane export` + `tar xf` (utilise ailleurs dans ce
  projet pour aplatir une image deja poussee), mais inutile ici puisque le
  contenu est deja disponible localement dans le build multi-etapes —
  simplifie significativement la construction d'un rootfs bootable a partir
  d'un stage Docker intermediaire (voir `crates/image-builder/Dockerfile`).
- Une image Docker rechargee dans `kind` (`kind load docker-image`) ne
  signale jamais qu'elle est perimee : reconstruire le code sans reconstruire
  ET recharger l'image dans le cluster produit des echecs silencieux et
  deroutants (le nouveau code n'est simplement jamais execute). Toujours
  verifier `docker images <nom>` / refaire `kind load` apres tout changement
  de code affectant une image utilisee en cluster.
- `fctools` 0.7.0-alpha.2 : `VmConfigurationData` ne derive que `Serialize`
  (pas `Deserialize`), et `Vm::restore` exige un `Vm` source vivant dans le
  meme process (son `ResourceSystem` fournit les ressources a recopier) —
  cette API n'est pas concue pour un snapshot qui doit survivre a un
  redemarrage complet du process appelant. Contournement viable : ne pas
  serialiser `VmConfigurationData` du tout, la **reconstruire a l'identique**
  a partir des memes parametres qu'un boot normal (elle est entierement
  determinee par eux) — la coherence du chemin virtuel jaile
  (`FlatVirtualPathResolver`, base sur le nom de fichier, pas sur l'identite
  de la ressource) rend cette reconstruction valide sans avoir besoin de
  l'objet source. Voir `Vm::restore_persisted`,
  `crates/firecracker/src/vm.rs`.
- Le `controller`, lance depuis un poste de dev (hors cluster), ne peut pas
  joindre une IP de pod kind directement — le reseau du CNI (10.244.0.0/16)
  n'est pas route vers l'hote par defaut. `docker run --network
  container:<nom-du-noeud-kind>` (partage le netns du noeud, qui lui route
  bien vers ses propres pods) contourne ce blocage pour du test manuel —
  meme categorie de contournement que celui deja documente pour
  `CAP_NET_ADMIN`/la microVM builder plus haut. En production, le
  `controller` tournant *dans* le cluster (pas encore le cas dans cette
  session de dev), ce probleme ne se pose simplement pas.
- Un `Job` Kubernetes ne supprime pas son pod a la fin (`Complete`) — le
  pod reste visible (`Terminating` puis disparait apres la periode de
  grace par defaut, ~30s) : un controller qui interroge ce pod pendant
  cette fenetre (ex: pour un appel de controle avant liberation) peut le
  voir "encore la" alors que son process principal a deja fini — prevoir
  l'idempotence/tolerance a un second appel plutot que supposer qu'un seul
  suffira.
- Le parametre de boot noyau Linux standard `ip=<client-ip>:<server-ip>:
  <gw-ip>:<netmask>:<hostname>:<device>:<autoconf>` (autoconfiguration IP,
  cf. `Documentation/admin-guide/nfs/nfsroot.rst`) configure une interface
  et sa route par defaut **avant que l'init du guest ne demarre**, sans
  aucune cooperation de celui-ci — le bon outil pour donner une IP a une VM
  dont on ne controle pas l'init (contrairement a la microVM "builder", qui
  a son propre init personnalise et peut donc configurer son reseau
  elle-meme en espace utilisateur).
- Chaque nouvelle image Docker qui commence a faire du reseau/pare-feu doit
  explicitement installer `iproute2`/`iptables` — l'oubli est facile
  (`image-builder` puis `vm-supervisor` ont chacun ete oublies une fois
  cette session) et le symptome (`lancement de ip: No such file or
  directory`, ou pire un `CrashLoopBackOff` sans message clair au premier
  coup d'oeil) ne pointe pas immediatement vers la cause.
- `jsonwebtoken::Validation::new()` active `validate_aud: true` par defaut :
  sans `set_audience(...)` explicite, n'importe quel token portant un `aud`
  (le cas de tout token OAuth2 reel emis par Kanidm) est rejete en
  `InvalidAudience` — invisible avec des JWT de test synthetiques qui
  n'incluent jamais `aud`. A ne decouvrir qu'en testant contre un vrai flux
  OAuth2, jamais avec des tokens fabriques a la main pour les tests.
- `reqwest` compile avec la feature `rustls-tls` ignore le trust store
  systeme et `SSL_CERT_FILE` : contre un service TLS a CA auto-signee (ex:
  Kanidm de dev), il faut construire un `Client` avec
  `.add_root_certificate(reqwest::Certificate::from_pem(...))` explicitement
  — un simple `reqwest::get()` echoue toujours, meme si `curl`/le systeme
  font confiance a cette CA.
- Donner a un `Pod` de test un vrai conteneur planifiable (ex:
  `registry.k8s.io/pause:3.9`) fait qu'un vrai kubelet le prend en charge et
  **ecrase** tout `status.podIP` patche manuellement par son adresse CNI
  reelle des que le conteneur demarre — y compris apres un premier
  `patch_status` reussi (course avec la reconciliation continue du
  kubelet). Pour un test qui a seulement besoin de controler `status.podIP`
  sans faire tourner de vrai conteneur, fixer `spec.nodeName` sur un nom de
  noeud inexistant : le Pod reste `Pending` a jamais (aucun kubelet ne le
  reclame), et le patch de statut manuel n'est plus jamais ecrase.
- `tonic-build` 0.14 a deplace toute la generation de code liee a `prost`
  vers un crate separe, `tonic-prost-build` (`tonic_build::configure()`
  n'existe plus) — cote runtime, le crate correspondant est
  `tonic-prost` (pas seulement `prost`). `tonic-prost-build` (comme
  `prost-build`) invoque le binaire externe `protoc` au moment du build ;
  sur une machine sans `apt`/`sudo`, un binaire precompile
  (github.com/protocolbuffers/protobuf/releases) pointe via la variable
  d'environnement `PROTOC` (`.cargo/config.toml` local, non commite car
  specifique a la machine) fonctionne tout aussi bien.
- Un `DaemonSet` qui bind-mounte tout `/dev` de l'hote (au lieu de devices
  individuels) casse le mount par defaut de kubelet sur
  `/dev/termination-log` (`CrashLoopBackOff` immediat, "read-only file
  system" au moment de creer ce point de montage) : toujours monter les
  devices necessaires individuellement (`hostPath` par device, type
  `CharDevice`), jamais `/dev` entier.
- Pour un device plugin Kubernetes (`/dev/kvm` ici), `securityContext.capabilities.add: [NET_ADMIN]`
  suffit a creer un TAP (`ip tuntap add`) mais **pas** a spawner un process
  dont les capabilities effectives viennent de `setcap` sur son binaire
  (ex: `jailer`, cf. Dockerfile `vm-supervisor`) : celui-ci echoue en
  "Operation not permitted" au spawn si `SYS_ADMIN`/`SYS_RESOURCE` ne sont
  pas *aussi* explicitement ajoutees au conteneur — les capabilities de
  fichier ne peuvent etre elevees a l'exec que si elles font deja partie du
  bounding set du conteneur, l'ensemble par defaut containerd/Docker
  (`CHOWN`, `DAC_OVERRIDE`, `FOWNER`, `SETUID`, `SETGID`, `SYS_CHROOT`,
  `MKNOD`, etc.) ne les couvrant pas. Un restart de pod qui echoue *apres*
  la creation du TAP mais *avant* que Firecracker demarre laisse ce TAP
  vivant dans le netns du pod (partage entre redemarrages d'un meme pod,
  contrairement a un nouveau pod) : le redemarrage suivant echoue alors en
  `EBUSY` sur la creation du TAP, masquant completement l'erreur d'origine
  (`SYS_ADMIN` manquant) derriere un symptome different — a diagnostiquer
  avec `restartPolicy: Never` pour capturer le tout premier essai sans
  bruit de redemarrage.
- L'endpoint API `/oauth2/authorise` de Kanidm (utilise par
  `get-oauth2-token.sh`) exige un `Authorization: Bearer` deja present —
  confirme en testant reellement : `401 Unauthorized` systematique sans ce
  header, jamais de redirection vers une page de login. Ce n'est donc PAS
  l'URL a utiliser pour un flux browser-based classique (un navigateur sans
  session ne peut pas fournir ce bearer sur une simple navigation) :
  `/ui/oauth2` (meme query params) sert la SPA Kanidm, qui gere son propre
  login+consentement en JS avant de rediriger reellement le navigateur vers
  `redirect_uri` — c'est cette URL qu'un client OAuth2 browser-based doit
  cibler, pas l'endpoint API.
- Un client OAuth2 public Kanidm (`create-public`, PKCE) refuse par defaut
  toute `redirect_uri` pointant vers `localhost` (protection standard contre
  le detournement d'un client public en local) : necessite
  `enable-localhost-redirects` explicite en plus de `add-redirect-url`,
  sans quoi l'echange de code echoue meme avec une redirect_uri par ailleurs
  correctement enregistree.
- Une image devcontainer standard (`mcr.microsoft.com/devcontainers/base`)
  n'a **pas** de systeme init installe : bootee directement par un noyau
  (sans runtime de conteneur), le PID 1 retombe sur un `/bin/sh` nu apres
  l'echec de `/sbin/init`/`/etc/init`/`/bin/init` — rien ne demarre tout
  seul, `postStartCommand` n'etant rejoue que par le CLI `devcontainer`.
  Ajouter `systemd`/`systemd-sysv` et declarer ses propres services comme
  unites systemd est necessaire pour tout devcontainer destine a booter
  comme microVM.
- Dans une image devcontainer standard, `systemctl enable <unit>` echoue
  silencieusement (exit 0, aucun symlink cree) : un faux `systemctl` y est
  installe pour eviter que l'installation de paquets `.deb` porteurs
  d'unites systemd echoue pendant un `docker build` classique (pas de PID 1
  systemd a ce moment-la). Contournement : creer soi-meme le symlink
  d'activation (`ln -s ../mon-service.service
  /etc/systemd/system/multi-user.target.wants/mon-service.service`), comme
  le fait `deb-systemd-helper` pour les paquets (qui, lui, n'est pas
  intercepte).

- Une chaine `iptables` qui filtre par liste blanche de *ports de
  destination* jette le trafic retour des connexions qu'on initie soi-meme
  (le port de destination y est un port ephemere, jamais dans la liste). Il
  faut un `-m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT` en premiere
  regle, sinon la connexion part mais son SYN-ACK n'arrive jamais et le
  symptome (`Connection timed out`) fait chercher du cote du service cible,
  qui lui va parfaitement bien.
- Un serveur Next.js custom n'a pas l'exclusivite de l'evenement `upgrade` :
  Next accroche son propre listener sur *notre* serveur des la premiere
  requete HTTP servie (via `req.socket.server`), et ce listener fait
  `socket.end()` sur tout chemin qui correspond a une route Next — un
  Route Handler catch-all suffit. Deleguer explicitement a
  `app.getUpgradeHandler()` ce qui ne nous concerne pas, et neutraliser
  l'auto-attachement pour le reste.
- Un bug qui n'apparait qu'*apres* la premiere requete HTTP d'un process
  ressemble a s'y meprendre a de la flakiness. Avant de conclure
  "intermittent", chercher l'etat du process qui change entre deux essais :
  ici un test qui reussissait 6/6 sur un serveur fraichement redemarre
  echouait 2/2 une fois une page chargee.
- `fetch` (undici) decompresse le corps selon `Content-Encoding` mais laisse
  cet en-tete et `Content-Length` intacts sur `response.headers`. Un proxy
  qui les relaie tels quels produit un `ERR_CONTENT_DECODING_FAILED` cote
  navigateur : les retirer tous les deux.
- Un client WebSocket qui a demande un sous-protocole
  (`Sec-WebSocket-Protocol`, ex. `tty` pour `ttyd`) ferme la connexion en
  `1006` si le `101` ne le confirme pas. Un proxy WebSocket doit relayer le
  sous-protocole dans les deux sens, pas seulement les octets.
- Tester un pont WebSocket avec un client `ws` Node.js ne remplace pas un
  vrai navigateur : plusieurs bugs de ce lot (fermeture par Next, absence de
  sous-protocole) ne se manifestaient que via l'`<iframe>` et le vrai client
  du service. Un navigateur pilote (Chromium headless, cookies de session
  injectes) est peu couteux a mettre en place et aurait fait gagner
  beaucoup de temps.
- Cote dashboard, `npm run dev` est `tsx watch server.ts` : lancer
  `npx tsx server.ts` a la main ne surveille pas le fichier, et on teste
  alors une version obsolete de `server.ts` sans s'en rendre compte
  (plusieurs resultats "incoherents" venaient de la).
- En Next.js 16 (App Router + Turbopack + React 19), le mode dev initialise un
  `debugChannel` de debug React dans le client (`createFromReadableStream`). Ce
  stream attend la fermeture du canal de debug transitant par le WebSocket HMR
  (`/_next/hmr`). Dans un custom server (`dashboard/server.ts`), déléguer
  l'upgrade WebSocket via `app.getUpgradeHandler()` au lieu de
  `(app as any).upgradeHandler` bloquait silencieusement la socket HMR : le
  stream du `debugChannel` ne se fermait jamais, `initialServerResponse` restait
  `pending` indéfiniment, et l'hydratation React (`hydrateRoot` / `useEffect`)
  ne s'exécutait sur aucune page sans lever la moindre erreur en console.
  Résolu en utilisant `(app as any).upgradeHandler(req, socket, head)`.
