//! Validation des JWT entrants. L'issuer de confiance est un fournisseur
//! OIDC standard, configure par variables d'environnement (`ATELIER_JWT_*`) :
//! rien de specifique a un provider n'est code en dur ici, seulement les
//! mecanismes generiques RFC 7517 (JWKS, jeu de cles publiques) et RFC 7636
//! (PKCE, cote client — ce fichier ne fait que verifier la signature et les
//! claims du JWT final). En pratique, Kanidm, Keycloak ou tout autre
//! fournisseur OIDC/OAuth2 conforme conviennent : ce module ne parle qu'a
//! l'`issuer` configure, jamais a une API propre a un produit donne. Le JWKS
//! est mis en cache et rafraichi periodiquement (voir
//! [`AuthState::from_env`] et [`spawn_jwks_refresh_task`]), avec un refetch
//! immediat si un `kid` inconnu se presente (rotation de cles cote
//! fournisseur).

use anyhow::{Context, Result};
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, Jwk, JwkSet, KeyAlgorithm};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Intervalle de rafraichissement periodique du JWKS en tache de fond (voir
/// [`spawn_jwks_refresh_task`]) : couvre une rotation de cles planifiee cote
/// fournisseur OIDC sans jamais avoir besoin de redemarrer `api-server`.
const JWKS_REFRESH_INTERVAL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone)]
pub struct TrustedIssuer {
    pub issuer: String,
    pub jwks_url: String,
    /// Valeur exacte attendue dans la claim `aud` (`ATELIER_JWT_AUDIENCE`,
    /// typiquement le `client_id` de l'OAuth2 Resource Server Atelier cote
    /// fournisseur OIDC). Requise, pas optionnelle : `jsonwebtoken` valide
    /// `aud` des qu'elle est presente dans le token (`validate_aud: true`
    /// par defaut) — un vrai token en porte toujours une. Sans audience
    /// configuree ici, `Validation::aud` resterait `None` et **tout** token
    /// reel serait rejete (`InvalidAudience`), constate en pratique en
    /// testant contre un vrai flux OAuth2 (voir docs/PROGRESS.md) — pas
    /// visible avec des tokens de test qui omettent simplement la claim
    /// `aud`.
    pub audience: String,
    /// Chemin vers un certificat CA supplementaire a faire confiance pour
    /// recuperer le JWKS (`ATELIER_JWT_CA_PATH`) : necessaire pour un
    /// fournisseur OIDC auto-heberge avec une CA privee (dev y compris —
    /// certificat auto-signe). `reqwest` (backend `rustls-tls` de ce
    /// workspace) ne consulte ni le magasin de confiance du systeme, ni
    /// `SSL_CERT_FILE` ; sans ce chemin explicite, seules les CA publiques
    /// standard (webpki-roots) sont acceptees.
    pub ca_path: Option<String>,
}

impl TrustedIssuer {
    /// `ATELIER_JWT_ISSUER` : valeur exacte attendue dans la claim `iss` des
    /// JWT presentes. `ATELIER_JWT_JWKS_URL` : URL du jeu de cles publiques
    /// (endpoint JWKS RFC 7517 du fournisseur OIDC configure pour Atelier).
    /// `ATELIER_JWT_AUDIENCE` : `client_id` attendu dans `aud` (voir doc du
    /// champ `audience`). `Ok(None)` si `ATELIER_JWT_ISSUER` est absent :
    /// l'auth est alors desactivee (toutes les requetes refusees par le
    /// middleware, utile seulement pour demarrer le binaire en dev sans
    /// fournisseur OIDC configure).
    pub fn from_env() -> Result<Option<Self>> {
        let Ok(issuer) = std::env::var("ATELIER_JWT_ISSUER") else {
            return Ok(None);
        };
        let jwks_url = std::env::var("ATELIER_JWT_JWKS_URL")
            .context("ATELIER_JWT_ISSUER est defini mais ATELIER_JWT_JWKS_URL est absent")?;
        let audience = std::env::var("ATELIER_JWT_AUDIENCE")
            .context("ATELIER_JWT_ISSUER est defini mais ATELIER_JWT_AUDIENCE est absent")?;
        let ca_path = std::env::var("ATELIER_JWT_CA_PATH").ok();
        Ok(Some(Self {
            issuer,
            jwks_url,
            audience,
            ca_path,
        }))
    }

