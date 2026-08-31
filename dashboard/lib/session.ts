import "server-only";
import { cookies } from "next/headers";
import { redirect } from "next/navigation";
import { OAUTH2_CLIENT_ID, oidcTokenUrl } from "@/lib/config";

const SESSION_COOKIE = "atelier_session";
const REFRESH_COOKIE = "atelier_refresh";
const PKCE_COOKIE = "atelier_oauth_pkce";

// Marge de securite avant l'expiration reelle du JWT (`exp`) : rafraichir
// un peu en avance evite une course entre une requete en cours et
// l'expiration exacte (ex: le token expire pendant le trajet
// dashboard -> api-server d'une requete deja en vol).
const REFRESH_SKEW_MS = 30_000;

// httpOnly : le token n'est jamais lisible par le JS cote navigateur (seule
// defense necessaire ici, c'est deja un JWT signe par le fournisseur OIDC —
// inutile de le rechiffrer, atelier-api-server le revalide integralement a
// chaque requete). `secure` desactive en dev pour fonctionner sur
// http://localhost sans certificat.
const isProd = process.env.NODE_ENV === "production";

function jwtExpiry(token: string): Date | undefined {
  try {
    const payload = token.split(".")[1];
    const json = Buffer.from(payload, "base64url").toString("utf8");
    const exp = JSON.parse(json).exp;
    return typeof exp === "number" ? new Date(exp * 1000) : undefined;
  } catch {
    return undefined;
  }
}

// `refreshToken` optionnel : absent pour un appel post-refresh qui n'en a
// pas recu de nouveau (Kanidm en rotation systematique en pratique, mais
// pas garanti par le protocole OAuth2 en general) — dans ce cas l'ancien
// est conserve tel quel (deja ecrit precedemment), pas efface.
export async function createSession(accessToken: string, refreshToken?: string): Promise<void> {
  const store = await cookies();
  try {
    store.set(SESSION_COOKIE, accessToken, {
      httpOnly: true,
      secure: isProd,
      sameSite: "lax",
      path: "/",
      expires: jwtExpiry(accessToken),
    });
    if (refreshToken) {
      // Pas de date d'expiration explicite ici : la duree de vie du refresh
      // token est decidee par le fournisseur OIDC (Keycloak l'expose en JWT
      // egalement en pratique, mais rien ne le garantit pour un fournisseur
      // OIDC generique — pas de decodage suppose ici) — le cookie survit
      // donc jusqu'a la fermeture du navigateur ou `destroySession()`, et un
      // refresh qui echoue (token revoque/expire cote fournisseur) declenche
      // de toute facon une vraie deconnexion.
      store.set(REFRESH_COOKIE, refreshToken, {
        httpOnly: true,
        secure: isProd,
        sameSite: "lax",
        path: "/",
      });
    }
  } catch {
    // Meme raison que `destroySession()` : un refresh declenche en plein
    // rendu d'un Server Component (pas une Server Action/Route Handler) ne
    // peut pas ecrire les cookies — Next.js leve, pas juste un no-op. Le
    // nouvel `accessToken` reste utilisable pour CETTE requete (retourne en
    // memoire par `refreshAccessToken()`), seule la persistance echoue ; la
    // prochaine requete retentera le refresh, cette fois potentiellement
    // depuis un contexte legal (`/api/auth/refresh`).
  }
}

export async function destroySession(): Promise<void> {
  const store = await cookies();
  try {
    store.delete(SESSION_COOKIE);
    store.delete(REFRESH_COOKIE);
  } catch {
    // `getAccessToken()`/`requireAccessToken()` sont appeles depuis des
    // Server Components (rendu de page), pas seulement des Server
    // Actions/Route Handlers — seuls contextes ou Next.js autorise
    // d'ecrire les cookies. Un refresh qui echoue en plein rendu (refresh
    // token revoque/expire pendant qu'une page normale se rend) tenterait
    // sinon de faire planter la page entiere (500) juste pour un nettoyage
    // de cookies qui, de toute facon, sera rejoue au prochain appel depuis
    // un contexte legal (`/api/auth/refresh`, `/api/auth/login`) — la
    // valeur de retour `null` de `refreshAccessToken()` suffit a declencher
    // la bonne logique (redirection vers /login) sans dependre de cette
    // ecriture ayant reussi ici.
  }
}

/**
 * Echange le refresh token contre un nouvel access token aupres du
 * fournisseur OIDC (`grant_type=refresh_token`, endpoint token standard —
 * `/protocol/openid-connect/token` pour Keycloak, voir `lib/config.ts`) et
 * met a jour la session en place — c'est ce qui rend le front immun a
 * l'expiration du JWT tant que l'utilisateur reste actif : plus besoin de
 * se reconnecter manuellement en plein milieu d'une session de
 * terminal/VS Code. Verifie reellement contre un vrai Keycloak (`POST
 * .../protocol/openid-connect/token`, `200`, nouveau `access_token` +
 * `refresh_token` en rotation).
 */
