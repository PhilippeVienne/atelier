import { NextRequest, NextResponse } from "next/server";
import { KANIDM_URL, OAUTH2_CLIENT_ID, oauth2RedirectUri } from "@/lib/config";
import { consumePkceParams, createSession } from "@/lib/session";

export async function GET(request: NextRequest) {
  const code = request.nextUrl.searchParams.get("code");
  const state = request.nextUrl.searchParams.get("state");
  const oauthError = request.nextUrl.searchParams.get("error");

  const pkce = await consumePkceParams();

  if (oauthError) {
    return loginError(request, `Kanidm a refuse la connexion : ${oauthError}`);
  }
  if (!code || !state) {
    return loginError(request, "reponse OAuth2 incomplete (code/state manquant)");
  }
  if (!pkce) {
    return loginError(request, "session de connexion expiree, reessayez");
  }
  if (state !== pkce.state) {
    return loginError(request, "state OAuth2 invalide (anti-CSRF)");
  }

  const redirectUri = oauth2RedirectUri(request.nextUrl.origin);
  const tokenRes = await fetch(new URL("/oauth2/token", KANIDM_URL), {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "authorization_code",
      code,
      redirect_uri: redirectUri,
      code_verifier: pkce.verifier,
      client_id: OAUTH2_CLIENT_ID,
    }),
    cache: "no-store",
  });

  if (!tokenRes.ok) {
    const body = await tokenRes.text().catch(() => "");
    return loginError(request, `echange du code aupres de Kanidm echoue (${tokenRes.status}) ${body}`);
  }

  const { access_token: accessToken } = (await tokenRes.json()) as { access_token?: string };
  if (!accessToken) {
    return loginError(request, "reponse token Kanidm sans access_token");
  }

  await createSession(accessToken);
  return NextResponse.redirect(new URL("/", request.nextUrl.origin));
}

function loginError(request: NextRequest, message: string) {
  const url = new URL("/login", request.nextUrl.origin);
  url.searchParams.set("error", message);
  return NextResponse.redirect(url);
}
