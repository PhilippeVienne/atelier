# Modele de securite et isolation reseau de la microVM

> Vue d'ensemble : voir [`../ARCHITECTURE.md`](../ARCHITECTURE.md).

## Modele de securite

- La seule surface d'attaque exposee par la microVM vers l'exterieur passe
  par `net-proxy` : **c'est le seul point d'entree reseau que la VM peut
  joindre**, jamais `identity-proxy` ni `mcp-gateway` directement (voir
  ci-dessous). Aucun acces direct de la VM au reste du cluster.
- Isolation memoire/noyau assuree par Firecracker (jailer, seccomp,
  cgroups) plutot que par la seule isolation de conteneur d'un Pod.
- Authentification externe : JWT emis par le fournisseur OIDC configure
  (`ATELIER_OIDC_ISSUER_URL`, Keycloak en dev), seule source de verite
  identite. Pas de gestion d'utilisateurs locale dans Atelier lui-meme.

## Isolation reseau de la microVM : mecanisme concret

Il ne suffit pas d'affirmer "aucun acces direct au reste du cluster" au
niveau applicatif (allowlist de `net-proxy`) : sans application au niveau
paquet, rien n'empeche la VM d'ouvrir une connexion TCP brute vers l'IP du
pod (`eth0`), l'API server Kubernetes, un autre pod, ou un service de
metadata cloud, en contournant `net-proxy` entierement. Cible retenue,
deux composants distincts pour deux transports distincts, plus une
decision de design qui reduit `net-proxy` a l'unique destination que la VM
connaisse :

1. **`mcp-gateway` : isolation structurelle, plus un alias HTTP optionnel
   via `net-proxy`.** Cible a terme : expose nativement via `vsock`
   (`AF_VSOCK`, adressage CID/port), pas sur le reseau IP de la VM — rien
   de ce qui transite par le tap reseau ne peut l'atteindre par ce chemin,
   et rien d'externe au couple hote/VM ne peut atteindre ce vsock.
   **Limite assumee (comme la limite CONNECT/MITM d'`identity-proxy`
   ci-dessous) : ce transport `vsock` n'est pas construit** — voir
   `docs/PROGRESS.md`, section "`mcp-gateway` : premier serveur MCP reel".
   `net-proxy` expose en attendant l'alias HTTP `mcp-gateway`
   (`ATELIER_MCP_GATEWAY_ADDR`, `crates/net-proxy/src/internal.rs`, branche
   cote `crates/controller/src/reconcile.rs`) — c'est aujourd'hui le
   **seul** chemin fonctionnel vers `mcp-gateway`, pas une simple
   alternative pour les clients qui prefèrent HTTP/SSE. La garantie de
   securite recherchee (mcp-gateway jamais joint directement par la VM)
   tient malgre tout : ce chemin passe par le meme port que `net-proxy`,
   pas par un port supplementaire a autoriser separement, exactement
   comme pour `identity-proxy` (point 2 ci-dessous).
2. **`identity-proxy` : jamais joint directement par la VM, uniquement via
   `net-proxy`.** Decision de design (revisee) : la premiere version
   configurait `identity-proxy` comme un second `HTTP_PROXY` que la VM
   pouvait joindre directement pour certains hotes, lui-meme chainant vers
   `net-proxy`. Rejetee : elle donnait a la VM une deuxieme destination
   reseau directe a autoriser au pare-feu, agrandissant la surface sans
   necessite, et cassait la garantie "net-proxy tranche toujours
   l'allowlist en premier" (identity-proxy aurait pu recevoir une requete
   qu'aucune allowlist n'avait encore validee). Design retenu : la VM ne
   configure qu'**un seul** `HTTP_PROXY`, `net-proxy` — c'est lui qui,
   apres avoir juge une destination autorisee, chaine la requete vers
   `identity-proxy` (`ATELIER_IDENTITY_PROXY_ADDR` cote net-proxy) si ce
   dernier est configure ; `identity-proxy` decide alors, selon ses propres
   regles (`Workshop`-scoped, pas connues de net-proxy), d'injecter un
   credential ou de relayer tel quel, puis se connecte **directement** a
   la destination finale — jamais en repassant par `net-proxy`, ce qui
   boucleraient indefiniment puisque net-proxy chaine deja tout l'egress
   autorise vers identity-proxy. Limite connue : un `CONNECT` (HTTPS) est
   un tunnel TCP opaque, le contenu est chiffre bout-a-bout — identity-proxy
   ne peut donc pas y injecter d'en-tete sans devenir un MITM TLS actif, ce
   qui n'est pas fait ; l'injection ne fonctionne que pour les requetes
   HTTP en clair relayees en forme absolue.