async function refreshAccessToken(): Promise<string | null> {
  const store = await cookies();
  const refreshToken = store.get(REFRESH_COOKIE)?.value;
  if (!refreshToken) return null;

  const res = await fetch(oidcTokenUrl(), {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "refresh_token",
      refresh_token: refreshToken,
      client_id: OAUTH2_CLIENT_ID,
    }),
    cache: "no-store",
  }).catch(() => null);

  if (!res || !res.ok) {
    // Refresh token lui-meme expire/revoque : plus rien a tenter, seule une
    // vraie reconnexion peut resoudre ca (Keycloak revoque le refresh token
    // si le SSO session max lifetime est atteint, ou si le token est deja
    // arrive en fin de vie/deja consomme) — nettoyer plutot que laisser une
    // session zombie qui echouerait silencieusement en boucle.
    await destroySession();
    return null;
  }

  const body = (await res.json()) as { access_token?: string; refresh_token?: string };
  if (!body.access_token) {
    await destroySession();
    return null;
  }
  await createSession(body.access_token, body.refresh_token);
  return body.access_token;
}

/**
 * Token brut, ou `null` si absent et non rafraichissable — ne redirige
 * jamais. Rafraichit silencieusement via le refresh token si l'access token
 * est absent/expire (ou sur le point de l'etre, voir `REFRESH_SKEW_MS`),
 * avant d'abandonner : c'est ce mecanisme qui rend transparente
 * l'expiration du JWT pour l'appelant.
 */
export async function getAccessToken(): Promise<string | null> {
  const store = await cookies();
  const token = store.get(SESSION_COOKIE)?.value ?? null;

  if (token) {
    const exp = jwtExpiry(token);
    if (!exp || exp.getTime() > Date.now() + REFRESH_SKEW_MS) {
      return token;
    }
  }

  return refreshAccessToken();
}

/** A utiliser dans les Server Components/Actions qui exigent une session. */
export async function requireAccessToken(): Promise<string> {
  const token = await getAccessToken();
  if (!token) {
    redirect("/login");
  }
  return token;
}

// Cookie ephemere (5 min) correlant /api/auth/login -> /api/auth/callback :
// state anti-CSRF + code_verifier PKCE. `sameSite: lax` est necessaire (pas
// `strict`) puisque ce cookie doit survivre a la redirection top-level
// initiee par le fournisseur OIDC au retour du flux.
export async function storePkceParams(state: string, verifier: string): Promise<void> {
  const store = await cookies();
  store.set(PKCE_COOKIE, JSON.stringify({ state, verifier }), {
    httpOnly: true,
    secure: isProd,
    sameSite: "lax",
    path: "/api/auth",
    maxAge: 5 * 60,
  });
}

export async function consumePkceParams(): Promise<{ state: string; verifier: string } | null> {
  const store = await cookies();
  const raw = store.get(PKCE_COOKIE)?.value;
  store.delete(PKCE_COOKIE);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw);
    if (typeof parsed.state === "string" && typeof parsed.verifier === "string") {
      return parsed;
    }
    return null;
  } catch {
    return null;
  }
}

/** Identite de l'utilisateur connecte, telle que la porte son jeton.
 *
 * Lue par decodage du JWT SANS verification de signature : ce jeton vient de
 * notre propre cookie httpOnly, et c'est l'api-server qui le valide vraiment
 * a chaque appel. Ce qui est lu ici ne sert qu'a l'AFFICHAGE (nom, entree de
 * menu) — jamais a autoriser quoi que ce soit, l'autorisation etant refaite
 * cote serveur (voir `ADMIN_ROLE` dans `crates/api-server`). */
export interface CurrentUser {
  subject: string;
  username: string | null;
  roles: string[];
}

export async function getCurrentUser(): Promise<CurrentUser | null> {
  const token = await getAccessToken();
  if (!token) return null;
  try {
    const payload = token.split(".")[1];
    const json = Buffer.from(
      payload.replace(/-/g, "+").replace(/_/g, "/"),
      "base64",
    ).toString("utf8");
    const claims = JSON.parse(json) as {
      sub?: string;
      preferred_username?: string;
      realm_access?: { roles?: string[] };
    };
    if (!claims.sub) return null;
    return {
      subject: claims.sub,
      username: claims.preferred_username ?? null,
      roles: claims.realm_access?.roles ?? [],
    };
  } catch {
    return null;
  }
}
