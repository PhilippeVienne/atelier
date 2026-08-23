# Dashboard Atelier

Frontend Next.js (App Router) pour `atelier-api-server` : CRUD des
`Workshop` (creation, suspend/resume, suppression), authentification via le
flux OAuth2/PKCE d'un fournisseur OIDC generique (Keycloak en dev, voir
`docs/PROGRESS.md` et `deploy/dev/keycloak/README.md`).

## Architecture d'authentification (backend-for-frontend)

Le dashboard tient le rôle de client OAuth2 public (PKCE), pas le
navigateur directement : `/api/auth/login` genere le couple PKCE et
redirige vers l'endpoint d'autorisation OIDC standard du realm
(`${ATELIER_OIDC_ISSUER_URL}/protocol/openid-connect/auth` par defaut,
convention Keycloak — voir `lib/config.ts`). Le fournisseur gere login +
consentement, puis redirige le navigateur vers
`/api/auth/callback?code=...&state=...` ; ce handler echange le code contre
un `access_token` aupres du fournisseur (endpoint token standard,
`/protocol/openid-connect/token` par defaut) et le stocke dans un cookie
**httpOnly** (`atelier_session`) — jamais expose au JS cote navigateur.
Toutes les requetes vers `atelier-api-server` (Server Components, Server
Actions) relaient ce token en `Authorization: Bearer`, qui le revalide
integralement (signature JWKS + audience) a chaque appel : le dashboard ne
fait aucune verification de securite lui-meme, `proxy.ts` ne fait qu'une
verification optimiste (presence du cookie) pour eviter un aller-retour
inutile.

Les chemins `/protocol/openid-connect/{auth,token}` sont configurables
separement de la base (`ATELIER_OIDC_AUTHORIZE_PATH`/
`ATELIER_OIDC_TOKEN_PATH`) plutot que resolus via la decouverte OIDC
(`/.well-known/openid-configuration`) : un fournisseur OIDC non-Keycloak
n'aurait qu'a surcharger ces deux variables, sans appel reseau
supplementaire a amortir a chaque login/refresh.

Voir `lib/config.ts`, `lib/session.ts`, `lib/api-server.ts`,
`app/api/auth/{login,callback}/route.ts`.

## Variables d'environnement

| Variable | Defaut (dev) | Usage |
|---|---|---|
| `ATELIER_API_SERVER_URL` | `http://localhost:8080` | Base URL d'`atelier-api-server` |
| `ATELIER_OIDC_ISSUER_URL` | `http://127.0.0.1:8080/realms/atelier` | Base URL du realm OIDC (Keycloak) — PAS la racine du serveur |
| `ATELIER_OIDC_AUTHORIZE_PATH` | `/protocol/openid-connect/auth` | Chemin de l'endpoint d'autorisation, relatif a `ATELIER_OIDC_ISSUER_URL` |
| `ATELIER_OIDC_TOKEN_PATH` | `/protocol/openid-connect/token` | Chemin de l'endpoint token, relatif a `ATELIER_OIDC_ISSUER_URL` |
| `ATELIER_OAUTH2_CLIENT_ID` | `atelier-dashboard` | Client OAuth2 public cote fournisseur OIDC |
| `ATELIER_OAUTH2_REDIRECT_URI` | `<origin>/api/auth/callback` | Doit correspondre exactement a une redirect_uri enregistree cote fournisseur |
| `NODE_EXTRA_CA_CERTS` | — | CA de dev (auto-signee) si le fournisseur OIDC est servi en TLS, sinon l'echange du code echoue en TLS (`deploy/dev/pki/ca/atelier-ca.crt`) |

## Tester en local contre l'infra de dev reelle

Prerequis, une seule fois (voir `deploy/dev/keycloak/README.md` pour le
detail complet du realm "atelier" et du client `atelier-dashboard`, deja
pre-configures dans `realm-export.json` — rien a creer manuellement).

Puis :

```sh
# api-server, cf. deploy/dev/keycloak/README.md pour ATELIER_JWT_*
cargo run -p atelier-api-server

# dashboard
cd dashboard
npm run dev
```

Ouvrir [http://localhost:3000](http://localhost:3000) : redirige vers
`/login`, "Se connecter" declenche le vrai flux OAuth2 vers Keycloak.

Deja verifie reellement cette session (voir `docs/PROGRESS.md`) : flux
complet login -> callback -> session -> refresh transparent, valide contre
un vrai Keycloak de dev — l'echange du code et le refresh du token sont
testes de bout en bout via curl/script en obtenant un token directement
aupres de Keycloak (`grant_type=password`, voir
`deploy/dev/keycloak/README.md` section 3) puis en simulant l'appel
`/api/auth/callback` et `/api/auth/refresh` ; seule la partie "clic humain
dans le formulaire de login Keycloak" n'est pas automatisable dans cet
environnement.
