/**
 * Allowlist egress "environnement de dev" : couvre les hotes reellement
 * necessaires pour construire un devcontainer standard (base image, apt,
 * features ghcr.io, Docker Hub, npm/pip) sans avoir a decouvrir chaque
 * domaine un par un via les logs `net-proxy` a chaque nouveau Workshop.
 * A dessein plus large qu'une allowlist de production scopee a un seul
 * depot — reserve a l'usage dev (cf. dashboard/AGENTS.md, decision prise
 * en session apres plusieurs echecs de build par manque d'un domaine).
 */
export const DEV_EGRESS_ALLOWLIST = [
  "github.com",
  "*.githubusercontent.com",
  "api.github.com",
  "codeload.github.com",
  "ghcr.io",
  "pkg-containers.githubusercontent.com",
  "mcr.microsoft.com",
  "*.data.mcr.microsoft.com",
  "archive.ubuntu.com",
  "security.ubuntu.com",
  "ports.ubuntu.com",
  "deb.debian.org",
  "download.docker.com",
  "get.docker.com",
  "registry-1.docker.io",
  "auth.docker.io",
  "production.cloudflare.docker.com",
  "registry.npmjs.org",
  "pypi.org",
  "files.pythonhosted.org",
  // NodeSource (feature devcontainers/node et claude-code, qui l'installe
  // elle-meme comme dependance) : verifie en pratique que sans ce domaine,
  // le fallback vers le paquet `nodejs` d'Ubuntu ne suffit pas (l'un des
  // scripts de feature echoue quand meme faute de `npm`/version attendue).
  "deb.nodesource.com",
  // Feature devcontainers/docker-in-docker : depot Docker Engine officiel
  // Microsoft (paquets .deb signes par leur cle GPG).
  "packages.microsoft.com",
];