    pub async fn fetch_jwks(&self) -> Result<JwkSet> {
        // `ATELIER_JWT_CA_PATH` (voir doc du champ `ca_path`) est un cas
        // particulier du mecanisme generique
        // `atelier_common::tls_client::client_builder_trusting_extra_ca`
        // (spec docs/specs/15-souverainete-airgap-inference-gpu.md §3.2,
        // tache 11.1) : garde son propre nom de variable pour ne pas casser
        // les deploiements existants qui la positionnent deja.
        let client =
            atelier_common::tls_client::client_builder_trusting_extra_ca("ATELIER_JWT_CA_PATH")?
                .build()
                .context("construction du client HTTP pour le JWKS")?;

        client
            .get(&self.jwks_url)
            .send()
            .await
            .context("requete de recuperation du JWKS")?
            .error_for_status()
            .context("le JWKS n'a pas pu etre recupere")?
            .json()
            .await
            .context("reponse JWKS invalide (JSON attendu)")
    }
}

/// Etat partage du middleware d'authentification. `None` (pas de fournisseur
/// OIDC configure) fait refuser systematiquement toute requete authentifiee :
/// evite de demarrer silencieusement en mode "tout est autorise".
pub enum AuthState {
    Configured {
        issuer: String,
        audience: String,
        /// Cache JWKS courant, partage avec la tache de fond de
        /// rafraichissement periodique ([`spawn_jwks_refresh_task`]) et mis
        /// a jour a la volee par [`require_auth`] des qu'un `kid` inconnu se
        /// presente.
        jwks: Arc<RwLock<JwkSet>>,
        /// Conserve pour permettre un refetch immediat du JWKS (`kid`
        /// inconnu, ou rafraichissement periodique) sans redemarrer le
        /// process.
        trusted: TrustedIssuer,
    },
    Disabled,
}

impl AuthState {
    pub async fn from_env() -> Result<Self> {
        match TrustedIssuer::from_env()? {
            Some(trusted) => {
                let jwks = trusted.fetch_jwks().await?;
                tracing::info!(issuer = %trusted.issuer, audience = %trusted.audience, keys = jwks.keys.len(), "JWKS charge");
                let jwks = Arc::new(RwLock::new(jwks));
                spawn_jwks_refresh_task(trusted.clone(), Arc::clone(&jwks));
                Ok(AuthState::Configured {
                    issuer: trusted.issuer.clone(),
                    audience: trusted.audience.clone(),
                    jwks,
                    trusted,
                })
            }
            None => {
                tracing::warn!(
                    "ATELIER_JWT_ISSUER absent : authentification desactivee, toutes les requetes protegees seront refusees"
                );
                Ok(AuthState::Disabled)
            }
        }
    }

    /// Construit un `AuthState::Configured` a partir d'un JWKS deja connu,
    /// sans tache de fond de rafraichissement — utilise par les tests
    /// d'integration, qui fournissent une cle de test statique et n'ont pas
    /// besoin d'un refetch reseau.
    #[doc(hidden)]
    pub fn from_static_jwks(issuer: String, audience: String, jwks: JwkSet) -> Self {
        let trusted = TrustedIssuer {
            issuer: issuer.clone(),
            jwks_url: String::new(),
            audience: audience.clone(),
            ca_path: None,
        };
        AuthState::Configured {
            issuer,
            audience,
            jwks: Arc::new(RwLock::new(jwks)),
            trusted,
        }
    }
}

/// Rafraichit le JWKS toutes les [`JWKS_REFRESH_INTERVAL`] : couvre une
/// rotation de cles planifiee cote fournisseur OIDC (Keycloak tourne
/// typiquement ses cles de signature toutes les quelques heures/jours par
/// defaut) sans jamais avoir besoin de redemarrer `api-server`. Best-effort :
/// un echec ponctuel (fournisseur temporairement injoignable) journalise
/// simplement et reessaie au prochain tick, en gardant le cache existant.
fn spawn_jwks_refresh_task(trusted: TrustedIssuer, jwks: Arc<RwLock<JwkSet>>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(JWKS_REFRESH_INTERVAL);
        // Le premier tick d'un `tokio::time::interval` se declenche
        // immediatement : on vient deja de recuperer le JWKS dans
        // `AuthState::from_env`, pas besoin de le refaire tout de suite.
        interval.tick().await;
        loop {
            interval.tick().await;
            match trusted.fetch_jwks().await {
                Ok(fresh) => {
                    tracing::info!(
                        keys = fresh.keys.len(),
                        "JWKS rafraichi (tache de fond periodique)"
                    );
                    *jwks.write().await = fresh;
                }
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        "echec du rafraichissement periodique du JWKS, cache conserve"
                    );
                }
            }
        }
    });
}

