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
- **Un modele a tarif fictif fausse toute lecture de la depense.** Les
  modeles `atelier-*-test` sont factures des dollars par requete pour exercer
  l'application des plafonds sans attendre une vraie consommation. Le
  2026-09-01, les journaux LiteLLM affichaient 211,49 $ dont 210,00 $
  provenaient de QUATORZE requetes de test a 15 $ piece : la depense reelle
  etait de 1,49 $. J'ai failli rapporter le premier chiffre. Toute agregation
  presentee a un humain doit les ecarter — et le DIRE, un montant retire sans
  etre montre est un montant qu'on finira par ne plus savoir expliquer.
- **Deux services derriere le meme mot de passe n'acceptent pas la meme
  preuve.** `ttyd --credential` implemente un vrai Basic Auth ; `code-server
  --auth password` l'IGNORE et redirige vers `/login` tant qu'il n'a pas son
  cookie `code-server-session`, meme face a une requete parfaitement
  authentifiee en Basic. Le pont de l'`api-server` injectait le meme en-tete
  pour les deux, et son commentaire affirmait que « les deux exigent ce Basic
  Auth » — mesure le 2026-09-01 : faux pour code-server. Un `302` n'est pas
  un refus visible, la panne ressemblait a une page de login normale. Quand
  un service parle un protocole d'authentification, le verifier plutot que
  le supposer : `curl -u ...` doit rendre `200`, pas `302`.
- **Une note « verifie absent » se perime.** La ligne `[~]` du plan disait
  que le devcontainer ne consommait pas `/session-auth`, sur la foi d'une
  verification faite dans les CLONES LOCAUX. Le depot amont l'avait fait
  depuis. Une affirmation sur un depot tiers doit nommer ce qui a ete
  regarde (« clone local a telle date »), sinon elle se lit comme un fait
  durable et gele une tache qui n'a plus lieu d'etre.
- **Un test qui se saute en CI pourrit sans que rien ne le dise.** Les tests
  qui exigent de la vraie infrastructure (cluster, Postgres, Forgejo) se
  sautent d'eux-memes en CI faute de connexion : ils ne deviennent donc
  jamais rouges quand le code change sous eux. Constate le 2026-09-01 : sept
  d'entre eux etaient casses, dont six par le passage a la propriete par
  GROUPE de la veille (jeton sans `groups` ni role -> `403`) et un par
  l'enrobage `cd /workspaces/<repo>` des commandes deleguees. La CI etait
  verte du debut a la fin. Apres tout changement touchant l'autorisation ou
  la forme des commandes, lancer `cargo test --workspace` sur une machine
  qui A l'infrastructure — la CI ne peut pas le faire a votre place.
- **Un binaire de dev laisse tourner corrompt les tests d'integration.** Un
  `target/debug/atelier-controller` oublie en arriere-plan reconcilie TOUS
  les Workshops du cluster, y compris ceux que les tests viennent de creer :
  il supprime le pod que le test s'apprete a verifier. Symptome le
  2026-09-01 : `apply_suspend_then_resume` echouait une fois sur deux en
  suite complete et passait 3/3 seul — ce qui ressemble exactement a une
  course dans le code, et n'en etait pas une. `pgrep -af target/debug`
  avant de diagnostiquer un test intermittent.
- **`cargo clippy` ne produit pas d'executable.** Verifier avec clippy puis
  relancer `target/debug/<binaire>` fait tourner l'ANCIEN binaire : le
  correctif compile, passe le lint, et ne s'execute pas. Constate le
  2026-08-31 sur le confinement de securite — le controller reconciliait
  toutes les 15 s sans erreur et n'ecrivait jamais la condition attendue, ce
  qui a envoye chercher la cause du cote du schema du CRD et de l'elagage
  Kubernetes. `cargo build -p <crate>` avant tout redemarrage manuel ; en cas
  de doute, comparer l'horodatage du binaire a celui de la modification.
- **Une regle d'injection ne sert a rien si la requete ne passe pas par
  l'injecteur.** Le controller creait bien une Virtual Key par Workshop
  (`atelier-wks-<nom>`), la plafonnait, l'ecrivait dans OpenBao et generait la
  regle d'injection — mais `net-proxy` aiguillait l'alias `llm-proxy` DROIT
  vers LiteLLM, la ou l'alias Git pointe vers `identity-proxy`. La cle
  n'etait donc jamais utilisee : le guest envoyait le jeton statique partage,
  et le plafond ne contraignait rien. Corrige en reproduisant le montage Git
  (alias -> identity-proxy + `hostAlias` vers le ClusterIP du service), ce qui
  a demande d'exposer LiteLLM sur le **port 80** : une microVM ne sort que sur
  80 et 443, et `identity-proxy` se connecte au port de la requete du guest.
  **Verification qui tranche** : `kubectl logs <pod> -c identity-proxy` doit
  afficher `credential injecte host="llm-proxy"`. Sans cette ligne, la cle est
  ignoree quoi qu'en dise la configuration.
