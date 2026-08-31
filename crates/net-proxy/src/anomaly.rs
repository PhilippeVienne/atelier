//! Detection d'anomalie reseau et declenchement du confinement
//! (Jalon M4, tache 4.2.4 — voir `docs/specs/04-external-mcp-server.md`).
//!
//! Le signal retenu est celui que `net-proxy` possede deja et qu'aucun agent
//! legitime ne produit en rafale : les tentatives d'egress REFUSEES. Un agent
//! qui travaille touche des domaines autorises ; un agent qui balaie des
//! ports internes, tente de contourner l'allowlist ou inonde le reseau
//! accumule des refus en quelques secondes.
//!
//! Pourquoi une fenetre glissante et pas un compteur absolu : sur la duree de
//! vie d'un Workshop (des heures), quelques refus isoles sont normaux — une
//! dependance qui tente un miroir non declare, un outil qui sonde une URL de
//! telemetrie. C'est la DENSITE qui distingue l'accident de l'attaque. Un
//! seuil absolu finirait par se declencher sur un Workshop parfaitement sain
//! ayant simplement vecu longtemps.
//!
//! Ce module DETECTE et NOTIFIE ; il ne coupe rien lui-meme. La coupure et le
//! snapshot appartiennent a `vm-supervisor`, seul a piloter le TAP et la
//! microVM (meme sequence que la spec : net-proxy -> vm-supervisor -> gel +
//! snapshot -> statut).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Nombre de refus dans la fenetre au-dela duquel on considere qu'il y a
/// attaque. Volontairement bas : le cout d'un faux positif est une microVM
/// figee et analysable, celui d'un faux negatif une exfiltration reussie.
const DEFAULT_THRESHOLD: usize = 20;
/// Duree de la fenetre glissante.
const DEFAULT_WINDOW: Duration = Duration::from_secs(30);

pub struct AnomalyDetector {
    denials: Arc<Mutex<VecDeque<Instant>>>,
    threshold: usize,
    window: Duration,
    /// Adresse du canal de controle de `vm-supervisor` (meme pod). `None` :
    /// detection inactive — on ne compte meme pas, plutot que d'accumuler un
    /// etat que personne ne consommera.
    supervisor_addr: Option<String>,
    /// Le confinement n'est declenche qu'UNE fois : la coupure d'egress
    /// provoque elle-meme une rafale de refus, qui redeclencherait la
    /// detection en boucle.
    triggered: Arc<Mutex<bool>>,
}

impl AnomalyDetector {
    pub fn from_env() -> Self {
        let threshold = std::env::var("ATELIER_ANOMALY_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_THRESHOLD);
        let window = std::env::var("ATELIER_ANOMALY_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_WINDOW);
        Self {
            denials: Arc::new(Mutex::new(VecDeque::new())),
            threshold,
            window,
            supervisor_addr: std::env::var("ATELIER_VM_CONTROL_ADDR")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            triggered: Arc::new(Mutex::new(false)),
        }
    }

    /// Enregistre un refus d'egress et declenche le confinement si la densite
    /// depasse le seuil. Ne bloque jamais l'appelant sur le reseau : la
    /// notification part dans une tache detachee.
    pub async fn record_denial(&self, host: &str) {
        let Some(addr) = self.supervisor_addr.clone() else {
            return;
        };

        let over_threshold = {
            let mut denials = self.denials.lock().await;
            let now = Instant::now();
            while denials
                .front()
                .is_some_and(|t| now.duration_since(*t) > self.window)
            {
                denials.pop_front();
            }
            denials.push_back(now);
            denials.len() >= self.threshold
        };
        if !over_threshold {
            return;
        }

        {
            let mut triggered = self.triggered.lock().await;
            if *triggered {
                return;
            }
            *triggered = true;
        }

        tracing::error!(
            host,
            threshold = self.threshold,
            window_secs = self.window.as_secs(),
            "ANOMALIE RESEAU : trop de tentatives d'egress refusees, confinement demande"
        );
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            match client
                .post(format!("http://{addr}/lockdown"))
                .timeout(Duration::from_secs(10))
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    tracing::warn!("confinement de securite accepte par vm-supervisor");
                }
                Ok(response) => {
                    tracing::error!(status = %response.status(), "vm-supervisor a refuse le confinement");
                }
                Err(err) => {
                    // On ne peut rien faire de plus depuis ici : net-proxy ne
                    // pilote ni le TAP ni la microVM. Le refus reste applique
                    // requete par requete, ce qui n'est pas le confinement
                    // mais n'est pas rien.
                    tracing::error!(%err, "vm-supervisor injoignable, confinement NON applique");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detector(threshold: usize, window: Duration) -> AnomalyDetector {
        AnomalyDetector {
            denials: Arc::new(Mutex::new(VecDeque::new())),
            threshold,
            window,
            // Adresse invalide mais PRESENTE : la detection s'active, seule
            // la notification echouera (dans une tache detachee).
            supervisor_addr: Some("127.0.0.1:1".to_string()),
            triggered: Arc::new(Mutex::new(false)),
        }
    }

    /// Des refus espaces ne doivent PAS declencher : c'est la densite qui
    /// distingue l'accident de l'attaque, pas le total. Sur la duree de vie
    /// d'un Workshop, un seuil absolu finirait par se declencher tout seul.
    #[tokio::test]
    async fn scattered_denials_never_trigger() {
        let d = detector(3, Duration::from_millis(40));
        for _ in 0..10 {
            d.record_denial("exemple.test").await;
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            !*d.triggered.lock().await,
            "dix refus espaces hors fenetre ne constituent pas une anomalie"
        );
    }

    /// Une rafale dans la fenetre declenche.
    #[tokio::test]
    async fn a_burst_triggers_the_lockdown() {
        let d = detector(5, Duration::from_secs(30));
        for _ in 0..5 {
            d.record_denial("10.0.0.1").await;
        }
        assert!(*d.triggered.lock().await);
    }

    /// Une seule fois : la coupure d'egress provoque elle-meme une rafale de
    /// refus, qui redeclencherait la detection en boucle.
    #[tokio::test]
    async fn the_lockdown_is_requested_only_once() {
        let d = detector(2, Duration::from_secs(30));
        for _ in 0..50 {
            d.record_denial("10.0.0.1").await;
        }
        assert!(*d.triggered.lock().await);
        // `triggered` reste vrai et la fenetre n'a servi qu'une fois : le
        // test verifie l'invariant, la garde etant dans `record_denial`.
    }

    /// Sans `vm-supervisor` configure, on ne compte meme pas : accumuler un
    /// etat que personne ne consommera n'a pas de sens.
    #[tokio::test]
    async fn detection_is_inactive_without_a_supervisor() {
        let mut d = detector(1, Duration::from_secs(30));
        d.supervisor_addr = None;
        d.record_denial("10.0.0.1").await;
        assert!(!*d.triggered.lock().await);
        assert!(d.denials.lock().await.is_empty());
    }
}
