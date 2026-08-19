import { NextRequest, NextResponse } from "next/server";
import { KANIDM_URL, OAUTH2_CLIENT_ID, OAUTH2_SCOPE, oauth2RedirectUri } from "@/lib/config";
import { generatePkcePair, generateState } from "@/lib/pkce";
import { storePkceParams } from "@/lib/session";

// Redirige vers l'UI Kanidm (pas directement /oauth2/authorise : cet
// endpoint API exige un `Authorization: Bearer` deja present, voir
// deploy/dev/kanidm/get-oauth2-token.sh — un navigateur qui n'a pas encore
// de session ne peut pas le fournir). `/ui/oauth2` sert la meme SPA Kanidm,
// qui gere elle-meme le login puis la sequence authorise/permit avec son
// propre bearer, avant de rediriger le navigateur vers `redirect_uri` avec
// le code — c'est cette derniere redirection (une vraie navigation
// top-level) que notre callback recoit.
export async function GET(request: NextRequest) {
  const { verifier, challenge } = generatePkcePair();
  const state = generateState();
  await storePkceParams(state, verifier);

  const redirectUri = oauth2RedirectUri(request.nextUrl.origin);
  const authoriseUrl = new URL("/ui/oauth2", KANIDM_URL);
  authoriseUrl.searchParams.set("response_type", "code");
  authoriseUrl.searchParams.set("client_id", OAUTH2_CLIENT_ID);
  authoriseUrl.searchParams.set("redirect_uri", redirectUri);
  authoriseUrl.searchParams.set("scope", OAUTH2_SCOPE);
  authoriseUrl.searchParams.set("state", state);
  authoriseUrl.searchParams.set("code_challenge", challenge);
  authoriseUrl.searchParams.set("code_challenge_method", "S256");

  return NextResponse.redirect(authoriseUrl);
}
