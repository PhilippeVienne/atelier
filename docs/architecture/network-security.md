# Modele de securite et isolation reseau de la microVM

> Vue d'ensemble : voir [`../ARCHITECTURE.md`](../ARCHITECTURE.md).

## Modele de securite

- La seule surface d'attaque exposee par la microVM vers l'exterieur passe
  par `net-proxy` : **c'est le seul point d'entree reseau que la VM peut
  joindre**, jamais `identity-proxy` ni `mcp-gateway` directement (voir
  ci-dessous). Aucun acces direct de la VM au reste du cluster.
- Isolation memoire/noyau assuree par Firecracker (jailer, seccomp,
  cgroups) plutot que par la seule isolation de conteneur d'un Pod.
- Authentification externe : JWT emis par Kanidm, seule source de verite
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
   via `net-proxy`.** Expose nativement via `vsock` (`AF_VSOCK`, adressage
   CID/port), pas sur le reseau IP de la VM — rien de ce qui transite par
   le tap reseau ne peut l'atteindre par ce chemin, et rien d'externe au
   couple hote/VM ne peut atteindre ce vsock. `net-proxy` expose en plus
   l'alias HTTP `mcp-gateway` (`ATELIER_MCP_GATEWAY_ADDR`,
   `crates/net-proxy/src/internal.rs`) pour les clients MCP qui prefèrent
   un transport HTTP/SSE standard au vsock — ce deuxieme chemin passe par
   le meme port que `net-proxy`, pas par un port supplementaire a
   autoriser separement.
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

**A faire** : donner effectivement un TAP reseau a la VM de l'agent
(aujourd'hui absente — `vm-supervisor` boote sans interface reseau, cf.
commentaire de tete de `crates/firecracker/src/network.rs`) et poser les
regles ci-dessus. Le mecanisme est specifie, pas encore cable.
