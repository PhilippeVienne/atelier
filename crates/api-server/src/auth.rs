//! Validation des JWT entrants. L'issuer de confiance est
//! [Kanidm](https://kanidm.com/), qui joue le role de fournisseur d'identite
//! pour Atelier (utilisateurs humains proprietaires de Workshops) et peut
//! lui-meme federer vers un provider externe (OIDC/LDAP) sans que
//! l'api-server ait a en connaitre les details : il ne parle qu'a l'issuer
//! Kanidm. JWKS recuperes et caches au demarrage ; MVP sans refresh dynamique.

pub struct TrustedIssuer {
    pub issuer: String,
    pub jwks_url: String,
}

// TODO: charger l'URL de l'instance Kanidm (issuer + JWKS) depuis la config
//       du cluster
// TODO: middleware axum qui valide `Authorization: Bearer <jwt>` et injecte
//       le `sub` (identite) dans les extensions de la requete
