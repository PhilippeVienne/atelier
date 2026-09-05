//! Client HTTP sortant capable de faire confiance a une CA d'entreprise
//! supplementaire (spec `docs/specs/15-souverainete-airgap-inference-gpu.md`
//! §3.2, tache 11.1) — generalise le mecanisme deja en place pour
//! `ATELIER_JWT_CA_PATH` (`crates/api-server/src/auth.rs::fetch_jwks`,
//! introduit avant cette tache pour parler a un fournisseur OIDC
//! auto-heberge derriere une CA privee).
//!
//! Le backend `rustls-tls` de `reqwest` (choisi dans tout ce workspace) ne
//! consulte NI le magasin de confiance du systeme d'exploitation NI
//! `SSL_CERT_FILE` : sans certificat explicitement ajoute, seules les CA
//! publiques standard (`webpki-roots`) sont acceptees. Toute CA
//! supplementaire doit donc etre ajoutee explicitement via
//! `reqwest::Certificate::from_pem` — jamais via `danger_accept_invalid_certs`,
//! qui desactiverait la validation TLS entierement plutot que d'etendre la
//! confiance a une CA precise.

use anyhow::{Context, Result};

/// Construit un `reqwest::ClientBuilder` faisant confiance aux CA publiques
/// standard PLUS, si la variable d'environnement `env_var` est positionnee,
/// au certificat PEM qu'elle designe (chemin de fichier). N'echoue que si la
/// variable est positionnee mais que le fichier est illisible ou ne contient
/// aucun certificat PEM structurellement valide — jamais silencieusement
/// ignore, pour ne pas masquer une configuration operateur invalide derriere
/// un `CERTIFICATE_VERIFY_FAILED` sans rapport plus tard.
///
/// **Piege trouve en verifiant** : `reqwest::Certificate::from_pem` (backend
/// `rustls-tls-native-roots` de ce workspace) ACCEPTE silencieusement un
/// contenu totalement invalide — des octets arbitraires, ou meme un bloc
/// `-----BEGIN CERTIFICATE-----`/`-----END CERTIFICATE-----` dont le corps
/// base64 est corrompu — sans jamais retourner d'erreur, ni au parsing ni a
/// `ClientBuilder::build()`. Un CA d'entreprise mal colle dans les values
/// Helm (tache 11.1) serait donc accepte sans avertissement, pour finir par
/// ne jamais matcher la chaine de certificats reelle au premier appel
/// reseau. On decode donc explicitement le PEM avec `rustls-pemfile` (deja
/// une dependance transitive de `rustls` dans ce workspace) pour s'assurer
/// qu'il contient au moins un certificat DER structurellement valide avant
/// de le transmettre a `reqwest`.
pub fn client_builder_trusting_extra_ca(env_var: &str) -> Result<reqwest::ClientBuilder> {
    let mut builder = reqwest::Client::builder();
    if let Ok(ca_path) = std::env::var(env_var) {
        let pem =
            std::fs::read(&ca_path).with_context(|| format!("lecture de {env_var} ({ca_path})"))?;
        let der_certs = rustls_pemfile::certs(&mut pem.as_slice())
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("{env_var} ({ca_path}) : bloc PEM illisible"))?;
        if der_certs.is_empty() {
            anyhow::bail!(
                "{env_var} ({ca_path}) : aucun certificat PEM valide trouve dans ce fichier"
            );
        }
        let cert = reqwest::Certificate::from_pem(&pem)
            .with_context(|| format!("{env_var} ({ca_path}) : certificat PEM invalide"))?;
        builder = builder.add_root_certificate(cert);
    }
    Ok(builder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `std::env::set_var` est un etat process-global : sequentialise les
    // tests de ce module pour eviter qu'ils ne se marchent dessus en
    // s'executant en parallele (comportement par defaut de `cargo test`).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Certificat auto-signe valide, genere une seule fois via `openssl`
    /// (deja une dependance systeme de ce workspace — voir les Dockerfiles
    /// `ca-certificates`) plutot que fige en dur : garantit un PEM
    /// structurellement correct sans ajouter de dependance de generation de
    /// certificats (`rcgen`) au workspace pour un seul test.
    fn self_signed_pem() -> String {
        let dir = tempfile::tempdir().expect("tempdir");
        let cert_path = dir.path().join("ca.pem");
        let key_path = dir.path().join("ca.key");
        let status = std::process::Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=atelier-test-ca",
                "-keyout",
            ])
            .arg(&key_path)
            .arg("-out")
            .arg(&cert_path)
            .status()
            .expect("lancement d'openssl (requis pour ce test)");
        assert!(status.success(), "openssl a echoue");
        std::fs::read_to_string(&cert_path).expect("lecture du certificat genere")
    }

    #[test]
    fn no_env_var_set_yields_default_builder_that_builds() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ATELIER_TEST_CA_PATH_UNSET");
        let builder = client_builder_trusting_extra_ca("ATELIER_TEST_CA_PATH_UNSET").unwrap();
        builder
            .build()
            .expect("le client par defaut doit se construire");
    }

    #[test]
    fn valid_pem_is_accepted_and_trusted() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ca.pem");
        std::fs::write(&path, self_signed_pem()).unwrap();
        std::env::set_var("ATELIER_TEST_CA_PATH_VALID", &path);

        let builder = client_builder_trusting_extra_ca("ATELIER_TEST_CA_PATH_VALID").unwrap();
        builder
            .build()
            .expect("un PEM valide doit produire un client");

        std::env::remove_var("ATELIER_TEST_CA_PATH_VALID");
    }

    #[test]
    fn garbage_bytes_are_rejected_explicitly() {
        // Regression du piege documente sur `client_builder_trusting_extra_ca` :
        // sans la validation `rustls_pemfile::certs`, `reqwest` acceptait ceci
        // silencieusement (verifie empiriquement avant le correctif).
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-cert.pem");
        std::fs::write(&path, b"ceci n'est pas un certificat").unwrap();
        std::env::set_var("ATELIER_TEST_CA_PATH_GARBAGE", &path);

        let err = client_builder_trusting_extra_ca("ATELIER_TEST_CA_PATH_GARBAGE").expect_err(
            "des octets arbitraires doivent etre rejetes, jamais ignores silencieusement",
        );
        assert!(err.to_string().contains("aucun certificat PEM valide"));

        std::env::remove_var("ATELIER_TEST_CA_PATH_GARBAGE");
    }

    #[test]
    fn pem_block_with_corrupted_body_is_rejected_explicitly() {
        // Meme piege que ci-dessus, mais avec les en-tetes PEM presents et un
        // corps base64 corrompu : `reqwest::Certificate::from_pem` seul
        // acceptait aussi ce cas silencieusement (verifie empiriquement).
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupted.pem");
        std::fs::write(
            &path,
            b"-----BEGIN CERTIFICATE-----\nnotbase64!!!\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        std::env::set_var("ATELIER_TEST_CA_PATH_CORRUPTED", &path);

        let err = client_builder_trusting_extra_ca("ATELIER_TEST_CA_PATH_CORRUPTED")
            .expect_err("un corps PEM corrompu doit etre rejete");
        assert!(err.to_string().contains("bloc PEM illisible"));

        std::env::remove_var("ATELIER_TEST_CA_PATH_CORRUPTED");
    }

    #[test]
    fn missing_file_is_rejected_explicitly() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ATELIER_TEST_CA_PATH_MISSING", "/nonexistent/ca.pem");
        let err = client_builder_trusting_extra_ca("ATELIER_TEST_CA_PATH_MISSING")
            .expect_err("un chemin absent doit etre rejete");
        assert!(err.to_string().contains("lecture de"));
        std::env::remove_var("ATELIER_TEST_CA_PATH_MISSING");
    }
}
