//! Validation des JWT entrants. L'issuer de confiance est
//! [Kanidm](https://kanidm.com/), qui joue le role de fournisseur d'identite
//! pour Atelier (utilisateurs humains proprietaires de Workshops) et peut
//! lui-meme federer vers un provider externe (OIDC/LDAP) sans que
//! l'api-server ait a en connaitre les details : il ne parle qu'a l'issuer
//! Kanidm. JWKS recuperes et caches au demarrage ; MVP sans refresh
//! dynamique (un changement de cles cote Kanidm necessite un redemarrage).

use anyhow::{Context, Result};
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, Jwk, JwkSet, KeyAlgorithm};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct TrustedIssuer {
    pub issuer: String,
    pub jwks_url: String,
}

impl TrustedIssuer {
    /// `ATELIER_JWT_ISSUER` : valeur exacte attendue dans la claim `iss` des
    /// JWT presentes. `ATELIER_JWT_JWKS_URL` : URL du jeu de cles publiques
    /// Kanidm (endpoint JWKS de l'OAuth2 Resource Server configure pour
    /// Atelier). `Ok(None)` si `ATELIER_JWT_ISSUER` est absent : l'auth est
    /// alors desactivee (toutes les requetes refusees par le middleware,
    /// utile seulement pour demarrer le binaire en dev sans Kanidm).
    pub fn from_env() -> Result<Option<Self>> {
        let Ok(issuer) = std::env::var("ATELIER_JWT_ISSUER") else {
            return Ok(None);
        };
        let jwks_url = std::env::var("ATELIER_JWT_JWKS_URL")
            .context("ATELIER_JWT_ISSUER est defini mais ATELIER_JWT_JWKS_URL est absent")?;
        Ok(Some(Self { issuer, jwks_url }))
    }

    pub async fn fetch_jwks(&self) -> Result<JwkSet> {
        reqwest::get(&self.jwks_url)
            .await
            .context("requete de recuperation du JWKS")?
            .error_for_status()
            .context("le JWKS n'a pas pu etre recupere")?
            .json()
            .await
            .context("reponse JWKS invalide (JSON attendu)")
    }
}

/// Etat partage du middleware d'authentification. `None` (pas de Kanidm
/// configure) fait refuser systematiquement toute requete authentifiee :
/// evite de demarrer silencieusement en mode "tout est autorise".
pub enum AuthState {
    Configured { issuer: String, jwks: JwkSet },
    Disabled,
}

impl AuthState {
    pub async fn from_env() -> Result<Self> {
        match TrustedIssuer::from_env()? {
            Some(trusted) => {
                let jwks = trusted.fetch_jwks().await?;
                tracing::info!(issuer = %trusted.issuer, keys = jwks.keys.len(), "JWKS Kanidm charge");
                Ok(AuthState::Configured { issuer: trusted.issuer, jwks })
            }
            None => {
                tracing::warn!(
                    "ATELIER_JWT_ISSUER absent : authentification desactivee, toutes les requetes protegees seront refusees"
                );
                Ok(AuthState::Disabled)
            }
        }
    }
}

/// Identite du sujet JWT authentifie, injectee dans les extensions de la
/// requete par [`require_auth`]. C'est ce sujet — jamais une valeur fournie
/// par le client dans le corps de la requete — qui devient
/// `WorkshopSpec.owner_subject` (voir `routes::create_workshop`) : un
/// client ne peut donc jamais creer ni manipuler un Workshop au nom de
/// quelqu'un d'autre.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser(pub String);

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
}

pub async fn require_auth(State(auth): State<Arc<AuthState>>, mut req: Request, next: Next) -> Response {
    let AuthState::Configured { issuer, jwks } = auth.as_ref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "authentification non configuree").into_response();
    };

    let Some(token) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return (StatusCode::UNAUTHORIZED, "en-tete Authorization: Bearer <jwt> requis").into_response();
    };

    match validate_token(token, issuer, jwks) {
        Ok(sub) => {
            req.extensions_mut().insert(AuthenticatedUser(sub));
            next.run(req).await
        }
        Err(err) => {
            tracing::warn!(%err, "JWT refuse");
            (StatusCode::UNAUTHORIZED, "JWT invalide").into_response()
        }
    }
}

fn validate_token(token: &str, issuer: &str, jwks: &JwkSet) -> Result<String> {
    let header = decode_header(token).context("en-tete JWT invalide")?;
    let kid = header.kid.context("JWT sans champ kid, impossible de choisir la cle")?;
    let jwk = jwks.find(&kid).context("kid absent du JWKS Kanidm charge au demarrage")?;

    let algorithm = algorithm_for_jwk(jwk)?;
    let decoding_key = DecodingKey::from_jwk(jwk).context("cle JWK invalide")?;

    let mut validation = Validation::new(algorithm);
    validation.set_issuer(&[issuer]);

    let data = decode::<Claims>(token, &decoding_key, &validation)
        .context("signature ou claims JWT invalides")?;
    Ok(data.claims.sub)
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