/// Identite du sujet JWT authentifie, injectee dans les extensions de la
/// requete par [`require_auth`]. C'est ce sujet — jamais une valeur fournie
/// par le client dans le corps de la requete — qui devient
/// `WorkshopSpec.owner_subject` (voir `routes::create_workshop`) : un
/// client ne peut donc jamais creer ni manipuler un Workshop au nom de
/// quelqu'un d'autre.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    /// Sujet OIDC : qui agit.
    pub subject: String,
    /// Groupes du sujet, tels que le jeton les porte. C'est ce qui donne
    /// acces a un Workshop (`WorkshopSpec.owner_group`, voir
    /// `docs/specs/07-groupes.md`).
    ///
    /// Portes ICI plutot que lus depuis `Claims` a chaque appel : le
    /// controle d'acces se fait en une dizaine d'endroits, et faire dependre
    /// chacun d'un `Extension<Claims>` supplementaire multiplie les
    /// occasions d'en oublier un.
    pub groups: Vec<String>,
}

/// Claims JWT standards OIDC extraites du token. Injectees telles quelles
/// dans les extensions de la requete par [`require_auth`] (voir
/// `Extension<Claims>`), pour que les handlers en aval qui ont besoin de
/// plus que le seul sujet (ex: afficher un nom d'utilisateur, verifier une
/// appartenance a un groupe) n'aient pas a re-decoder le JWT.
#[derive(Debug, Clone, Deserialize)]
pub struct Claims {
    /// Identifiant unique et stable du sujet cote fournisseur OIDC — c'est
    /// cette valeur qui devient [`AuthenticatedUser`].
    pub sub: String,
    /// Nom d'utilisateur lisible (claim standard OIDC `preferred_username`),
    /// absente de certains fournisseurs/configurations : `Option`.
    pub preferred_username: Option<String>,
    /// Adresse email (claim standard OIDC `email`), absente si le scope
    /// `email` n'a pas ete demande/accorde : `Option`.
    pub email: Option<String>,
    /// Appartenances a des groupes : pas une claim standard OIDC core (varie
    /// selon le fournisseur — mapper Keycloak, extension Kanidm, etc.),
    /// quasi jamais garantie presente : `Option`.
    #[serde(default)]
    pub groups: Option<Vec<String>>,
    /// Roles de realm, au format Keycloak (`{"roles": ["admin", ...]}`).
    /// Pas une claim OIDC standard : d'autres fournisseurs les exposent
    /// autrement, d'ou l'`Option` et l'absence de toute exigence de presence
    /// — un jeton sans roles est parfaitement valide, il n'a simplement
    /// aucun privilege d'administration.
    #[serde(default)]
    pub realm_access: Option<RealmAccess>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RealmAccess {
    #[serde(default)]
    pub roles: Vec<String>,
}

impl Claims {
    /// Le sujet porte-t-il ce role de realm ?
    ///
    /// L'autorisation se joue ICI, cote serveur, jamais sur ce que
    /// l'interface choisit d'afficher : masquer un bouton n'empeche personne
    /// d'appeler la route directement.
    pub fn has_role(&self, role: &str) -> bool {
        self.realm_access
            .as_ref()
            .is_some_and(|access| access.roles.iter().any(|r| r == role))
    }
}

pub async fn require_auth(
    State(auth): State<Arc<AuthState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let AuthState::Configured {
        issuer,
        audience,
        jwks,
        trusted,
    } = auth.as_ref()
    else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "authentification non configuree",
        )
            .into_response();
    };

    let Some(token) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return (
            StatusCode::UNAUTHORIZED,
            "en-tete Authorization: Bearer <jwt> requis",
        )
            .into_response();
    };

    let Ok(header) = decode_header(token) else {
        return (StatusCode::UNAUTHORIZED, "en-tete JWT invalide").into_response();
    };
    let Some(kid) = header.kid else {
        return (
            StatusCode::UNAUTHORIZED,
            "JWT sans champ kid, impossible de choisir la cle",
        )
            .into_response();
    };

    // Refetch immediat si ce `kid` n'est pas (encore) dans le cache local :
    // une rotation de cles cote fournisseur OIDC ne doit jamais faire
    // rejeter un token par ailleurs valide simplement parce que la tache de
    // fond periodique ([`spawn_jwks_refresh_task`]) n'est pas encore
    // repassee. Best-effort : si le refetch echoue (fournisseur injoignable
    // momentanement), on retente quand meme la validation avec le cache
    // existant plutot que de refuser tout de suite.
    let known = jwks.read().await.find(&kid).is_some();
    if !known {
        tracing::info!(%kid, "kid absent du cache JWKS local, refetch immediat");
        match trusted.fetch_jwks().await {
            Ok(fresh) => *jwks.write().await = fresh,
            Err(err) => tracing::warn!(?err, "refetch JWKS immediat echoue"),
        }
    }

    let result = {
        let jwks = jwks.read().await;
        validate_token(token, issuer, audience, &jwks)
    };

    match result {
        Ok(claims) => {
            req.extensions_mut().insert(AuthenticatedUser {
                subject: claims.sub.clone(),
                groups: claims.groups.clone().unwrap_or_default(),
            });
            req.extensions_mut().insert(claims);
            next.run(req).await
        }
        Err(err) => {
            tracing::warn!(?err, "JWT refuse");
            (StatusCode::UNAUTHORIZED, "JWT invalide").into_response()
        }
    }
}

