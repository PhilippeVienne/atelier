# Devcontainer de validation : chemin de production complet pour `mcp-gateway`

Devcontainer minimal (systemd + `curl`), dedie a fermer le dernier "reste a
faire" de `mcp-gateway` (voir `docs/PROGRESS.md`) : verifier depuis
l'**interieur d'une vraie microVM agent** que le chemin complet
`HTTP_PROXY` (injecte par `image-builder`) -> `net-proxy` (pare-feu TAP de
`vm-supervisor` + alias `mcp-gateway`) -> `mcp-gateway` fonctionne, sans
aucun raccourci (pas de vsock, pas de partage de netns/process avec
l'hote — contrairement a `demo/vsock-probe/`, ce guest ne connait que ce
qu'un devcontainer normal connaitrait).

`mcp_probe.sh` : `curl` pur (lit `HTTP_PROXY` depuis l'environnement du
process, fourni par systemd via `EnvironmentFile=/etc/environment`) fait un
vrai handshake MCP (`initialize` -> `notifications/initialized` ->
`tools/call request_egress`) contre `http://mcp-gateway/mcp`. Resultat
journalise dans `/tmp/atelier-mcp-agent-probe.log` (`sync` explicite apres
chaque ligne, meme raison que le `fsync` de `demo/vsock-probe/` — inspection
post-mortem via `debugfs`).

## Verifie reellement (sans mock)

- **`/etc/environment`** injecte avec exactement le contenu produit par
  `inject_net_proxy_config` (`crates/image-builder/src/main.rs`, deja
  verifie octet pour octet contre le vrai pipeline plus tot dans cette
  session — reproduit ici a la main pour eviter le blocage d'auth git sur
  un depot prive — devenu sans objet depuis que `ministack-workshop` vit
  dans un depot public dedie, github.com/PhilippeVienne/atelier-workspace).
- **Vrai `atelier-vm-supervisor`** : boot avec le **vrai** TAP +
  `restrict_to_net_proxy` (le pare-feu de production, pas une version
  allegee) — la seule sortie du guest est `net-proxy`.
- **Vrai `atelier-net-proxy`** avec l'alias `mcp-gateway` configure
  (`ATELIER_MCP_GATEWAY_ADDR`).
- **Vrai `atelier-mcp-gateway`**, tool `egress` active.
- Resultat, lu dans le `rootfs.ext4` du guest apres extinction : les trois
  etapes du handshake reussissent, et **le call `tools/call` a un effet
  reel et verifiable ailleurs** — `net-proxy` journalise independamment
  `allowlist elargie a chaud (request_egress) host="example.com" count=1`,
  preuve croisee (pas juste "le guest dit que ca a marche", mais "un
  systeme tiers confirme que ca a marche").

Ferme le dernier point ouvert de la section `mcp-gateway` de
`docs/PROGRESS.md` : jusqu'ici, aucun des tests HTTP de `mcp-gateway`
n'avait ete fait depuis l'interieur d'un guest reellement boote par
`vm-supervisor` (toujours via un client `curl` sur l'hote, partageant le
netns Docker).
