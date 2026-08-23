import { NextRequest, NextResponse } from "next/server";
import { OAUTH2_CLIENT_ID, oauth2RedirectUri, oidcTokenUrl, requestOrigin } from "@/lib/config";
import { consumePkceParams, createSession } from "@/lib/session";

export async function GET(request: NextRequest) {
  const origin = requestOrigin(request);
  const code = request.nextUrl.searchParams.get("code");
  const state = request.nextUrl.searchParams.get("state");
  const oauthError = request.nextUrl.searchParams.get("error");

  const pkce = await consumePkceParams();

  if (oauthError) {
    return loginError(origin, `le fournisseur OIDC a refuse la connexion : ${oauthError}`);
  }
  if (!code || !state) {
    return loginError(origin, "reponse OAuth2 incomplete (code/state manquant)");
  }
  if (!pkce) {
    return loginError(origin, "session de connexion expiree, reessayez");
  }
  if (state !== pkce.state) {
    return loginError(origin, "state OAuth2 invalide (anti-CSRF)");
  }

  const redirectUri = oauth2RedirectUri(origin);
  const tokenRes = await fetch(oidcTokenUrl(), {
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
    return loginError(
      origin,
      `echange du code aupres du fournisseur OIDC echoue (${tokenRes.status}) ${body}`,
    );
  }

  const { access_token: accessToken, refresh_token: refreshToken } = (await tokenRes.json()) as {
    access_token?: string;
    refresh_token?: string;
  };
  if (!accessToken) {
    return loginError(origin, "reponse token du fournisseur OIDC sans access_token");
  }

  await createSession(accessToken, refreshToken);
  return NextResponse.redirect(new URL("/", origin));
}

function loginError(origin: string, message: string) {
  const url = new URL("/login", origin);
  url.searchParams.set("error", message);
  return NextResponse.redirect(url);
}
