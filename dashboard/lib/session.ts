import "server-only";
import { cookies } from "next/headers";
import { redirect } from "next/navigation";

const SESSION_COOKIE = "atelier_session";
const PKCE_COOKIE = "atelier_oauth_pkce";

// httpOnly : le token n'est jamais lisible par le JS cote navigateur (seule
// defense necessaire ici, c'est deja un JWT signe par Kanidm — inutile de
// le rechiffrer, atelier-api-server le revalide integralement a chaque
// requete). `secure` desactive en dev pour fonctionner sur http://localhost
// sans certificat.
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

export async function createSession(accessToken: string): Promise<void> {
  const store = await cookies();
  store.set(SESSION_COOKIE, accessToken, {
    httpOnly: true,
    secure: isProd,
    sameSite: "lax",
    path: "/",
    expires: jwtExpiry(accessToken),
  });
}

export async function destroySession(): Promise<void> {
  const store = await cookies();
  store.delete(SESSION_COOKIE);
}

/** Token brut, ou `null` si absent — ne redirige jamais. */
export async function getAccessToken(): Promise<string | null> {
  const store = await cookies();
  return store.get(SESSION_COOKIE)?.value ?? null;
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
// initiee par Kanidm au retour du flux.
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
