//! Jeton ephemere `X-Atelier-Squad-Token` (spec docs/specs/16-escouades-
//! multi-agents-swarms-mesh.md §3.2, tache 12.2) : authentification
//! applicative d'une connexion inter-Workshops, EN PLUS (jamais a la place)
//! de la `NetworkPolicy` par campagne (tache 12.1) — celle-ci ne fait que
//! restreindre QUELS PODS peuvent etablir une connexion TCP, jamais QUI
//! l'agent croit parler a l'autre bout. Un pod compromis mais toujours dans
//! la meme campagne ne peut pas forger de jeton valide sans la cle derivee.
//!
//! **Correction par rapport a la premiere redaction de la spec** : pas un
//! en-tete HTTP injecte par `identity-proxy` — les services exportes
//! (`Workshop.spec.exported_services`) ne sont pas necessairement HTTP (un
//! port applicatif quelconque). Le jeton est donc une PREMIERE LIGNE
//! textuelle envoyee avant les octets relayes (`crate::net-proxy::ingress`),
//! valide pour n'importe quel protocole applicatif — meme principe qu'un
//! prefixe de trame, pas une semantique HTTP.
//!
//! La cle de signature n'est JAMAIS transmise sur le reseau : chaque
//! Workshop d'une campagne recoit la MEME cle derivee (`derive_campaign_key`,
//! HMAC-SHA256 d'un secret de signature connu seulement du controller, jamais
//! du CRD `Workshop` lui-meme) via une variable d'environnement posee par le
//! controller (`ATELIER_SQUAD_TOKEN_KEY`) — un Workshop hors de la campagne
//! ne recoit jamais cette cle, meme s'il connait le `campaign_id` (qui,
//! lui, est un champ public du CRD).

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

/// TTL par defaut d'un jeton (spec 16 §3.2 : "TTL court, 15 min") — assez
/// long pour couvrir l'etablissement d'une connexion meme sous forte charge,
/// assez court pour qu'un jeton intercepte perde rapidement toute valeur.
pub const DEFAULT_TTL_SECS: u64 = 15 * 60;

/// Derive la cle partagee d'une campagne a partir du secret de signature
/// GLOBAL du controller (`ATELIER_SQUAD_TOKEN_SIGNING_KEY`, jamais transmis
/// tel quel a un Workshop) et de son `campaign_id`. HMAC-SHA256, encodee en
/// hexadecimal — jamais le secret de signature lui-meme : meme si la cle
/// derivee d'UNE campagne fuitait, elle ne donnerait aucune prise sur les
/// AUTRES campagnes (contrairement a un secret global partage tel quel).
pub fn derive_campaign_key(signing_key: &str, campaign_id: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(signing_key.as_bytes())
        .expect("HMAC accepte toute longueur de cle");
    mac.update(campaign_id.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Emet un jeton pour `workshop_name`, valide `ttl` secondes a partir de
/// maintenant. Format `<workshop_name>|<expiry_unix>|<hex_hmac>` — pas de
/// structure JWT complete (pas de besoin d'algorithmes negociables ni de
/// claims arbitraires ici, un format minimal reduit la surface de parsing).
pub fn mint(campaign_key: &str, workshop_name: &str, ttl_secs: u64) -> String {
    let expiry = now_unix() + ttl_secs;
    let signature = sign(campaign_key, workshop_name, expiry);
    format!("{workshop_name}|{expiry}|{signature}")
}

/// Verifie un jeton emis par [`mint`] : renvoie le nom du Workshop emetteur
/// si la signature est valide ET que le jeton n'est pas expire, une raison
/// d'echec textuelle sinon (jamais de panique sur une entree malveillante).
pub fn verify(campaign_key: &str, token: &str) -> Result<String, String> {
    let mut parts = token.splitn(3, '|');
    let (Some(workshop_name), Some(expiry_str), Some(signature)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return Err("format de jeton invalide, attendu nom|expiration|signature".to_string());
    };
    let expiry: u64 = expiry_str
        .parse()
        .map_err(|_| "expiration non numerique dans le jeton".to_string())?;

    let expected = sign(campaign_key, workshop_name, expiry);
    // Comparaison en temps constant : une comparaison octet-a-octet
    // naive fuiterait la position du premier octet different par le
    // temps d'execution (timing attack classique sur une verification de
    // signature).
    if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
        return Err("signature invalide".to_string());
    }
    if now_unix() > expiry {
        return Err("jeton expire".to_string());
    }
    Ok(workshop_name.to_string())
}

fn sign(campaign_key: &str, workshop_name: &str, expiry: u64) -> String {
    let mut mac = HmacSha256::new_from_slice(campaign_key.as_bytes())
        .expect("HMAC accepte toute longueur de cle");
    mac.update(format!("{workshop_name}|{expiry}").as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("horloge systeme anterieure a epoch")
        .as_secs()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_freshly_minted_token_verifies_and_returns_the_workshop_name() {
        let key = derive_campaign_key("global-signing-secret", "campaign-42");
        let token = mint(&key, "ws-backend", DEFAULT_TTL_SECS);
        assert_eq!(verify(&key, &token), Ok("ws-backend".to_string()));
    }

    #[test]
    fn different_campaigns_derive_different_keys() {
        let key_a = derive_campaign_key("global-signing-secret", "campaign-a");
        let key_b = derive_campaign_key("global-signing-secret", "campaign-b");
        assert_ne!(key_a, key_b);

        let token = mint(&key_a, "ws-backend", DEFAULT_TTL_SECS);
        assert!(
            verify(&key_b, &token).is_err(),
            "un jeton d'une campagne ne doit jamais verifier sous la cle d'une autre"
        );
    }

    #[test]
    fn an_expired_token_is_rejected() {
        let key = derive_campaign_key("global-signing-secret", "campaign-42");
        // TTL de 0s : deja expire au moment de la verification (l'horloge a
        // forcement avance d'au moins quelques microsecondes).
        let token = mint(&key, "ws-backend", 0);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert_eq!(verify(&key, &token), Err("jeton expire".to_string()));
    }

    #[test]
    fn a_tampered_workshop_name_is_rejected() {
        let key = derive_campaign_key("global-signing-secret", "campaign-42");
        let token = mint(&key, "ws-backend", DEFAULT_TTL_SECS);
        // Substitue le nom mais garde expiration+signature d'origine : la
        // signature ne correspond plus au nouveau nom.
        let mut parts = token.splitn(3, '|');
        let _original_name = parts.next().unwrap();
        let rest: Vec<&str> = parts.collect();
        let tampered = format!("ws-attacker|{}|{}", rest[0], rest[1]);
        assert!(verify(&key, &tampered).is_err());
    }

    #[test]
    fn malformed_tokens_are_rejected_without_panicking() {
        let key = derive_campaign_key("global-signing-secret", "campaign-42");
        assert!(verify(&key, "").is_err());
        assert!(verify(&key, "not-enough-parts").is_err());
        assert!(verify(&key, "a|not-a-number|deadbeef").is_err());
    }
}