- **LiteLLM facture 0 pour une combinaison provider/modele dont il ignore le
  tarif.** `anthropic/deepseek-chat` (endpoint Anthropic natif de DeepSeek)
  n'a pas de grille integree : `/model/info` renvoie
  `input_cost_per_token: 0`, toute la comptabilite reste a zero et les
  plafonds de Virtual Key ne se declenchent jamais — en silence, puisque les
  appels reussissent. Il faut poser `input_cost_per_token`/
  `output_cost_per_token` explicitement dans `model_list`. Les valeurs a
  utiliser sont celles que LiteLLM applique deja au meme modele sous son
  provider natif (`deepseek/deepseek-chat`), lisibles dans `/model/info` :
  aucun chiffre a inventer. **Symptome** : injection confirmee dans les logs
  d'identity-proxy, mais `spend` obstinement a `0.000000`.
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
- Le binaire `claude` standalone (compile Bun,
  `/usr/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe`, ~245 Mo)
  segfault systematiquement dans un Workshop batti sur
  `mcr.microsoft.com/devcontainers/python:1-3.12`, avec une adresse fautive
  DIFFERENTE a chaque invocation (`0xFFFFFFFFFFFFFFBE` avec un prompt,
  `0x0` sur `--version`, `0xBBADBEEF` — une valeur de poison memoire
  classique — en `chroot`). Deux fausses pistes ecartees par la preuve
  avant la bonne (2026-09-01) :
  1. Pas une corruption reseau/registre : `claude.exe` extrait directement
     des layers OCI du registre (`sha256sum` calcule sans erreur) est
     bit-a-bit identique a celui lu depuis le `rootfs.ext4` monte en loop
     sur le noeud kind (meme `sha256`).
  2. Pas une corruption du systeme de fichiers ext4 : `e2fsck -n` sur le
     `rootfs.ext4` reellement utilise par le Workshop (retrouve dans le PVC
     `atelier-image-cache`, `/var/local-path-provisioner/.../sha256_<digest
     du Workshop>/rootfs.ext4` sur le conteneur du noeud kind) rapporte
     `clean` — aucune corruption de metadonnees.
  **Preuve retenue** : le meme `claude.exe`, lance en `chroot` sur ce
  `rootfs.ext4` monte, EN DEHORS DE TOUTE MICROVM/FIRECRACKER, segfault a
  l'identique (`Bun v1.4.0 ... panic(main thread): Segmentation fault`) —
  sur le noeud kind lui-meme (meme CPU, meme noyau hote que le reste du
  systeme). Le meme binaire, execute via `node
  /usr/lib/node_modules/@anthropic-ai/claude-code/cli-wrapper.cjs
  --version` (le launcher Node.js de secours du paquet npm, normalement
  jamais invoque car le postinstall copie le binaire natif par-dessus)
  fonctionne parfaitement (`2.1.197 (Claude Code)`, exit 0) dans le MEME
  chroot. Conclusion : bug du runtime Bun standalone lui-meme dans un
  environnement restreint (chroot minimal sans `/proc` complet au moment du
  premier essai, microVM Firecracker ensuite) — pas une corruption de
  donnees ni un probleme de CPU/architecture. Contournement immediat pour
  `pm-engine` (non applique) : invoquer `node
  .../claude-code/cli-wrapper.cjs` au lieu de `claude` dans
  `DelegateToClaudeCode`, ou reinstaller le paquet npm avec
  `--ignore-scripts` pour empecher le postinstall d'ecraser le wrapper par
  le binaire Bun.
  Constate en validant reellement le chantier planificateur (ticket
  greenfield, 2026-09-01).
  Defaut connexe corrige le meme jour (reste utile independamment de la
  cause ci-dessus) : `DelegateToClaudeCode`
  (`pm_engine.nodes`) ne verifiait PAS DU TOUT le resultat de son propre
  `exec_in_workshop` — le crash restait invisible jusqu'a
  `RunDevcontainerTests`, qui echouait sur `.devcontainer/test.sh: No such
  file or directory` (symptome trompeur, pointe vers un oubli de l'agent
  alors qu'il n'a jamais tourne), et `AutoCorrectionLoop` rappelait le meme
  binaire casse jusqu'a epuiser tout le budget de correction (3 tentatives
  identiques, aucune ligne de code ecrite). Le noeud echoue desormais
  immediatement des que `exit_code != 0`, sans passer par la boucle de
  correction — une erreur d'environnement ne se corrige pas en reformulant
  le prompt. Reverifie en reel : la deuxieme tentative de delegation crashe
  a l'identique mais fait echouer le graphe immediatement, sans les 3 tours
  de correction inutiles.
