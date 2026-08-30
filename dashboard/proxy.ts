import { NextResponse } from "next/server";
import type { NextRequest } from "next/server";
import { OAUTH2_CLIENT_ID, oidcTokenUrl } from "@/lib/config";

const PUBLIC_PATHS = ["/login"];
const SESSION_COOKIE = "atelier_session";
const REFRESH_COOKIE = "atelier_refresh";

// Meme marge que `lib/session.ts::REFRESH_SKEW_MS` (rafraichir un peu avant
// l'expiration reelle plutot que pile dessus).
const REFRESH_SKEW_MS = 30_000;

const isProd = process.env.NODE_ENV === "production";

function jwtExpiryMs(token: string): number | undefined {
  try {
    const payload = token.split(".")[1];
    const json = Buffer.from(payload, "base64url").toString("utf8");
    const exp = JSON.parse(json).exp;
    return typeof exp === "number" ? exp * 1000 : undefined;
  } catch {
    return undefined;
  }
}

function isFresh(token: string | undefined): boolean {
  if (!token) return false;
  const exp = jwtExpiryMs(token);
  return !exp || exp > Date.now() + REFRESH_SKEW_MS;
}

interface RefreshedTokens {
  accessToken: string;
  refreshToken?: string;
}

async function refresh(refreshToken: string): Promise<RefreshedTokens | null> {
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
  if (!res || !res.ok) return null;
  const body = (await res.json()) as { access_token?: string; refresh_token?: string };
  if (!body.access_token) return null;
  return { accessToken: body.access_token, refreshToken: body.refresh_token };
}

// Anciennement une simple presence de cookie ("hasSession = cookie present",
// sans verifier l'expiration) : ca provoquait une boucle de redirection
// reelle (constatee en session de debug) des que l'access token expirait —
// `/login` redirigeait vers `/` sur la seule presence du cookie perime,
// tandis que le rendu de `/` (via `lib/session.ts::requireAccessToken`,
// appele depuis un Server Component) echouait a rafraichir ET a persister
// le cookie zombie (Next.js interdit d'ecrire des cookies depuis le rendu
// d'un Server Component — seul un Route Handler/Server Action/ce Proxy le
// peut), donc redirigeait vers `/login` en boucle indefiniment. Le Proxy
// EST un contexte legal pour ecrire des cookies (`response.cookies.set`,
// reellement persistes cote navigateur) : c'est desormais ici, avant meme
// que la page ne se rende, que le refresh a lieu et que son resultat
// (succes ou echec) est traduit en un etat de cookie coherent — plus de
// zombie qui ne se met jamais a jour.
export default async function proxy(request: NextRequest) {
  const { pathname } = request.nextUrl;

  if (pathname.startsWith("/api/auth/") || pathname.startsWith("/_next")) {
    return NextResponse.next();
  }

  const isPublic = PUBLIC_PATHS.includes(pathname);
  let accessToken = request.cookies.get(SESSION_COOKIE)?.value;
  const refreshToken = request.cookies.get(REFRESH_COOKIE)?.value;
  let refreshed: RefreshedTokens | null = null;

  if (!isFresh(accessToken) && refreshToken) {
    refreshed = await refresh(refreshToken);
    if (refreshed) accessToken = refreshed.accessToken;
  }

  const hasSession = isFresh(accessToken);

  let response: NextResponse;
  if (!hasSession && !isPublic) {
    response = NextResponse.redirect(new URL("/login", request.url));
  } else if (hasSession && isPublic) {
    response = NextResponse.redirect(new URL("/", request.url));
  } else {
    response = NextResponse.next();
  }

  if (refreshed) {
    response.cookies.set(SESSION_COOKIE, refreshed.accessToken, {
      httpOnly: true,
      secure: isProd,
      sameSite: "lax",
      path: "/",
    });
    if (refreshed.refreshToken) {
      response.cookies.set(REFRESH_COOKIE, refreshed.refreshToken, {
        httpOnly: true,
        secure: isProd,
        sameSite: "lax",
        path: "/",
      });
    }
  } else if (!hasSession) {
    // Refresh absent/echoue : purge le cookie zombie plutot que de le
    // laisser presenter indefiniment un access token mort — c'est cette
    // purge, absente avant, qui casse la boucle de redirection.
    response.cookies.delete(SESSION_COOKIE);
    response.cookies.delete(REFRESH_COOKIE);
  }

  return response;
}

export const config = {
  matcher: ["/((?!_next/static|_next/image|favicon.ico).*)"],
};
