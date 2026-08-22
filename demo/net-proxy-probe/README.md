# Devcontainer de validation : usage reel du proxy injecte par `image-builder`

Devcontainer minimal (systemd + `curl` + `netcat-openbsd`), dedie a la
validation comportementale de `inject_net_proxy_config`
(`crates/image-builder/src/main.rs`) : est-ce qu'un devcontainer booté
**exactement comme le fait `vm-supervisor`** (boot_args par defaut, sans
`init=` personnalise, TAP + iptables `restrict_to_net_proxy`) utilise
reellement `HTTP_PROXY`/`HTTPS_PROXY` lus dans `/etc/environment` pour
sortir, plutot que de se faire silencieusement bloquer par les regles
iptables qui ne laissent passer que `net-proxy` ?

Contenu :

- `probe.sh` : au demarrage, fait un vrai `curl -sf https://example.com/`
  (donc un vrai `CONNECT` TLS si le proxy est configure), puis sert le
  resultat (`OK`/`FAIL`) en boucle sur le port `9999` via `nc`.
- `atelier-proxy-probe.service` : unite systemd, `EnvironmentFile=-/etc/environment`
  (optionnel, tiret en tete) — `curl` recoit `HTTP_PROXY`/`HTTPS_PROXY`
  automatiquement de systemd si `image-builder` les a bien injectes dans ce
  fichier, sans rien avoir a sourcer manuellement.

## Verifie reellement (sans mock)

Protocole en A/B, meme rootfs de base (export Docker reel de cette image,
`docker export`), deux variantes :

- **`with-proxy`** : `/etc/environment` + `/etc/resolv.conf` ecrits avec
  exactement le contenu produit par `inject_net_proxy_config` (verifie au
  prealable octet pour octet contre le vrai pipeline `image-builder`, voir
  `docs/PROGRESS.md`).
- **`without-proxy`** : rootfs identique, sans ces deux fichiers (baseline).

Les deux `rootfs.ext4` sont boots avec le **vrai binaire** `atelier-vm-supervisor`
(pas un test isole ni une reimplementation — le meme process qui tourne en
production dans le pod parent), dans un conteneur Docker `--privileged
--network host` (meme contournement documente ailleurs dans ce projet pour
obtenir `CAP_NET_ADMIN` + une vraie sortie Internet), a cote d'un vrai
`atelier-net-proxy` (allowlist `*`) partageant le meme netns — reproduit le
tete-a-tete `net-proxy`+`vm-supervisor` d'un pod reel.

Resultat observe :

- **`with-proxy`** : boot confirme (`microVM running`), le service systemd
  demarre (console guest : `Started atelier-proxy-probe.service`), et **le
  vrai process `net-proxy`** journalise un acces reel :
  `egress autorise peer=169.254.0.2:xxxxx host="example.com" port=443
  method=CONNECT allowed=true` — preuve que le guest a bien lu
  `HTTPS_PROXY` depuis `/etc/environment` et l'a utilise pour un vrai
  tunnel TLS via `net-proxy`.
- **`without-proxy`** : boot confirme, meme service demarre, mais **aucune**
  entree n'apparait dans les logs de `net-proxy` pendant toute la fenetre de
  ce run — cohérent avec une tentative de connexion directe (`curl` sans
  proxy configure) silencieusement rejetee par les regles iptables de
  `restrict_to_net_proxy`, jamais un `CONNECT` ne parvient a `net-proxy`.

Cette paire de resultats (A avec logs, B sans) est la preuve que
l'injection faite par `image-builder` a un effet reel et necessaire : sans
elle, un devcontainer construit aujourd'hui n'a tout simplement aucun moyen
de sortir, meme si `net-proxy` autorise tout.

**Limite non couverte par ce test** : sonder directement le port `9999`
depuis l'hote (pour lire `OK`/`FAIL` sans dependre des logs `net-proxy`) n'a
pas fonctionne de maniere fiable dans cet environnement (probablement un
souci d'invocation de `nc` cote guest/hote, pas explore plus loin vu que les
logs `net-proxy` donnaient deja une preuve suffisante) — a reprendre si une
verification independante des logs est necessaire.

## Reproduire

```sh
docker build -t atelier-net-proxy-probe:dev -f .devcontainer/Dockerfile .devcontainer
CID=$(docker create atelier-net-proxy-probe:dev)
docker export "$CID" -o /tmp/rootfs.tar && docker rm "$CID"
mkdir -p /tmp/with-proxy /tmp/without-proxy
tar xf /tmp/rootfs.tar -C /tmp/with-proxy
tar xf /tmp/rootfs.tar -C /tmp/without-proxy

cat > /tmp/with-proxy/etc/environment <<'EOF'
HTTP_PROXY=http://169.254.0.1:3128
HTTPS_PROXY=http://169.254.0.1:3128
http_proxy=http://169.254.0.1:3128
https_proxy=http://169.254.0.1:3128
NO_PROXY=169.254.0.1
no_proxy=169.254.0.1
EOF
echo "nameserver 169.254.0.1" > /tmp/with-proxy/etc/resolv.conf

for variant in with-proxy without-proxy; do
  SIZE_MB=$(( $(du -sk /tmp/$variant | cut -f1) / 1024 + 256 ))
  truncate -s "${SIZE_MB}M" /tmp/$variant.ext4
  mke2fs -F -t ext4 -d /tmp/$variant /tmp/$variant.ext4
done

# Puis, dans un conteneur --privileged --network host --device=/dev/kvm
# --device=/dev/net/tun (avec target/release/atelier-{net-proxy,vm-supervisor}
# et deploy/dev/firecracker/assets montes) : lancer atelier-net-proxy
# (ATELIER_EGRESS_ALLOWLIST='*'), puis atelier-vm-supervisor avec
# ATELIER_VM_ROOTFS_PATH pointant tour a tour sur chaque variante, et
# observer les logs de net-proxy.
```
