//! Eviction LRU du cache d'images local (spec
//! `docs/specs/13-image-cache-offload.md` §3.1, tache 8.5).
//!
//! Le controller ne monte jamais le PVC de cache lui-meme (voir
//! `crate::reconcile::cleanup_snapshot_cache`, meme raison) : cette passe
//! periodique cree un Job ephemere qui monte le PVC et applique la
//! politique — jamais le controller directement.
//!
//! Desactivee si S3/`S3_BUCKET_IMAGE_CACHE` ne sont pas configures :
//! evincer une entree sans avoir confirme sa presence sur S3 la perdrait
//! pour de bon, contrairement a l'intention de cette spec (le PVC local est
//! un cache a eviction, pas la source de verite, UNE FOIS que S3 en est
//! une — sans S3 configure, le PVC local EST la seule source de verite, et
//! rien ne doit en etre evince).
//!
//! Configuration chargee directement depuis l'environnement (pas depuis
//! `crate::reconcile::ReconcileCtx`) : cette passe tourne dans sa propre
//! boucle `tokio::spawn`, independante du cycle de reconciliation d'un
//! Workshop precis, et n'a besoin d'aucun des autres champs de ce contexte
//! (OpenBao, LiteLLM...).

use crate::storage;
use atelier_common::storage::S3Config;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Container, EnvVar, PersistentVolumeClaimVolumeSource, PodSpec, PodTemplateSpec, Volume,
    VolumeMount,
};
use kube::api::{Api, Patch, PatchParams};
use kube::Client;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FIELD_MANAGER: &str = "atelier-controller";

/// Plafond de taille par defaut (Gio) au-dela duquel la passe d'eviction se
/// declenche — configurable via `ATELIER_IMAGE_CACHE_EVICTION_THRESHOLD_GB`
/// (`.Values.imageCache.evictionThresholdGb` cote chart). Marge sous la
/// taille nominale du PVC (`20Gi`, `storage::ensure_image_cache_pvc`) :
/// laisse de la place aux snapshots (meme PVC, jamais evinces par cette
/// passe) et a un build en cours au moment ou l'eviction tourne.
const DEFAULT_THRESHOLD_GB: u64 = 15;