3. **Application au niveau paquet : pare-feu sur le device TAP de la
   VM.** La VM recoit une seule interface reseau (le TAP link-local `/30`
   deja implemente dans `crates/firecracker/src/network.rs`, ex. hote
   `169.254.0.1`, guest `169.254.0.2`) et une route par defaut vers l'IP
   hote. Comme tous les conteneurs d'un meme pod partagent une seule et
   meme network namespace, `net-proxy` lie sur `0.0.0.0` est deja joignable
   depuis la VM a `169.254.0.1:<son port>` sans aucun NAT ni forwarding —
   c'est de la livraison locale dans la meme netns. **Il ne faut donc pas
   reutiliser tel quel le `MASQUERADE` inconditionnel de
   `setup_link_local_tap`** (legitime pour la microVM "builder" de
   `image-builder`, qui a explicitement besoin de sortir vers un registre
   OCI/depot git quelconque, elle-meme isolee autrement — voir
   "Reseau kind ↔ registre" dans `PROGRESS.md`) : pour la VM de l'agent, la
   sortie doit rester fermee par defaut.
   - Ne pas poser de regle `MASQUERADE`/`FORWARD -j ACCEPT` vers `eth0`
     pour ce TAP : sans route de sortie, un paquet vers une destination
     autre que `169.254.0.1` est simplement injoignable.
   - Poser explicitement, en defense en profondeur (le sysctl
     `net.ipv4.ip_forward` est global au netns du pod — une autre
     microVM du meme pod qui l'active ne doit pas rouvrir cette voie par
     accident) :
     ```
     iptables -N atelier-vm-<id>
     iptables -A atelier-vm-<id> -p tcp -d 169.254.0.1 --dport <port net-proxy> -j ACCEPT
     iptables -A atelier-vm-<id> -p udp -d 169.254.0.1 --dport 53              -j ACCEPT
     iptables -A atelier-vm-<id> -p tcp -d 169.254.0.1 --dport 53              -j ACCEPT
     iptables -A atelier-vm-<id> -j DROP
     iptables -A INPUT   -i <tap> -j atelier-vm-<id>
     iptables -A FORWARD -i <tap> -j DROP
     ```
     (chaine dediee par VM, nettoyee au `teardown()`, symetrique a ce que
     fait deja `NetworkSetup::teardown` pour la regle NAT de la VM
     "builder"). Une seule ligne `ACCEPT` pour un port applicatif — celui
     de `net-proxy` — puisque `identity-proxy` et l'alias `mcp-gateway` ne
     sont plus atteints qu'a travers lui. Le port de controle de
     `net-proxy` (`ATELIER_NET_PROXY_CONTROL_ADDR`, le websocket
     `/portforward` destine a `api-server`) reste hors de cette liste : la
     VM ne doit jamais pouvoir l'atteindre, seul `api-server` le peut,
     depuis l'exterieur du pod.
   - Consequence assumee du DNS relaye par `net-proxy` : pas de resolveur
     DNS ouvert vers l'exterieur pour la VM en dehors de `net-proxy`
     lui-meme, qui applique la meme allowlist que l'egress HTTP(S) (un
     nom refuse recoit `REFUSED` sans jamais atteindre l'upstream —
     `crates/net-proxy/src/dns.rs`) et filtre en plus les requetes a
     questions multiples et les types `ANY`/`AXFR`/`IXFR`, memes pour un
     nom autorise.