- `ensure_image_build_job` (`crates/controller/src/reconcile.rs`) ne cable
  PAS `ATELIER_GIT_ALIAS_ADDR`/`ATELIER_LLM_PROXY_ADDR`/`hostAliases` sur le
  sidecar `net-proxy` du Job `image-builder`, contrairement a
  `ensure_parent_pod` (le pod RUNTIME du Workshop, lui correctement cable) —
  verifie en lisant les deux fonctions cote a cote. Constate en pratique
  (2026-09-01) : un Job `image-builder` pointant `devcontainerRepo` vers le
  depot cible sur `git.atelier.internal` echoue avec `connexion directe a
  git.atelier.internal:3000` (`net-proxy::upstream::connect`, resolution DNS
  normale, PAS l'alias interne) — la microVM builder clone donc dans le
  vide et s'eteint en ~3s sans avoir rien construit. Pourquoi ca n'a pas
  bloque le run reel de validation du meme jour : le nom de Workshop
  `pm-1-task-1` avait deja 16 versions en cache dans le registre
  (`atelier-workshops/pm-1-task-1`, sessions anterieures) — le Job a
  probablement reutilise une image cachee au lieu de rebuild depuis MON
  `.devcontainer/devcontainer.json`. Autrement dit : tout Workshop dont le
  `devcontainerRepo` EST le depot cible (le flux normal de `pm-engine`,
  pas un devcontainer externe type `vscode-remote-try-python`) risque de ne
  jamais pouvoir (re)construire son image la premiere fois, en silence si
  le cache masque le probleme.
  **Corrige et verifie en reel le meme jour** : `ensure_image_build_job`
  cable desormais `ATELIER_GIT_ALIAS_ADDR` sur son sidecar `net-proxy`
  (resolution directe du ClusterIP de la forge via
  `git_identity::resolve_cluster_ip`, sans passer par `identity-proxy` —
  ce Job n'en a pas, l'auth Git au build passe deja par
  `resolve_git_credentials` cote `image-builder`, jamais par l'injection
  d'en-tete). Deuxieme cause distincte trouvee en verifiant :
  `ATELIER_GIT_HOST_SERVICE` n'etait meme pas defini dans l'environnement
  du controller de dev (`ctx.git_identity` valait `None`, feature
  entierement inactive, RUNTIME compris) — ajoute a
  `deploy/dev/local-stack/env.sh`. Avec les deux, une microVM builder
  fraiche (nom de Workshop jamais vu, donc sans cache registre) reste
  active des minutes durant au lieu de s'eteindre en 3s : preuve que
  `git.atelier.internal` resout desormais et que le clone demarre
  reellement.
- Une microVM builder qui clone avec succes peut ensuite rester bloquee
  SANS AUCUNE erreur, en plein telechargement d'un binaire externe
  volumineux (constate deux fois, 2026-09-01, en installant `opencode`
  dans un devcontainer — `curl | bash` PUIS, separement, le postinstall npm
  de `opencode-ai`, memes symptomes les deux fois) : `net-proxy` journalise
  `egress autorise ... host="release-assets.githubusercontent.com" ...
  allowed=true` (le tunnel CONNECT s'etablit), puis plus AUCUNE ligne
  pendant 10+ minutes, alors que le process `firecracker` de la microVM
  reste vivant (CPU/memoire en hausse lente, pas de crash). Aucun
  `rx_rate_limiter`/`tx_rate_limiter` configure cote Firecracker
  (`crates/firecracker/src/vm.rs`) — la lenteur n'est donc pas une
  limitation de bande passante deliberee. Cause non identifiee : a
  suspecter le relais CONNECT de `net-proxy` face a un gros telechargement
  HTTPS (redirections de CDN GitHub, HTTP/2, ou un bug de streaming qui
  bufferise tout avant de relayer).
  **Contourne, pas corrige, le meme jour** : `opencode` est desormais baque
  dans l'image `atelier-image-builder` elle-meme (telecharge au `docker
  build`, reseau normal du host — 2,7s reels mesures, contre 10+ minutes
  bloquees via ce chemin) et injecte directement dans le rootfs par
  `inject_opencode_binary` (`crates/image-builder/src/main.rs`), sans
  jamais passer par `net-proxy`. Le bug lui-meme (le tunnel CONNECT qui se
  bloque) reste non identifie et non corrige — seul le cas d'usage
  `opencode` n'en depend plus.
- **`opencode` segfault EXACTEMENT comme `claude.exe`** (2026-09-01,
  meme jour) : c'est aussi un executable Bun standalone
  (`bun run --compile`, confirme par `npm pack opencode-linux-x64` —
  meme `sha256` que le binaire injecte par `inject_opencode_binary`, donc
  pas un probleme de mauvaise variante baseline/avx2 selectionnee au
  build). Meme signature de crash (`Bun vX.Y.Z ... panic(main thread):
  Segmentation fault`, adresse fautive differente a chaque essai, dont
  `0xBBADBEEF`), reproduite dans le MEME `chroot` du `rootfs.ext4` reel qui
  avait fait crasher `claude.exe` plus tot ce jour. **Consequence directe
  pour le chantier "remplacer Claude Code par opencode"** : le motif
  initial (fuir ce crash) ne tient pas — les deux CLI partagent la meme
  fragilite Bun dans cet environnement precis. Le motif licence/open
  source, lui, reste valable independamment. Contrairement a
  `@anthropic-ai/claude-code`, le paquet npm `opencode-ai` ne fournit
  AUCUN launcher Node de secours (`postinstall.mjs` ne pose que
  `bin/opencode.exe`, pas de `.cjs` invocable via `node`) : pas de
  contournement equivalent a `node cli-wrapper.cjs` disponible ici. Cause
  racine du crash Bun toujours non identifiee (`strace` indisponible sur le
  noeud kind pour investiguer plus loin) — reste ouvert, bloquant pour
  toute execution reelle d'un agent Bun-compile dans ce Workshop, quel que
  soit le CLI choisi.
- `ttyd` (terminal web), `code-server` (IDE web), et l'utilisateur
  `vscode`/uid 1000 dont `inject_workspace_refresh` supposait deja
  l'existence n'etaient fournis QUE par le devcontainer de demo externe
  (`github.com/PhilippeVienne/atelier-workspace`), jamais par
  `image-builder` — verifie en lisant `crates/vm-supervisor`,
  `crates/net-proxy` et `crates/controller/src/openbao.rs` : aucun
  n'installe quoi que ce soit, tout est seulement *consomme* (sonde de
  readiness `GUEST_TERMINAL_PORT=7681`, endpoints metadata
  `/session-auth`/`/ssh-authorized-key`). Consequence reelle : un Workshop
  sur n'importe quel autre depot cible ne repondait jamais sur `ttyd:7681`
  et ne passait donc jamais `Running`.
  **Corrige et verifie en reel le meme jour** (2026-09-01, meme technique
  qu'`inject_opencode_binary`) : `ttyd` (binaire statique) et
  `code-server` (archive autonome, Node embarque) sont desormais baques
  dans l'image `atelier-image-builder` au `docker build` et injectes par
  `inject_terminal_and_ide` (`crates/image-builder/src/main.rs`), avec
  `ensure_vscode_user` qui cree l'utilisateur/groupe `vscode` directement
  dans `/etc/passwd`/`/etc/group`/`/etc/shadow` du rootfs SI absent (au
  lieu de le supposer). Verifie sur un vrai rootfs monte en boucle : les
  deux unites systemd sont installees et activees (symlinks
  `multi-user.target.wants/`), `ttyd --version` s'execute sans probleme en
  `chroot` (binaire natif, aucun rapport avec le crash Bun ci-dessus).
  **Non couvert par cette correction** (reste ouvert) :
  - `sshd` et le script `atelier-fetch-ssh-authorized-key.sh`
    (`ssh-authorized-key` du meme endpoint metadata) — plus complexe
    (config sshd, generation de host keys) et pas sur le chemin critique
    du readiness-probe, laisse pour un chantier separe.
  - Aucune image de base sans systemd n'a ete testee : `vm-supervisor` ne
    passe aucun `init=` au noyau (`ATELIER_VM_BOOT_ARGS`), donc PID1 du
    guest reste `/sbin/init` de l'image cible tel quel — une image sans
    systemd (beaucoup d'images "slim" le sont) n'executerait AUCUNE des
    unites installees ici, silencieusement. Question deja posee sans
    reponse dans `docs/archive/PROGRESS-2026-08.md:860-864` : soit le
    devcontainer source installe son propre systeme init, soit
    `image-builder`/`vm-supervisor` devront un jour en injecter un
    generique (a la maniere d'`atelier-builder-vm-init` pour la microVM
    builder) — toujours non tranche.
  **Les deux points corriges et verifies en reel le meme jour (2026-09-01)** :
  - `sshd` : embarque (binaire Debian + bibliotheques resolues par `ldd`,
    executees via `ld.so --library-path`) et injecte par `inject_sshd`.
    Deux bugs reels trouves en testant une vraie connexion SSH de bout en
    bout (pas seulement `sshd -t`) :
    1. `sshd` se RE-EXECUTE lui-meme (`execve`) a chaque connexion
       entrante, en repartant du chemin binaire brut — le wrapper
       `ld.so --library-path` ne survit pas a ce re-exec
       (`libwrap.so.0: cannot open shared object file`, alors meme que le
       fichier est present dans le lot de bibliotheques embarquees).
       `LD_LIBRARY_PATH` (variable d'environnement normale, heritee par
       tout processus enfant/re-exec) est necessaire EN PLUS du wrapper,
       pas a sa place.
    2. Un compte avec `!` en `/etc/shadow` (verrou explicite pose par
       `ensure_system_user` pour "aucun mot de passe utilisable") est vu
       par `sshd` comme ADMINISTRATIVEMENT VERROUILLE ("User vscode not
       allowed because account is locked") — un blocage qui s'applique a
       TOUTE methode d'authentification, y compris par cle publique,
       contrairement a ce qu'on pourrait supposer. `*` a la place n'a pas
       ce defaut. Touche aussi les comptes `vscode` PRE-EXISTANTS de
       l'image de base (Microsoft en pose un avec `!`) : `unlock_shadow_
       password` corrige desormais tout compte, cree par nous ou non.
    3. (Mineur mais reel) `UsePrivilegeSeparation no` est un directive
       DEPRECIEE, silencieusement ignoree par OpenSSH >= 7.5 — `sshd`
       exige toujours un compte systeme dedie (`sshd`, cree par
       `ensure_sshd_user`) pour sa separation de privileges, quoi que dise
       la config.
    Verifie par une vraie connexion `ssh vscode@<guest> whoami` reussie
    (cle publique, cle hote generee au premier demarrage).
  - Init sans systemd : nouveau crate `crates/guest-init`
    (`atelier-guest-init`), modele sur `atelier-builder-vm-init` mais
    PERSISTANT (ne reboote jamais) — monte les pseudo-filesystems, lance
    les scripts de service en arriere-plan avec relance sur sortie, et
    boucle sur `waitpid` pour recolter les zombies (responsabilite non
    negociable d'un PID 1). Le reseau n'a pas besoin d'etre reconfigure :
    le noyau le fait lui-meme au boot (`ip=`, deja pose par
    `vm-supervisor::kernel_ip_boot_arg`). `ensure_init_system`
    (`image-builder`) detecte l'absence de `systemd` dans le rootfs
    construit et bascule `/sbin/init` vers ce binaire dans ce cas
    seulement — les images avec systemd gardent leur fonctionnement
    inchange. Verifie sur `debian:bookworm-slim` (sans systemd, sans
    utilisateur `vscode` prealable) : `ttyd`/`code-server`/`sshd` demarrent
    tous les trois sous ce nouvel init (`unshare --pid` reel, pas une
    simulation), et un `kill -9` sur l'un d'eux declenche bien sa relance
    automatique.
- `deploy/dev/keycloak/realm-export.json` contenait un faux champ
  `"//serviceAccountRoles": "<note explicative>"`, invente pour glisser un
  commentaire dans du JSON (qui n'en supporte pas). Keycloak avait importe
  ce fichier une seule fois avec succes a la creation du realm ; l'import
  n'est rejoue qu'a la prochaine absence du realm en base, jamais a un
  simple redemarrage — le fichier invalide est donc reste latent, invisible,
  pendant des jours. Constate le 2026-09-02 quand un redemarrage du
  conteneur du noeud kind (recuperation apres un incident sans rapport, voir
  plus bas) a fait perdre l'etat du realm : Keycloak a retente l'import et a
  echoue net (`Unrecognized field "//serviceAccountRoles"`),
  `atelier-keycloak-dev` en `CrashLoopBackOff`. Le vrai role
  (`atelier-pm-bot` -> `developer`, sans quoi l'api-server refuse la
  creation de Workshops en 403, voir `DEVELOPER_ROLE` dans
  `crates/api-server/src/routes.rs`) est correctement pose ailleurs dans le
  fichier (`users[].realmRoles`) — ce faux champ n'etait qu'une note
  redondante. Retire ; toute note sur ce fichier doit vivre dans un
  commentaire Markdown a cote, jamais comme un champ JSON invente.
- **Incident reel, cause par une erreur de nettoyage** (2026-09-02) : un
  `rm -rf` lance sur un repertoire de test QUI CONTENAIT ENCORE DES
  MONTAGES LIES (`mount --bind /dev`, pose pour un test `chroot` du crash
  Bun ci-dessus) a traverse le bind-mount et supprime pour de vrai
  `/dev/null` du conteneur du noeud kind — un bind-mount partage les memes
  entrees que la source, une suppression a travers l'un supprime l'autre.
  Consequence : `docker exec` casse entierement sur ce conteneur
  (`unable to setup user: stat /dev/null: no such file or directory`),
  aucune commande possible dedans, y compris pour reparer. Recupere par un
  `docker restart` du conteneur (choix de l'utilisateur, l'autre option
  etant un `mknod` manuel en root — indisponible sans `sudo` interactif
  depuis cette session) : `/dev` de ce noeud est un `devtmpfs`, repeuple
  automatiquement par le noyau au (re)demarrage. Le redemarrage a lui-meme
  revele le bug ci-dessus (Keycloak). Regle a appliquer desormais, sans
  exception : ne JAMAIS `rm -rf` un repertoire qui a servi de point de
  montage sans verifier `mount | grep <repertoire>` et tout demonter
  d'abord — un repertoire de test avec des bind-mounts actifs n'est jamais
  "juste des fichiers".
- **Le crash Bun ne se reproduit PAS dans une vraie microVM** (2026-09-02,
  conclusion de l'enquete ouverte la veille) : `opencode --version` puis un
  `opencode run` complet tournent sans incident dans un Workshop
  Firecracker fraichement construit (`/proc`, `/sys` et `/dev` y sont tous
  les trois montes, verifie). Le SIGILL — rapporte par le gestionnaire de
  crash de Bun comme un "segfault", ce qui a longtemps oriente le
  diagnostic a cote — ne survient que dans un environnement d'execution
  ampute de ces pseudo-systemes de fichiers, typiquement un `chroot` de
  diagnostic monte a la main. Ce n'est ni la glibc (binaire strictement
  identique, il tourne sur l'hote comme dans un `docker run`), ni le CPU
  hybride (teste sous `taskset`), ni une corruption de l'image. Corollaire
  de methode : un `chroot` minimal n'est PAS un substitut fidele a la
  microVM pour reproduire un plantage d'exécutable.
- **La console serie du guest est journalisee en `debug!`** (2026-09-02) :
  `drain_console_pipes` (`crates/firecracker/src/vm.rs`) envoie toute la
  sortie de la microVM builder dans `tracing::debug!`. Au niveau `INFO` par
  defaut du Job `*-image-build`, un echec d'`envbuilder` est donc
  totalement invisible : le seul symptome visible est une microVM qui
  "s'eteint trop vite" puis un `crane export` qui echoue en
  `MANIFEST_UNKNOWN` — message qui ne dit rien de la vraie cause. Plusieurs
  heures ont ete perdues a soupconner KVM, le registre et le redemarrage du
  noeud, alors qu'un seul run avec `RUST_LOG=debug` donnait la reponse en
  clair (ici : `reference not found`, le depot de test n'avait pas de
  branche `main`). Reflexe a avoir : devant un build de devcontainer qui
  echoue sans explication, rejouer le Job avec `RUST_LOG=debug` AVANT toute
  autre hypothese.
- **`opencode` exige une section `models` explicite** (2026-09-02) : pour un
  fournisseur OpenAI-compatible declare a la main, `opencode` ne decouvre
  RIEN via `/v1/models` — il ne connait que le catalogue models.dev et ce
  qui est declare dans `provider.<nom>.models`. Sans cette section,
  `opencode models` ne liste rien pour le fournisseur et
  `opencode run --model atelier/atelier-workshop-agent` se bloque
  indefiniment, sans message, sans code d'erreur. Ajoutee dans la config
  generee par `inject_net_proxy_config` (`crates/image-builder/src/main.rs`).
  A cote de ca, `opencode run` n'ecrit strictement rien tant que son stdin
  n'est pas ferme : diagnostiquer avec `< /dev/null`, sinon meme les
  messages d'erreur restent invisibles.
- **Une variable d'environnement DEFINIE mais VIDE n'est pas "absente"**
  (2026-09-02) : `deploy/dev/local-stack.sh` n'ecrit le bloc LiteLLM que si
  `DEEPSEEK_API_KEY` ou `ANTHROPIC_API_KEY` est exporte au moment ou on le
  lance ; sinon il genere `ATELIER_LLM_PROXY_AUTH_TOKEN=""`. Le motif
  `std::env::var(...).ok()` acceptait cette chaine vide, et le guest
  recevait un jeton d'authentification vide dans `/etc/environment` — pire
  qu'une absence franche de configuration, puisque l'agent partait quand
  meme et echouait sans rien dire. Corrige par un
  `.filter(|v| !v.trim().is_empty())` dans `crates/controller` et
  `crates/image-builder` : une valeur vide vaut desormais "non configure",
  comme l'absence de la variable. En dev, `ATELIER_LLM_PROXY_ADDR` doit
  valoir l'adresse vue par le controller (port-forward `127.0.0.1:14000`)
  et `ATELIER_LLM_PROXY_POD_ADDR` celle vue par les pods — meme
  dedoublement que `OPENBAO_ADDR`/`ATELIER_OPENBAO_POD_ADDR`.
- **La ConfigMap LiteLLM ne se met pas a jour toute seule** (2026-09-02) :
  editer `deploy/dev/llm-proxy/config.yaml` ne change rien tant que la
  ConfigMap n'est pas recreee ET le Deployment redemarre. L'alias
  `atelier-workshop-agent` ajoute pour `opencode` etait donc absent du
  proxy en fonctionnement : la requete retombait sur le wildcard `"*"`
  (route vers `anthropic/deepseek-chat`, l'endpoint Anthropic de DeepSeek),
  qui rejette les outils d'`opencode` en `400` — `tools[0]: unknown variant
  'custom'`. `opencode` retente alors en silence, ce qui se voit seulement
  comme un blocage. Les `400` n'apparaissent que dans les logs du pod
  LiteLLM : c'est la premiere chose a regarder quand un agent "ne repond
  pas" sans erreur.
- **Ce qu'un redemarrage du noeud kind detruit dans OpenBao** (2026-09-02) :
  le pod OpenBao de dev perd ses METHODES D'AUTH au redemarrage (les
  secrets KV, eux, survivent — ce qui rend le diagnostic trompeur : le
  secret cherche est bien la, mais plus personne ne peut s'authentifier
  pour le lire). Symptome cote api-server : `cle SSH indisponible pour ce
  Workshop`, avec un `login OpenBao refuse` en `WARN` comme seule trace.
  Remise en etat : rejouer le bloc `bao auth enable kubernetes` +
  `bao write auth/kubernetes/config` de `deploy/dev/local-stack.sh`, puis
  REDEMARRER le controller — c'est lui qui cree le role et la policy
  `atelier-api-server`, et seulement a son demarrage. Cause voisine et
  meme symptome exact : le jeton de ServiceAccount
  (`deploy/dev/local-stack/api-server-sa-token`) a une duree de vie de 24 h
  et expire donc chaque jour ; `kubectl create token atelier-api-server
  --duration=24h` le regenere. Verifier lequel des deux est en cause en
  rejouant le login a la main
  (`POST /v1/auth/kubernetes/login`) : le message y est explicite
  (`token is expired`), contrairement au `login OpenBao refuse` cote
  api-server.
- **Une chaine de bugs silencieux entre le PM et son agent** (2026-09-02) :
  faire tourner le graphe complet du PM sur un depot greenfield, avec un
  devcontainer ORDINAIRE (`mcr.microsoft.com/devcontainers/javascript-node:20`,
  aucune surcouche atelier), a fait tomber six defauts d'affilee. Aucun ne
  produisait de message utile ; tous se presentaient sous le meme deguisement,
  `connexion SSH echouee: Disconnected` ou `Unexpected server error`. Ils sont
  listes ici dans l'ordre ou il a fallu les demeler, parce que chacun masquait
  le suivant :
  1. **`export LD_LIBRARY_PATH` global dans le script de demarrage de `sshd`.**
     Nos bibliotheques viennent de bookworm, l'image cible est plus recente :
     `mkdir`, `chmod`, `chown`, `seq` mouraient tous en `stack smashing
     detected` / `GLIBC_2.38 not found`. Le script s'arretait a sa premiere
     ligne utile. La variable ne doit etre posee que sur `sshd`/`ssh-keygen`,
     via `env`.
  2. **`sshd` se re-execute par connexion.** Le re-exec repart du chemin brut,
     donc avec l'editeur de liens de l'image cible et nos bibliotheques
     bookworm : mort immediate, connexion coupee avant l'echange de versions
     (`kex_exchange_identification`), alors que `sshd` annonce tranquillement
     `Server listening on port 2222`. `-r` desactive ce re-exec.
  3. **Le port SSH.** `crate::exec` (api-server) visait `22` par defaut, herite
     de l'epoque ou seul le devcontainer de demo fournissait SSH, via le
     service systeme. Notre `sshd` injecte ecoute sur `2222`. Sur toute image
     ordinaire, l'exec frappait donc a une porte que personne n'ecoutait.
  4. **`UsePAM no` prive le guest de `/etc/environment`.** C'est `pam_env` qui
     lit ce fichier, et une session SSH non interactive n'ouvre ni
     `/etc/profile` ni `~/.bashrc`. L'agent demarrait sans
     `OPENCODE_CONFIG_CONTENT` ni jeton LLM : `opencode` ne connaissait aucun
     fournisseur et mourait sur `Unexpected server error`. Corrige par
     `~/.ssh/environment` + `PermitUserEnvironment yes`.
  5. **`git clone` ignorait le proxy.** libcurl, sous `git`, ne lit
     deliberement que `http_proxy` en MINUSCULES pour les URL `http://`
     (protection historique contre l'en-tete CGI `Proxy:`). Avec les seules
     majuscules, le clone tentait de resoudre `git.atelier.internal` lui-meme
     et echouait ; le workspace etait livre vide, sans code ni depot git, et
     l'agent n'avait rien sur quoi travailler.
  6. **Le Workshop s'annoncait `Running` trop tot.** La sonde ne verifiait que
     `ttyd`, qui ecoute avant `sshd` : tout `exec_in_workshop` lance dans la
     foulee echouait. Elle verifie desormais les deux portes d'entree.
  La lecon commune : chacun de ces defauts etait invisible en test unitaire et
  invisible avec le devcontainer de demo — lequel apportait son propre `sshd`
  systeme, avec PAM, sur le port 22, et masquait donc a lui seul les points
  1 a 4. Un composant qu'on rend generique doit etre exerce sur une image
  ORDINAIRE, pas sur celle qui a servi a l'ecrire.
- **Le snapshot d'un Workshop survit au Workshop** (2026-09-02) : les
  snapshots persistants sont indexes par NOM
  (`/cache/snapshots/default_<nom>/`). Recreer un Workshop portant un nom
  deja utilise ne reconstruit rien : `vm-supervisor` restaure l'ancien
  snapshot, donc l'ancien rootfs — sans les binaires fraichement injectes, et
  avec l'ancienne cle SSH. Symptome : un Workshop qui semble ignorer une
  image tout juste reconstruite. En debug, prendre un nom neuf (ou supprimer
  le repertoire de snapshot) plutot que de rejouer le meme.
- **Deux comptes pour un meme uid, et c'est tres bien ainsi** (2026-09-02) :
  beaucoup d'images de devcontainer utilisent deja l'uid 1000 (`node` sur les
  images Node, `ubuntu` ailleurs). `ensure_vscode_user` cree malgre tout
  `vscode` avec ce meme uid : `/etc/passwd` porte alors deux noms pour un
  seul uid, et `whoami` affiche le nom de l'image. Ce n'est pas un bug —
  memes droits, meme acces aux fichiers du workspace, et `sshd` resout bien
  `/home/vscode` — mais il faut le savoir avant de s'alarmer en voyant
  `whoami` repondre `node` dans un Workshop.
- **Le keep-alive d'une session d'agent survit a celui de la destination**
  (2026-09-02) : `forward_rewriting` (`crates/net-proxy/src/proxy.rs`)
  gardait UNE connexion vers la destination pour toute la duree de la
  connexion cliente. Or celle d'un agent vit des dizaines de minutes, avec de
  longues pauses pendant que le modele reflechit, la ou `uvicorn` (sous
  LiteLLM) ferme apres quelques secondes d'inactivite. La requete suivante
  partait donc dans une socket morte : `relai du corps de la requete` cote
  net-proxy, `AI_APICallError: the socket connection was closed unexpectedly`
  cote agent. Constate sur une connexion ouverte 2 min 38 s plus tot.
  `opencode` retente, ce qui rendait le defaut presque invisible — un seul
  echec sur dix-huit echanges — mais un client sans retry, ou une requete non
  idempotente, y perdrait le tour. net-proxy rouvre desormais une connexion
  quand la destination a raccroche ENTRE deux requetes, donc avant d'avoir
  ecrit le moindre octet de la suivante : aucun rejeu, aucune requete
  dupliquee. Le test de regression a ete verifie dans les deux sens (il
  echoue si l'on neutralise la detection).
- **Une execution deleguee sans plafond global** (2026-09-02, meme run que le
  raccrochage de destination ci-dessus) : `wait_for_exec_completion`
  (`pm_engine/exec_client.py`) n'avait qu'un `timeout_s` PAR OPERATION
  reseau, et le serveur emet un `ping` a chaque sondage meme quand rien
  n'avance — il rearme donc ce delai indefiniment. Un agent reste ainsi
  suspendu 1 h 20 sur un appel au modele parti dans une socket morte, sans
  que rien, ni cote PM ni cote atelier, ne le signale : ni erreur, ni log, le
  graphe attendait juste. Ajoute `total_timeout_s` (45 min par defaut,
  `asyncio.timeout` autour de tout l'echange) : passe ce plafond, l'execution
  echoue franchement (`status: Failed`), avec la sortie deja recue
  conservee — c'est elle qui dit ou l'agent s'est arrete.
- **Le meme raccrochage, un cran plus loin : `identity-proxy`** (2026-09-02) :
  le correctif net-proxy ci-dessus n'a pas suffi. Un run reel est reste
  bloque 45 minutes malgre lui (garde-fou cote PM declenche) : `net-proxy`
  chaine tout le trafic LLM par `identity-proxy` (injection de credentials),
  qui maintient LUI AUSSI une connexion persistante vers la vraie
  destination — meme defaut, une couche plus bas, dans une crate distincte
  que mon premier correctif ne touchait pas. Pire encore : `forward()`
  (`crates/identity-proxy/src/proxy.rs`) rendait la main SANS repondre au
  client sur une ecriture en echec (`Broken pipe`) — le client (net-proxy)
  restait alors a attendre une reponse qui n'arriverait jamais, PENDANT que
  net-proxy lui-meme attendait une requete suivante qui n'arriverait pas
  davantage : chacun des deux bouts attendait l'autre, une impasse parfaite,
  invisible de l'exterieur (aucune erreur, aucun log au-dela d'un `WARN`
  isole).
  Correctif different de celui de net-proxy, plus simple ici puisque
  `forward()` est une boucle synchrone requete/reponse (pas de tache de
  copie separee) : `try_read` avec un tampon d'1 octet, standard pour
  detecter un FIN sans bloquer, AVANT d'ecrire le moindre octet de la
  requete courante — jamais apres un echec d'ecriture partiel, qui rendrait
  un rejeu incorrect (le corps deja consomme depuis le client ne peut pas
  etre relu).
  Lecon a retenir : une connexion egress passe par plusieurs hops
  (`net-proxy` -> `identity-proxy` -> destination), et CHACUN peut
  independamment garder une connexion persistante trop longtemps vivante.
  Corriger le premier hop sans verifier les suivants laisse le meme defaut
  intact, juste plus loin dans la chaine.
