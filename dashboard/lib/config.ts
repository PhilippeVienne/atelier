// Configuration lue depuis l'environnement : aucune valeur par defaut pour
// les secrets/URLs de production, seulement pour le confort de dev local
// (memes ports que deploy/dev/kanidm/README.md et crates/api-server).

export const API_SERVER_URL =
  process.env.ATELIER_API_SERVER_URL ?? "http://localhost:8080";

export const KANIDM_URL =
  process.env.ATELIER_KANIDM_URL ?? "https://localhost:8443";

export const OAUTH2_CLIENT_ID =
  process.env.ATELIER_OAUTH2_CLIENT_ID ?? "atelier";

export const OAUTH2_SCOPE = "openid";

// Doit correspondre a une des redirect_uri enregistrees cote Kanidm pour ce
// client (`kanidm system oauth2 add-redirect-url`, voir
// deploy/dev/kanidm/README.md) — Kanidm valide une correspondance exacte
// (`oauth2_strict_redirect_uri`).
export function oauth2RedirectUri(origin: string): string {
  return process.env.ATELIER_OAUTH2_REDIRECT_URI ?? `${origin}/api/auth/callback`;
}