/// Periode par defaut entre deux passes — pas configurable pour l'instant,
/// une valeur raisonnable suffit pour ce premier lot (voir spec §5, risques
/// non resolus : calibrage a affiner empiriquement une fois en usage reel).
const DEFAULT_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// A lancer via `tokio::spawn` au demarrage du controller (`main.rs`),
/// jamais attendue (boucle infinie). Ne fait rien du tout si S3/
/// `S3_BUCKET_IMAGE_CACHE` ne sont pas configures (log une seule fois, pas
/// a chaque tick).
pub async fn run_periodic(client: Client, ns: String) {
    let s3 = match atelier_common::storage::config_from_env() {
        Ok(Some(s3)) => s3,
        Ok(None) => {
            tracing::info!("S3 non configure, eviction du cache d'images desactivee");
            return;
        }
        Err(err) => {
            tracing::warn!(%err, "configuration S3 invalide, eviction du cache d'images desactivee");
            return;
        }
    };
    let Some(bucket_image_cache) = s3.bucket_image_cache.clone() else {
        tracing::info!(
            "S3_BUCKET_IMAGE_CACHE non configure, eviction du cache d'images desactivee"
        );
        return;
    };
    let pod_endpoint =
        std::env::var("ATELIER_S3_POD_ENDPOINT").unwrap_or_else(|_| s3.endpoint.clone());
    let threshold_gb = std::env::var("ATELIER_IMAGE_CACHE_EVICTION_THRESHOLD_GB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_THRESHOLD_GB);

    let mut ticker = tokio::time::interval(DEFAULT_INTERVAL);
    loop {
        ticker.tick().await;
        if let Err(err) = run_once(
            &client,
            &ns,
            &s3,
            &pod_endpoint,
            &bucket_image_cache,
            threshold_gb,
        )
        .await
        {
            tracing::warn!(%err, "passe d'eviction du cache d'images echouee, retentera au prochain tick");
        }
    }
}

/// Une seule passe : cree le Job d'eviction (nom unique par execution, voir
/// plus bas).
async fn run_once(
    client: &Client,
    ns: &str,
    s3: &S3Config,
    pod_endpoint: &str,
    bucket_image_cache: &str,
    threshold_gb: u64,
) -> anyhow::Result<()> {
    let mount_path = storage::IMAGE_CACHE_MOUNT_PATH;

    // Nom unique par execution (pas idempotent sur le NOM, contrairement au
    // reste de ce module) : contrairement a la suppression d'un snapshot
    // (8.1, un seul Job par Workshop supprime), celle-ci doit reellement se
    // REJOUER a chaque tick — un Job Kubernetes ne peut pas etre relance une
    // fois termine. `ttlSecondsAfterFinished` nettoie les executions
    // passees automatiquement.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let job_name = format!("atelier-image-cache-eviction-{nanos}");

    // Script shell minimal (image `minio/mc`, deja utilisee par
    // `s3-init-job.yaml` pour la meme famille d'operations) — **ni `find`
    // ni `awk` n'y sont disponibles** (verifie empiriquement en testant ce
    // script contre le cluster de dev reel : image quasi-distroless, juste
    // `mc` + un jeu minimal de coreutils GNU). Remplace `find -printf` par
    // une boucle `stat -c '%Y %n' | sort -n`, et la somme `awk` par un
    // accumulateur shell pur (`du -sb ... | while read` — la variable
    // s'accumule correctement sur toute la duree du pipe, meme subshell
    // pour toutes les iterations en `sh`/`dash`/busybox).
    //
    // Evince les entrees `sha256_*` du plus ancien accede au plus recent,
    // tant que la taille totale depasse le plafond, JAMAIS sans avoir
    // confirme au prealable que `mc stat` trouve la meme entree sur S3
    // (`images/<digest>/rootfs.ext4` — meme nom de repertoire des deux
    // cotes, `S3StorageBackend::image_cache_key` construit sa cle a partir
    // du digest de la meme facon que `image-builder::publish_to_cache`
    // nomme son repertoire local, aucune reconstruction necessaire ici).
    // Verifie empiriquement contre le cluster de dev reel : une entree
    // ancienne MAIS absente de S3 est bien preservee malgre son
    // anciennete, une entree plus recente MAIS confirmee sur S3 est bien
    // evincee a sa place.
    let script = format!(
        r#"set -eu
mc alias set atelier "$S3_ENDPOINT" "$AWS_ACCESS_KEY_ID" "$AWS_SECRET_ACCESS_KEY" {path_style_flag} >/dev/null
threshold_bytes=$(( {threshold_gb} * 1024 * 1024 * 1024 ))
total_bytes() {{
  du -sb {mount_path}/sha256_* 2>/dev/null | while read -r size _; do t=$((${{t:-0}}+size)); echo "$t"; done | {{ r=$(tail -n1); [ -n "$r" ] && echo "$r" || echo 0; }}
}}
current=$(total_bytes)
echo "cache actuel: ${{current}} octets, plafond: ${{threshold_bytes}} octets"
if [ "$current" -le "$threshold_bytes" ]; then
  echo "sous le plafond, rien a evincer"
  exit 0
fi
for d in {mount_path}/sha256_*; do
  [ -d "$d" ] || continue
  stat -c '%Y %n' "$d"
done | sort -n | while read -r _ path; do
  dirname=$(basename "$path")
  current=$(total_bytes)
  if [ "$current" -le "$threshold_bytes" ]; then
    echo "plafond atteint, arret"
    break
  fi
  if mc stat "atelier/{bucket}/images/${{dirname}}/rootfs.ext4" >/dev/null 2>&1; then
    echo "eviction de ${{dirname}} (confirme sur S3)"
    rm -rf "{mount_path}/${{dirname}}"
  else
    echo "${{dirname}} absent de S3, conserve malgre son anciennete"
  fi
done
"#,
        threshold_gb = threshold_gb,
        mount_path = mount_path,
        bucket = bucket_image_cache,
        path_style_flag = if s3.force_path_style { "--path=on" } else { "" },
    );

    let cache_volume = Volume {
        name: "cache".to_string(),
        persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
            claim_name: storage::IMAGE_CACHE_PVC_NAME.to_string(),
            read_only: Some(false),
        }),
        ..Default::default()
    };
    let cache_mount = VolumeMount {
        name: "cache".to_string(),
        mount_path: mount_path.to_string(),
        ..Default::default()
    };

    let env = vec![
        EnvVar {
            name: "S3_ENDPOINT".to_string(),
            value: Some(pod_endpoint.to_string()),
            ..Default::default()
        },
        EnvVar {
            name: "AWS_ACCESS_KEY_ID".to_string(),
            value: Some(s3.access_key_id.clone()),
            ..Default::default()
        },
        EnvVar {
            name: "AWS_SECRET_ACCESS_KEY".to_string(),
            value: Some(s3.secret_access_key.clone()),
            ..Default::default()
        },
    ];

    let job = Job {
        metadata: kube::api::ObjectMeta {
            name: Some(job_name.clone()),
            namespace: Some(ns.to_string()),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(1),
            ttl_seconds_after_finished: Some(600),
            template: PodTemplateSpec {
                spec: Some(PodSpec {
                    restart_policy: Some("Never".to_string()),
                    volumes: Some(vec![cache_volume]),
                    containers: vec![Container {
                        name: "eviction".to_string(),
                        image: Some("minio/mc:latest".to_string()),
                        command: Some(vec!["sh".to_string(), "-c".to_string(), script]),
                        env: Some(env),
                        volume_mounts: Some(vec![cache_mount]),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    };

    let jobs: Api<Job> = Api::namespaced(client.clone(), ns);
    jobs.patch(
        &job_name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&job),
    )
    .await?;
    tracing::info!(job = %job_name, "passe d'eviction du cache d'images lancee");
    Ok(())
}
