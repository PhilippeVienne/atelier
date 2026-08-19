# Dashboard Atelier

Frontend Next.js (App Router) pour `atelier-api-server` : CRUD des
`Workshop` (creation, suspend/resume, suppression), authentification via le
flux OAuth2/PKCE de Kanidm (voir `docs/PROGRESS.md` et
`deploy/dev/kanidm/README.md`).

## Architecture d'authentification (backend-for-frontend)

Le dashboard tient le rôle de client OAuth2 public (PKCE), pas le
navigateur directement : `/api/auth/login` genere le couple PKCE et
redirige vers `${ATELIER_KANIDM_URL}/ui/oauth2` (l'UI Kanidm, pas
directement `/oauth2/authorise` — cet endpoint API exige un `Authorization:
Bearer` deja present, qu'un navigateur sans session ne peut pas fournir).
Kanidm gere login + consentement, puis redirige le navigateur vers
`/api/auth/callback?code=...&state=...` ; ce handler echange le code contre
un `access_token` aupres de Kanidm et le stocke dans un cookie **httpOnly**
(`atelier_session`) — jamais expose au JS cote navigateur. Toutes les
requetes vers `atelier-api-server` (Server Components, Server Actions)
relaient ce token en `Authorization: Bearer`, qui le revalide integralement
(signature JWKS + audience) a chaque appel : le dashboard ne fait aucune
verification de securite lui-meme, `proxy.ts` ne fait qu'une verification
optimiste (presence du cookie) pour eviter un aller-retour inutile.

Voir `lib/session.ts`, `lib/api-server.ts`, `app/api/auth/{login,callback}/route.ts`.

## Variables d'environnement

| Variable | Defaut (dev) | Usage |
|---|---|---|
| `ATELIER_API_SERVER_URL` | `http://localhost:8080` | Base URL d'`atelier-api-server` |
| `ATELIER_KANIDM_URL` | `https://localhost:8443` | Base URL de Kanidm |
| `ATELIER_OAUTH2_CLIENT_ID` | `atelier` | Client OAuth2 public cote Kanidm |
| `ATELIER_OAUTH2_REDIRECT_URI` | `<origin>/api/auth/callback` | Doit correspondre exactement a une redirect_uri enregistree cote Kanidm |
| `NODE_EXTRA_CA_CERTS` | — | CA du Kanidm de dev (auto-signee), sinon l'echange du code echoue en TLS (`deploy/dev/kanidm/data/ca.pem`) |

## Tester en local contre l'infra de dev reelle

Prerequis, une seule fois (voir `deploy/dev/kanidm/README.md` pour le
detail complet du client OAuth2 `atelier`) :

```sh
kanidm system oauth2 add-redirect-url atelier http://localhost:3000/api/auth/callback --name idm_admin
kanidm system oauth2 enable-localhost-redirects atelier --name idm_admin
```

Puis :

```sh
# api-server, cf. deploy/dev/kanidm/README.md pour ATELIER_JWT_*
cargo run -p atelier-api-server

# dashboard
cd dashboard
export NODE_EXTRA_CA_CERTS="$(pwd)/../deploy/dev/kanidm/data/ca.pem"
npm run dev
```

Ouvrir [http://localhost:3000](http://localhost:3000) : redirige vers
`/login`, "Se connecter avec Kanidm" declenche le vrai flux OAuth2.

Deja verifie reellement cette session (voir `docs/PROGRESS.md`) : flux
complet login -> callback -> session -> liste/creation/suppression de
Workshops, valide contre un vrai Kanidm, un vrai `atelier-api-server` et un
vrai cluster kind — seule la partie "clic humain dans l'UI de login
Kanidm" n'est pas automatisable (le reste du flux, y compris l'echange du
code et l'appel a l'API, est teste de bout en bout via curl en scriptant le
cote Kanidm comme le fait `deploy/dev/kanidm/get-oauth2-token.sh`).
