#!/usr/bin/env bash
# Obtient un vrai access_token OAuth2 (flux authorization_code + PKCE) pour
# un compte Kanidm, en scriptant curl comme le ferait un navigateur.
#
# Sert a tester `atelier-api-server` contre un vrai flux OAuth2 (pas un
# JWKS synthetique) : voir docs/PROGRESS.md, section "api-server : vrai
# flux OAuth2 Kanidm valide". Prerequis, une seule fois :
#
#   kanidm system oauth2 create-public atelier "Atelier" \
#     https://localhost:9443/callback --name idm_admin
#   kanidm system oauth2 update-scope-map atelier idm_all_persons openid \
#     --name idm_admin
#   kanidm person create atelier-test-user "Atelier Test User" --name idm_admin
#   docker exec atelier-kanidm-dev kanidmd recover-account \
#     -c /data/server.toml atelier-test-user
#
# Usage : ./get-oauth2-token.sh <username> <password>
# Affiche le JSON de reponse du endpoint /oauth2/token (access_token,
# id_token, refresh_token) sur stdout.
#
# `kanidm login` (le CLI officiel, deja bien teste) sert uniquement a
# authentifier l'utilisateur et obtenir un bearer token — le flux OAuth2
# proprement dit (authorise/permit/token) n'est pas expose par le CLI, donc
# scripte ici a la main par-dessus ce bearer token. Ce bearer doit etre
# envoye explicitement sur CHAQUE appel /oauth2/* : rien ne l'ajoute
# automatiquement (source de confusion constatee en pratique en testant).

set -euo pipefail

KANIDM_URL="${KANIDM_URL:-https://localhost:8443}"
CA_PATH="${KANIDM_CA_PATH:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/data/ca.pem}"
CLIENT_ID="${ATELIER_OAUTH2_CLIENT_ID:-atelier}"
REDIRECT_URI="${ATELIER_OAUTH2_REDIRECT_URI:-https://localhost:9443/callback}"

USERNAME="${1:?usage: $0 <username> <password>}"
PASSWORD="${2:?usage: $0 <username> <password>}"

CURL=(curl -s --cacert "$CA_PATH")
KANIDM_HOME=$(mktemp -d)
trap 'rm -rf "$KANIDM_HOME"' EXIT

# 1. `kanidm login` (image kanidm/tools, deja utilisee ailleurs dans ce
#    depot, cf. README.md) : ecrit un bearer token dans
#    ~/.cache/kanidm_tokens, JSON indexe par instance puis par identite.
docker run --rm --network host \
  --user "$(id -u):$(id -g)" \
  -v "$CA_PATH:/data/ca.pem:ro" \
  -v "$KANIDM_HOME:/home" \
  -e HOME=/home \
  -e KANIDM_URL="$KANIDM_URL" -e KANIDM_CA_PATH=/data/ca.pem \
  kanidm/tools:latest kanidm login --name "$USERNAME" --password "$PASSWORD" >&2

BEARER=$(python3 -c "
import json
with open('$KANIDM_HOME/.cache/kanidm_tokens') as f:
    d = json.load(f)
tokens = next(iter(d['instances'].values()))['tokens']
print(next(iter(tokens.values())))
")

# 2. PKCE (S256) : verifier aleatoire + challenge derive.
CODE_VERIFIER=$(openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n')
CODE_CHALLENGE=$(printf '%s' "$CODE_VERIFIER" | openssl dgst -sha256 -binary | openssl base64 -A | tr '+/' '-_' | tr -d '=')

# 3. Consentement : POST /oauth2/authorise (JSON, Bearer) — "Permitted" si
#    deja consenti pour cet (utilisateur, client, scope) auparavant, sinon
#    "ConsentRequested" avec un consent_token a POSTer sur
#    /oauth2/authorise/permit (accepte le token brut en corps JSON, pas un
#    objet).
AUTHORISE_BODY=$(cat <<JSON
{"response_type":"code","client_id":"$CLIENT_ID","state":"cli","code_challenge":"$CODE_CHALLENGE","code_challenge_method":"S256","redirect_uri":"$REDIRECT_URI","scope":"openid"}
JSON
)
CONSENT=$("${CURL[@]}" -X POST "$KANIDM_URL/oauth2/authorise" \
  -H "Authorization: Bearer $BEARER" -H 'Content-Type: application/json' \
  -d "$AUTHORISE_BODY")
CONSENT_TOKEN=$(echo "$CONSENT" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("ConsentRequested",{}).get("consent_token","") if isinstance(d,dict) else "")')
if [ -n "$CONSENT_TOKEN" ]; then
  "${CURL[@]}" -X POST "$KANIDM_URL/oauth2/authorise/permit" \
    -H "Authorization: Bearer $BEARER" -H 'Content-Type: application/json' \
    -d "\"$CONSENT_TOKEN\"" > /dev/null
fi

# 4. GET (forme "navigateur", seule a produire une vraie redirection 302
#    avec ?code=... en query string) sans suivre la redirection.
LOCATION=$("${CURL[@]}" -D - -o /dev/null "$KANIDM_URL/oauth2/authorise" \
  -H "Authorization: Bearer $BEARER" \
  --get --data-urlencode "response_type=code" --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "state=cli" --data-urlencode "code_challenge=$CODE_CHALLENGE" \
  --data-urlencode "code_challenge_method=S256" --data-urlencode "redirect_uri=$REDIRECT_URI" \
  --data-urlencode "scope=openid" \
  | grep -i '^location:' | tr -d '\r' | sed 's/^[Ll]ocation: //')
CODE=$(python3 -c "import sys,urllib.parse as u; q=u.urlparse('$LOCATION').query; print(dict(u.parse_qsl(q))['code'])")

# 5. Echange du code contre un token.
"${CURL[@]}" -X POST "$KANIDM_URL/oauth2/token" \
  --data-urlencode "grant_type=authorization_code" \
  --data-urlencode "code=$CODE" \
  --data-urlencode "redirect_uri=$REDIRECT_URI" \
  --data-urlencode "code_verifier=$CODE_VERIFIER" \
  --data-urlencode "client_id=$CLIENT_ID"
echo