**Fait** : `vm-supervisor` cree desormais ce TAP (`crates/vm-supervisor/src/main.rs`,
`setup_link_local_tap` + `NetworkSetup::restrict_to_net_proxy`) et boote la
VM avec. Le guest n'a pas d'init personnalise (contrairement a la microVM
"builder") : l'adresse/route par defaut sont posees par le noyau lui-meme
via le parametre de boot `ip=<guest>::<host>:<masque>::eth0:off`
(autoconfiguration IP standard Linux, ne necessite aucune cooperation de
l'init du guest) — verifie reellement (`IP-Config: Complete: device=eth0,
ipaddr=169.254.0.2, mask=255.255.255.252, gw=169.254.0.1`), de meme que les
regles iptables (`iptables -S atelier-vm-<tap>` confirme les `ACCEPT`
port net-proxy/DNS puis le `DROP` final).

**Ferme : passerelle transparente, zero configuration interne au guest.**
L'idee initialement envisagee ici (injecter `HTTP_PROXY`/`HTTPS_PROXY`/un
resolveur DNS dans l'image construite par `image-builder`) a ete
abandonnee : un vrai bug trouve en testant `ministack-workshop` en a
montre la limite avant meme d'etre construite — l'etape `RUN apt-get`
d'un `Dockerfile`, executee par `envbuilder` dans la microVM "builder",
n'herite jamais de `HTTP_PROXY` (contrairement au clone git et au push
registre, qui eux passent bien par cette variable), ce qui aurait rendu
la meme approche fragile une fois appliquee a la VM de l'agent, pour
n'importe quel devcontainer arbitraire fourni par l'utilisateur d'un
Workshop — jamais garanti de respecter ces variables.

Solution retenue a la place : `net-proxy` devient une **passerelle
transparente** — le guest n'a besoin d'absolument aucune configuration
reseau particuliere (ni `HTTP_PROXY`, ni resolveur DNS specifique), il n'a
meme pas besoin de savoir que `net-proxy` existe.

- `NetworkSetup::enable_transparent_gateway` (`crates/firecracker/src/network.rs`)
  pose, en plus de la chaine `filter` existante (inchangee), une chaine
  `nat` dediee sur le TAP :
  ```
  iptables -t nat -N atelier-vm-nat-<tap>
  iptables -t nat -A atelier-vm-nat-<tap> -p tcp --dport 80  -j REDIRECT --to-port <port HTTP transparent>
  iptables -t nat -A atelier-vm-nat-<tap> -p tcp --dport 443 -j REDIRECT --to-port <port TLS transparent>
  iptables -t nat -A atelier-vm-nat-<tap> -p udp --dport 53  -j REDIRECT --to-port 53
  iptables -t nat -A atelier-vm-nat-<tap> -p tcp --dport 53  -j REDIRECT --to-port 53
  iptables -t nat -A PREROUTING -i <tap> -j atelier-vm-nat-<tap>
  ```
  `REDIRECT` reecrit l'IP de destination vers celle de l'interface
  d'entree **avant** la decision de routage : le paquet devient une
  livraison locale (chemin `INPUT`), jamais un transit `FORWARD` — `sysctl
  net.ipv4.ip_forward` reste a 0 comme avant, `FORWARD -j DROP` reste
  inchange pour tout ce qui n'est pas explicitement 80/443/53. Le
  raisonnement de la section precedente ("ne pas activer `ip_forward`,
  risque partage entre VMs du meme netns") reste donc valide et n'est
  **pas** contourne par ce mecanisme.
- `net-proxy` ecoute deux ports supplementaires
  (`ATELIER_NET_PROXY_TRANSPARENT_HTTP_ADDR`/`_TLS_ADDR`, defaut
  `0.0.0.0:3180`/`:3181`) : le port HTTP transparent reutilise tel quel
  `handle_connection` (une requete origin-form + `Host:` y arrive deja
  dans le format attendu) ; le port TLS transparent lit le SNI du
  `ClientHello` **en clair, sans jamais dechiffrer** (`crates/net-proxy/src/tls_sni.rs`,
  meme principe que `ssl_preread` nginx/`req.ssl_sni` HAProxy), verifie
  l'allowlist, puis relaie les octets tels quels — aucun certificat, aucune
  confiance CA a gerer, la validation TLS de bout en bout reste intacte
  cote guest.
- Le port 53 (DNS) est redirige selon le meme principe **quel que soit**
  le serveur DNS que le guest croit utiliser (`REDIRECT` matche sur le
  port de destination, pas sur l'IP) — ferme le trou DNS mentionne
  ci-dessus sans configuration guest non plus.
- La VM de l'agent avait deja une route par defaut vers `net-proxy` sans
  aucune configuration interne (`ip=` kernel, voir plus haut) : rien a
  changer cote `vm-supervisor` au-dela d'appeler
  `enable_transparent_gateway` a la place de `restrict_to_net_proxy`. La
  VM "builder" (`atelier-builder-vm-init`, notre propre bootstrap, jamais
  le contenu du Workshop), elle, n'avait pas de route par defaut — une
  ligne `ip route add default via <host_ip>` a ete ajoutee, seule
  "configuration interne" necessaire, et seulement a un composant de
  plateforme, jamais au devcontainer de l'utilisateur.
- **Verifie reellement** contre le Workshop de demo `ministack-workshop`
  (Dockerfile **non modifie**) : `apt-get install systemd` (via
  `archive.ubuntu.com`/`security.ubuntu.com`, HTTP transparent),
  `deb.nodesource.com` (HTTPS transparent, feature Node.js), et
  l'integralite du build (docker-in-docker, claude-code, code-server)
  aboutissent avec succes, `imageDigest` publie et Workshop `Running`
  (`crates/firecracker/tests/network.rs`, nouveau test
  `enables_transparent_redirect_without_touching_forward`, verifie en
  plus le contenu exact des regles via `iptables -t nat -S` sous
  `CAP_NET_ADMIN` reel).
