# Devcontainer de validation : transport MCP `vsock` natif

Devcontainer minimal (systemd + `python3`), dedie a la validation du
transport `AF_VSOCK` de `mcp-gateway` (voir `crates/mcp-gateway/src/main.rs`,
`crates/firecracker/src/vm.rs::VsockConfig`) : un client MCP a l'interieur
du guest peut-il parler directement `AF_VSOCK` a `mcp-gateway`, sans passer
par `net-proxy`/TAP/iptables/allowlist ?

`vsock_probe.py` : client MCP minimal ecrit a la main (stdlib `socket` +
`json`, `socket.AF_VSOCK` disponible nativement sur Linux depuis Python
3.9) — connecte a `(cid=2, port=10000)` (2 = l'hote, cote guest), fait un
vrai handshake MCP (`initialize` -> `notifications/initialized` ->
`tools/list`), et journalise chaque etape dans `/tmp/atelier-vsock-probe.log`
(pas la console : ecrire dans `/etc/environment`-style vers `/dev/console`
s'est avere peu fiable pour ce test, voir historique de session — un fichier
relu apres coup via `debugfs`, comme pour `demo/net-proxy-probe/`, est plus
robuste).

## Verifie reellement (sans mock)

- Le **vrai** binaire `atelier-vm-supervisor` boote ce devcontainer avec un
  device vsock actif (`ATELIER_VM_VSOCK_UDS_FILENAME`), jaile par le
  **vrai** `jailer`/`firecracker`.
- Le **vrai** binaire `atelier-mcp-gateway` (meme `ServerHandler` que le
  transport HTTP, `rmcp::ServiceExt::serve` sur un `UnixStream` plutot que
  `StreamableHttpService`) lie `<uds_path>_<port>` — le socket que
  Firecracker relaie pour les connexions **initiees par le guest**
  (convention Firecracker ; `<uds_path>` lui-meme, cote "hote initie vers
  guest", est cree par Firecracker au boot).
- Les deux process partagent le meme `--chroot-base-dir` (`/srv/jailer`,
  meme convention qu'en production ou ce chemin est un volume `emptyDir`
  partage entre les conteneurs `vm-supervisor` et `mcp-gateway` du pod
  parent, voir `crates/controller/src/reconcile.rs`).
- **Piege trouve en testant** : le jailer insere le nom de l'executable
  comme composant de chemin —
  `<chroot_base_dir>/<exec_file_name>/<jail_id>/root/`, pas
  `<chroot_base_dir>/<jail_id>/root/` comme on pourrait le supposer par
  analogie avec `--chroot-base-dir` seul. Corrige dans `reconcile.rs` et ce
  script apres inspection reelle de l'arborescence produite.
- Resultat, lu directement dans le `rootfs.ext4` du guest apres extinction
  (`debugfs -R "cat /tmp/atelier-vsock-probe.log" ...`, meme technique que
  pour valider l'injection HTTP_PROXY) : les trois etapes du handshake
  reussissent, `tools/list` renvoie effectivement `request_credential` et
  `request_egress` — la meme configuration que le tool `mcp-gateway`
  expose par ailleurs en HTTP.

## Limite assumee

`python3`/la stdlib socket n'est pas ce qu'un agent MCP reel (Claude Code,
etc.) utiliserait pour parler a `mcp-gateway` — ce test valide le
**transport** (le tuyau fonctionne, jailer+firecracker+vsock+rmcp
s'assemblent correctement), pas qu'un client MCP standard sache aujourd'hui
choisir ce transport plutot que HTTP. Aucun mecanisme cote `image-builder`
n'annonce encore au guest que ce chemin existe (pas d'equivalent au
`HTTP_PROXY` injecte dans `/etc/environment` pour vsock) — a faire si ce
transport doit devenir le chemin par defaut plutot qu'une alternative bas
niveau disponible pour qui sait s'en servir.