fn validate_token(token: &str, issuer: &str, audience: &str, jwks: &JwkSet) -> Result<Claims> {
    let header = decode_header(token).context("en-tete JWT invalide")?;
    let kid = header
        .kid
        .context("JWT sans champ kid, impossible de choisir la cle")?;
    let jwk = jwks.find(&kid).context("kid absent du JWKS charge")?;

    let algorithm = algorithm_for_jwk(jwk)?;
    let decoding_key = DecodingKey::from_jwk(jwk).context("cle JWK invalide")?;

    let mut validation = Validation::new(algorithm);
    validation.set_issuer(&[issuer]);
    // `jsonwebtoken` valide `aud` des qu'elle est presente dans le token
    // (`validate_aud: true` par defaut) — sans ceci, tout token reel
    // (qui porte toujours un `aud`, contrairement aux tokens de test) se
    // ferait rejeter en `InvalidAudience`. Voir doc de
    // `TrustedIssuer::audience`.
    validation.set_audience(&[audience]);

    let data = decode::<Claims>(token, &decoding_key, &validation)
        .context("signature ou claims JWT invalides")?;
    Ok(data.claims)
}

/// L'algorithme n'est pas toujours annonce explicitement par un JWK (champ
/// `alg` optionnel dans la RFC) : a defaut, on l'infere du type de cle
/// (`kty`), en suivant les correspondances usuelles de la RFC 7518.
fn algorithm_for_jwk(jwk: &Jwk) -> Result<Algorithm> {
    if let Some(key_algorithm) = jwk.common.key_algorithm {
        return match key_algorithm {
            KeyAlgorithm::RS256 => Ok(Algorithm::RS256),
            KeyAlgorithm::RS384 => Ok(Algorithm::RS384),
            KeyAlgorithm::RS512 => Ok(Algorithm::RS512),
            KeyAlgorithm::ES256 => Ok(Algorithm::ES256),
            KeyAlgorithm::ES384 => Ok(Algorithm::ES384),
            KeyAlgorithm::PS256 => Ok(Algorithm::PS256),
            KeyAlgorithm::PS384 => Ok(Algorithm::PS384),
            KeyAlgorithm::PS512 => Ok(Algorithm::PS512),
            KeyAlgorithm::EdDSA => Ok(Algorithm::EdDSA),
            other => anyhow::bail!("algorithme JWK non supporte pour une signature: {other:?}"),
        };
    }
    match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => Ok(Algorithm::RS256),
        AlgorithmParameters::EllipticCurve(ec) => match ec.curve {
            EllipticCurve::P256 => Ok(Algorithm::ES256),
            EllipticCurve::P384 => Ok(Algorithm::ES384),
            ref other => anyhow::bail!("courbe EC non supportee: {other:?}"),
        },
        other => anyhow::bail!("type de cle JWK non supporte: {other:?}"),
    }
}
