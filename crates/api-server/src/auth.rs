//! Validation des JWT entrants contre une liste d'issuers de confiance
//! (JWKS recuperes et caches au demarrage). MVP : pas de refresh dynamique.

pub struct TrustedIssuer {
    pub issuer: String,
    pub jwks_url: String,
}

// TODO: charger la liste des issuers de confiance depuis la config du cluster
// TODO: middleware axum qui valide `Authorization: Bearer <jwt>` et injecte
//       le `sub` (identite) dans les extensions de la requete
