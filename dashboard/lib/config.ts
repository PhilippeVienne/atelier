// Configuration lue depuis l'environnement : aucune valeur par defaut pour
// les secrets/URLs de production, seulement pour le confort de dev local.
//
// Convention de dev locale : un ingress Traefik unique dans le cluster kind
// (`deploy/dev/traefik/`), routant par en-tete `Host` vers les 4 domaines
// de dev (`auth.`/`git.`/`app.`/`api.atelier.local`), tous sur le port 80
// standard (Traefik en `hostNetwork`, voir `deploy/dev/traefik/README.md`
// pour le detail — IP du node kind, `/etc/hosts` a renseigner). Remplace
// d'anciens port-forwards individuels par service, source de collisions de
// port constatees en pratique (`atelier-api-server` et le port-forward
// Keycloak ont failli finir sur le meme port 8080).
export const API_SERVER_URL =
  process.env.ATELIER_API_SERVER_URL ?? "http://api.atelier.local";

// `services/pm-engine` (Jalon M5, tache 5.5.x) : service Python distinct de
// `atelier-api-server`, pas encore derriere l'ingress Traefik partage — pas
// de domaine `.atelier.local` dedie pour l'instant, defaut de dev en
// port-forward direct (`uvicorn pm_engine.main:app --port 8100`).
export const PM_ENGINE_URL =
  process.env.ATELIER_PM_ENGINE_URL ?? "http://127.0.0.1:8100";

// Base OIDC generique — pour Keycloak c'est l'URL du realm (ex:
// `http://auth.atelier.local/realms/atelier`), PAS la racine du
// serveur : les endpoints standards
// (`/protocol/openid-connect/{auth,token,certs}`,
// `/.well-known/openid-configuration`) sont tous relatifs a cette base.
// Anciennement `ATELIER_KANIDM_URL`/`KANIDM_URL`, ou la base etait la racine
// du serveur (Kanidm n'a pas de notion de realm, ses chemins etaient
// `/ui/oauth2` et `/oauth2/token`, non conformes a la convention OIDC).
export const OIDC_ISSUER_URL =
  process.env.ATELIER_OIDC_ISSUER_URL ?? "http://auth.atelier.local/realms/atelier";

// Chemins OIDC standards, relatifs a `OIDC_ISSUER_URL`, configurables
// separement plutot que resolus via la decouverte
// (`/.well-known/openid-configuration`) : evite un appel reseau
// supplementaire (et sa mise en cache) a chaque login/refresh pour un flux
// qui n'a besoin que de deux endpoints stables. Les defauts correspondent a
// la convention Keycloak (`deploy/dev/keycloak/README.md`) ; un autre
// fournisseur OIDC generique n'aurait qu'a surcharger ces deux variables.
export const OIDC_AUTHORIZE_PATH =
  process.env.ATELIER_OIDC_AUTHORIZE_PATH ?? "/protocol/openid-connect/auth";

export const OIDC_TOKEN_PATH =
  process.env.ATELIER_OIDC_TOKEN_PATH ?? "/protocol/openid-connect/token";

// Construit une URL d'endpoint OIDC par simple concatenation de chaines
// (et non `new URL(path, OIDC_ISSUER_URL)`) : `OIDC_ISSUER_URL` contient
// deja un chemin non-vide (le realm, ex. `/realms/atelier`) et le
// constructeur `URL(path, base)` avec un `path` commencant par `/` remplace
// entierement le chemin de la base au lieu de l'y ajouter — ce qui perdrait
// le segment `/realms/atelier`.
function oidcUrl(path: string): URL {
  const base = OIDC_ISSUER_URL.replace(/\/+$/, "");
  const suffix = path.startsWith("/") ? path : `/${path}`;
  return new URL(`${base}${suffix}`);
}

export function oidcAuthorizeUrl(): URL {
  return oidcUrl(OIDC_AUTHORIZE_PATH);
}

export function oidcTokenUrl(): URL {
  return oidcUrl(OIDC_TOKEN_PATH);
}

export const OAUTH2_CLIENT_ID =
  process.env.ATELIER_OAUTH2_CLIENT_ID ?? "atelier-dashboard";

export const OAUTH2_SCOPE = "openid";

// Doit correspondre a une des redirect_uri enregistrees cote OIDC pour ce
// client (`deploy/dev/keycloak/realm-export.json`, client
// `atelier-dashboard`) — Keycloak, comme Kanidm, valide une correspondance
// stricte des redirect_uri.
export function oauth2RedirectUri(origin: string): string {
  return process.env.ATELIER_OAUTH2_REDIRECT_URI ?? `${origin}/api/auth/callback`;
}

// `request.nextUrl.origin` ne reflete PAS l'en-tete `Host` reellement recu
// par ce serveur custom (`server.ts`, `next({ dev })` sans `hostname`
// explicite) : il retombe systematiquement sur `http://localhost:3000`,
// verifie en pratique en envoyant `Host: app.atelier.local` directement au
// process Node sur le port 3000 (pas seulement via l'ingress Traefik).
// Consequence reelle, pas seulement cosmetique : le cookie PKCE
// (`atelier_oauth_pkce`) est pose sur le domaine effectivement visite par
// le navigateur (ex. `app.atelier.local`) au moment de `/api/auth/login`,
// mais si `redirect_uri` retombe sur `localhost:3000`, le callback OAuth2
// atterrit sur un domaine DIFFERENT — le navigateur n'envoie alors pas ce
// cookie, et l'echange du code echoue ("session de connexion expiree").
// Cette fonction lit l'en-tete `Host` directement plutot que de faire
// confiance a `nextUrl.origin`, pour que login/callback restent sur le
// meme domaine que celui effectivement utilise par le navigateur.
export function requestOrigin(request: Request): string {
  const host = request.headers.get("host");
  if (!host) {
    // Ne devrait pas arriver (HTTP/1.1 exige Host), mais mieux vaut
    // degrader vers nextUrl que de planter.
    return new URL(request.url).origin;
  }
  const proto = request.headers.get("x-forwarded-proto") ?? "http";
  return `${proto}://${host}`;
}
