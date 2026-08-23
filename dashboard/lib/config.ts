// Configuration lue depuis l'environnement : aucune valeur par defaut pour
// les secrets/URLs de production, seulement pour le confort de dev local
// (memes ports que deploy/dev/keycloak/README.md et crates/api-server).

export const API_SERVER_URL =
  process.env.ATELIER_API_SERVER_URL ?? "http://localhost:8080";

// Base OIDC generique — pour Keycloak c'est l'URL du realm (ex:
// `http://127.0.0.1:8080/realms/atelier`), PAS la racine du serveur : les
// endpoints standards (`/protocol/openid-connect/{auth,token,certs}`,
// `/.well-known/openid-configuration`) sont tous relatifs a cette base.
// Anciennement `ATELIER_KANIDM_URL`/`KANIDM_URL`, ou la base etait la racine
// du serveur (Kanidm n'a pas de notion de realm, ses chemins etaient
// `/ui/oauth2` et `/oauth2/token`, non conformes a la convention OIDC).
export const OIDC_ISSUER_URL =
  process.env.ATELIER_OIDC_ISSUER_URL ?? "http://127.0.0.1:8080/realms/atelier";

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
