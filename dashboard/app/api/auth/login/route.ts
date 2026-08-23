import { NextRequest, NextResponse } from "next/server";
import { OAUTH2_CLIENT_ID, OAUTH2_SCOPE, oauth2RedirectUri, oidcAuthorizeUrl } from "@/lib/config";
import { generatePkcePair, generateState } from "@/lib/pkce";
import { storePkceParams } from "@/lib/session";

// Redirige vers l'endpoint d'autorisation OIDC standard du realm
// (`/protocol/openid-connect/auth` pour Keycloak, voir
// deploy/dev/keycloak/README.md) : le navigateur y gere lui-meme le login
// (formulaire Keycloak) puis redirige vers `redirect_uri` avec le code —
// c'est cette redirection (une vraie navigation top-level) que notre
// callback recoit.
export async function GET(request: NextRequest) {
  const { verifier, challenge } = generatePkcePair();
  const state = generateState();
  await storePkceParams(state, verifier);

  const redirectUri = oauth2RedirectUri(request.nextUrl.origin);
  const authoriseUrl = oidcAuthorizeUrl();
  authoriseUrl.searchParams.set("response_type", "code");
  authoriseUrl.searchParams.set("client_id", OAUTH2_CLIENT_ID);
  authoriseUrl.searchParams.set("redirect_uri", redirectUri);
  authoriseUrl.searchParams.set("scope", OAUTH2_SCOPE);
  authoriseUrl.searchParams.set("state", state);
  authoriseUrl.searchParams.set("code_challenge", challenge);
  authoriseUrl.searchParams.set("code_challenge_method", "S256");

  return NextResponse.redirect(authoriseUrl);
}
