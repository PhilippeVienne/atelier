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
    /// Valeur exacte attendue dans la claim `aud` (`ATELIER_JWT_AUDIENCE`,
    /// typiquement le `client_id` de l'OAuth2 Resource Server Atelier cote
    /// Kanidm). Requise, pas optionnelle : `jsonwebtoken` valide `aud` des
    /// qu'elle est presente dans le token (`validate_aud: true` par
    /// defaut) — un vrai token Kanidm en porte toujours une. Sans
    /// audience configuree ici, `Validation::aud` resterait `None` et
    /// **tout** token reel serait rejete (`InvalidAudience`), constate en
    /// pratique en testant contre un vrai flux OAuth2 (voir
    /// docs/PROGRESS.md) — pas visible avec des tokens de test qui
    /// omettent simplement la claim `aud`.
    pub audience: String,
    /// Chemin vers un certificat CA supplementaire a faire confiance pour
    /// recuperer le JWKS (`ATELIER_JWT_CA_PATH`) : necessaire pour un
    /// Kanidm auto-heberge avec une CA privee (dev y compris — certificat
    /// auto-signe). `reqwest` (backend `rustls-tls` de ce workspace) ne
    /// consulte ni le magasin de confiance du systeme, ni `SSL_CERT_FILE` ;
    /// sans ce chemin explicite, seules les CA publiques standard
    /// (webpki-roots) sont acceptees.
    pub ca_path: Option<String>,
}

impl TrustedIssuer {
    /// `ATELIER_JWT_ISSUER` : valeur exacte attendue dans la claim `iss` des
    /// JWT presentes. `ATELIER_JWT_JWKS_URL` : URL du jeu de cles publiques
    /// Kanidm (endpoint JWKS de l'OAuth2 Resource Server configure pour
    /// Atelier). `ATELIER_JWT_AUDIENCE` : `client_id` attendu dans `aud`
    /// (voir doc du champ `audience`). `Ok(None)` si `ATELIER_JWT_ISSUER`
    /// est absent : l'auth est alors desactivee (toutes les requetes
    /// refusees par le middleware, utile seulement pour demarrer le binaire
    /// en dev sans Kanidm).
    pub fn from_env() -> Result<Option<Self>> {
        let Ok(issuer) = std::env::var("ATELIER_JWT_ISSUER") else {
            return Ok(None);
        };
        let jwks_url = std::env::var("ATELIER_JWT_JWKS_URL")
            .context("ATELIER_JWT_ISSUER est defini mais ATELIER_JWT_JWKS_URL est absent")?;
        let audience = std::env::var("ATELIER_JWT_AUDIENCE")
            .context("ATELIER_JWT_ISSUER est defini mais ATELIER_JWT_AUDIENCE est absent")?;
        let ca_path = std::env::var("ATELIER_JWT_CA_PATH").ok();
        Ok(Some(Self { issuer, jwks_url, audience, ca_path }))
    }

    pub async fn fetch_jwks(&self) -> Result<JwkSet> {
        let mut builder = reqwest::Client::builder();
        if let Some(ca_path) = &self.ca_path {
            let pem = std::fs::read(ca_path)
                .with_context(|| format!("lecture de ATELIER_JWT_CA_PATH ({ca_path})"))?;
            let cert = reqwest::Certificate::from_pem(&pem).context("certificat CA invalide (PEM attendu)")?;
            builder = builder.add_root_certificate(cert);
        }
        let client = builder.build().context("construction du client HTTP pour le JWKS")?;

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

/// Etat partage du middleware d'authentification. `None` (pas de Kanidm
/// configure) fait refuser systematiquement toute requete authentifiee :
/// evite de demarrer silencieusement en mode "tout est autorise".
pub enum AuthState {
    Configured { issuer: String, audience: String, jwks: JwkSet },
    Disabled,
}

impl AuthState {
    pub async fn from_env() -> Result<Self> {
        match TrustedIssuer::from_env()? {
            Some(trusted) => {
                let jwks = trusted.fetch_jwks().await?;
                tracing::info!(issuer = %trusted.issuer, audience = %trusted.audience, keys = jwks.keys.len(), "JWKS Kanidm charge");
                Ok(AuthState::Configured { issuer: trusted.issuer, audience: trusted.audience, jwks })
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
    let AuthState::Configured { issuer, audience, jwks } = auth.as_ref() else {
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

    match validate_token(token, issuer, audience, jwks) {
        Ok(sub) => {
            req.extensions_mut().insert(AuthenticatedUser(sub));
            next.run(req).await
        }
        Err(err) => {
            tracing::warn!(?err, "JWT refuse");
            (StatusCode::UNAUTHORIZED, "JWT invalide").into_response()
        }
    }
}

fn validate_token(token: &str, issuer: &str, audience: &str, jwks: &JwkSet) -> Result<String> {
    let header = decode_header(token).context("en-tete JWT invalide")?;
    let kid = header.kid.context("JWT sans champ kid, impossible de choisir la cle")?;
    let jwk = jwks.find(&kid).context("kid absent du JWKS Kanidm charge au demarrage")?;

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
