//! Stockage S3 hybride : archive les sessions terminal/VS Code volumineuses
//! (`atelier-api-server`) et, a terme, le cache d'images/snapshots
//! Firecracker (`image-builder`/`vm-supervisor`, spec
//! docs/specs/13-image-cache-offload.md) vers un bucket compatible S3
//! (RustFS en dev, n'importe quel service compatible en production — voir
//! le principe transversal de substitutabilite,
//! `docs/specs/00-architecture-principles-substitutability.md`).
//!
//! Deplace ici depuis `crates/api-server/src/storage.rs` (tache 8.2) pour
//! etre partage par plusieurs crates plutot que par la seule `api-server` :
//! meme raison que `atelier_common::telemetry`, deja partage de la meme
//! facon.
//!
//! Le trait [`StorageBackend`] est deliberement generique (bucket/cle en
//! parametres, flux en entree/sortie) pour permettre une implementation
//! alternative future sans toucher au reste du code appelant.
//! [`S3StorageBackend`] est la seule implementation a ce jour, au-dessus
//! d'`aws-sdk-s3`.
//!
//! Convention de cle S3 pour les archives de session :
//! `workshops/<workshop_name>/sessions/<session_id>.zst` (voir
//! [`session_archive_key`]) — un prefixe par Workshop permet un listing/
//! nettoyage cible sans avoir a connaitre a l'avance tous les
//! `session_id`.
//!
//! Compression : `zstd` en streaming via la crate `async-compression`
//! (`ZstdEncoder`/`ZstdDecoder` operant sur `AsyncRead`), choisie plutot que
//! la crate `zstd` nue pour eviter de gerer nous-memes un thread bloquant
//! autour de libzstd — le flux source (typiquement une session
//! terminal/VS Code en cours de rejeu depuis une WebSocket) ne doit jamais
//! etre charge entierement en memoire.

use std::pin::Pin;

use anyhow::{Context, Result};
use async_compression::tokio::bufread::{ZstdDecoder, ZstdEncoder};
use aws_sdk_s3::config::{BehaviorVersion, Builder as S3ConfigBuilder, Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_sdk_s3::Client as S3Client;
use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, BufReader};

/// Un flux d'octets asynchrone, possede et type-efface : c'est le type
/// commun utilise par [`StorageBackend`] en entree comme en sortie, pour
/// rester agnostique du type concret du flux source/destination (une
/// WebSocket de `crate::terminal`/`crate::vscode`, un fichier temporaire,
/// un test...).
pub type BoxedAsyncRead = Pin<Box<dyn AsyncRead + Send + Sync>>;

/// Backend de stockage objet, substituable (voir principe transversal de
/// substitutabilite du projet). `S3StorageBackend` est l'implementation de
/// reference au-dessus d'`aws-sdk-s3`, mais toute implementation
/// alternative (autre SDK, autre protocole) peut se glisser derriere ce
/// meme trait sans impact sur les appelants.
#[async_trait::async_trait]
pub trait StorageBackend: Send + Sync {
    /// Televerse `stream` vers `bucket`/`key`. N'importe quelle
    /// transformation (compression, chiffrement...) doit avoir deja ete
    /// appliquee au flux avant l'appel : ce trait ne connait que des
    /// octets bruts.
    async fn upload_stream(&self, bucket: &str, key: &str, stream: BoxedAsyncRead) -> Result<()>;

    /// Recupere `bucket`/`key` sous forme d'un flux consommable
    /// progressivement (pas de chargement integral en memoire).
    async fn download_stream(&self, bucket: &str, key: &str) -> Result<BoxedAsyncRead>;

    /// Supprime `bucket`/`key`.
    async fn delete_object(&self, bucket: &str, key: &str) -> Result<()>;
}

/// Configuration S3 chargee depuis l'environnement (voir [`config_from_env`]).
#[derive(Debug, Clone)]
pub struct S3Config {
    pub endpoint: String,
    pub region: String,
    pub bucket_sessions: String,
    pub bucket_snapshots: String,
    pub force_path_style: bool,
    pub access_key_id: String,
    pub secret_access_key: String,
}

/// Charge la configuration S3 depuis l'environnement, selon la meme
/// convention que les autres configurations optionnelles du projet (voir
/// `crates/controller/src/openbao.rs::config_from_env` et
/// `crates/api-server/src/auth.rs::TrustedIssuer::from_env`) : `S3_ENDPOINT`
/// agit comme interrupteur principal — absent, le stockage S3 est
/// simplement desactive (`Ok(None)`) ; present, toutes les autres variables
/// deviennent obligatoires et une absence produit une erreur explicite
/// plutot qu'un comportement degrade silencieux.
///
/// Les identifiants (`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`) sont lus
/// explicitement ici plutot que via la chaine de decouverte standard
/// d'`aws-config` (IMDS, profils `~/.aws/...`) : ces mecanismes ne
/// s'appliquent pas a un endpoint S3 personnalise comme RustFS/MinIO.
pub fn config_from_env() -> Result<Option<S3Config>> {
    let Ok(endpoint) = std::env::var("S3_ENDPOINT") else {
        tracing::info!("S3_ENDPOINT absent, stockage S3 desactive");
        return Ok(None);
    };
    let region =
        std::env::var("S3_REGION").context("S3_ENDPOINT est defini mais S3_REGION est absent")?;
    let bucket_sessions = std::env::var("S3_BUCKET_SESSIONS")
        .context("S3_ENDPOINT est defini mais S3_BUCKET_SESSIONS est absent")?;
    let bucket_snapshots = std::env::var("S3_BUCKET_SNAPSHOTS")
        .context("S3_ENDPOINT est defini mais S3_BUCKET_SNAPSHOTS est absent")?;
    let access_key_id = std::env::var("AWS_ACCESS_KEY_ID")
        .context("S3_ENDPOINT est defini mais AWS_ACCESS_KEY_ID est absent")?;
    let secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY")
        .context("S3_ENDPOINT est defini mais AWS_SECRET_ACCESS_KEY est absent")?;
    let force_path_style = match std::env::var("S3_FORCE_PATH_STYLE") {
        Ok(raw) => raw
            .parse::<bool>()
            .context("S3_FORCE_PATH_STYLE doit valoir \"true\" ou \"false\"")?,
        Err(_) => false,
    };
    Ok(Some(S3Config {
        endpoint,
        region,
        bucket_sessions,
        bucket_snapshots,
        force_path_style,
        access_key_id,
        secret_access_key,
    }))
}

/// Implementation de [`StorageBackend`] au-dessus d'`aws-sdk-s3`, adaptee a
/// tout service compatible S3 avec un endpoint personnalise (RustFS/MinIO
/// en dev, potentiellement un autre fournisseur en production).
#[derive(Clone)]
pub struct S3StorageBackend {
    client: S3Client,
    bucket_sessions: String,
    #[allow(dead_code)]
    // branche par la tache 8.4 (docs/specs/13-image-cache-offload.md), pas encore
    bucket_snapshots: String,
}

impl S3StorageBackend {
    /// Construit le backend a partir d'une configuration deja chargee. Le
    /// client est construit explicitement via `aws_sdk_s3::config::Builder`
    /// (endpoint personnalise + identifiants statiques +
    /// `force_path_style`) plutot que via la decouverte AWS standard, qui
    /// ne s'applique pas a un endpoint non-AWS.
    pub fn new(config: &S3Config) -> Self {
        let credentials = Credentials::new(
            config.access_key_id.clone(),
            config.secret_access_key.clone(),
            None,
            None,
            "atelier-storage-env",
        );
        let s3_config = S3ConfigBuilder::new()
            .behavior_version(BehaviorVersion::latest())
            .endpoint_url(&config.endpoint)
            .region(Region::new(config.region.clone()))
            .credentials_provider(credentials)
            .force_path_style(config.force_path_style)
            .build();
        Self {
            client: S3Client::from_conf(s3_config),
            bucket_sessions: config.bucket_sessions.clone(),
            bucket_snapshots: config.bucket_snapshots.clone(),
        }
    }

    /// Charge la configuration depuis l'environnement et construit le
    /// backend en une seule etape. `Ok(None)` si `S3_ENDPOINT` est absent
    /// (stockage S3 desactive), erreur explicite si la configuration est
    /// partielle.
    pub fn from_env() -> Result<Option<Self>> {
        Ok(config_from_env()?.map(|config| Self::new(&config)))
    }

    /// Cle S3 conventionnelle d'une archive de session : un prefixe par
    /// Workshop (`workshops/<workshop_name>/sessions/`) permet de lister ou
    /// nettoyer toutes les sessions d'un Workshop sans connaitre a l'avance
    /// leurs `session_id`.
    pub fn session_archive_key(workshop_name: &str, session_id: &str) -> String {
        format!("workshops/{workshop_name}/sessions/{session_id}.zst")
    }

    /// Compresse `source` en zstd en streaming (jamais charge entierement
    /// en memoire) et le televerse dans le bucket `S3_BUCKET_SESSIONS` sous
    /// la cle conventionnelle (voir [`Self::session_archive_key`]). Renvoie
    /// la cle S3 utilisee, pour qu'elle soit persistee par l'appelant (ex:
    /// en base, associee a la session) et reutilisee par
    /// [`Self::get_session_stream`].
    pub async fn upload_session_archive(
        &self,
        workshop_name: &str,
        session_id: &str,
        source: impl AsyncRead + Send + Sync + Unpin + 'static,
    ) -> Result<String> {
        let key = Self::session_archive_key(workshop_name, session_id);
        let compressed = ZstdEncoder::new(BufReader::new(source));
        self.upload_stream(&self.bucket_sessions, &key, Box::pin(compressed))
            .await?;
        Ok(key)
    }

    /// Recupere une archive de session depuis le bucket `S3_BUCKET_SESSIONS`
    /// (cle telle que renvoyee par [`Self::upload_session_archive`]) et la
    /// decompresse en streaming pour rejeu progressif par l'appelant.
    pub async fn get_session_stream(&self, s3_key: &str) -> Result<BoxedAsyncRead> {
        let compressed = self.download_stream(&self.bucket_sessions, s3_key).await?;
        let decompressed = ZstdDecoder::new(BufReader::new(compressed));
        Ok(Box::pin(decompressed))
    }

    /// Lit `stream` par blocs de [`MULTIPART_PART_SIZE`] et televerse
    /// chaque bloc comme une part du multipart upload `upload_id` deja
    /// ouvert. Renvoie la liste des parts completees, dans l'ordre —
    /// necessaire a `complete_multipart_upload`. Aucun bloc n'est cree pour
    /// un flux vide (liste vide en retour).
    async fn upload_multipart_parts(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        stream: &mut BoxedAsyncRead,
    ) -> Result<Vec<CompletedPart>> {
        let mut parts = Vec::new();
        let mut part_number: i32 = 1;
        let mut buf = vec![0u8; MULTIPART_PART_SIZE];
        loop {
            let mut filled = 0usize;
            while filled < buf.len() {
                let n = stream
                    .read(&mut buf[filled..])
                    .await
                    .context("lecture du flux source pour le televersement multipart")?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            if filled == 0 {
                break;
            }
            let chunk = Bytes::copy_from_slice(&buf[..filled]);
            let is_last_part = filled < buf.len();
            let output = self
                .client
                .upload_part()
                .bucket(bucket)
                .key(key)
                .upload_id(upload_id)
                .part_number(part_number)
                .body(ByteStream::from(chunk))
                .send()
                .await
                .with_context(|| {
                    format!("televersement de la part {part_number} echoue (bucket={bucket}, cle={key})")
                })?;
            let e_tag = output
                .e_tag()
                .with_context(|| format!("reponse S3 sans ETag pour la part {part_number}"))?
                .to_string();
            parts.push(
                CompletedPart::builder()
                    .part_number(part_number)
                    .e_tag(e_tag)
                    .build(),
            );
            part_number += 1;
            if is_last_part {
                break;
            }
        }
        Ok(parts)
    }
}

/// Taille de part utilisee pour le televersement multipart (voir
/// [`StorageBackend::upload_stream`]) : S3 impose un minimum de 5 Mio pour
/// toute part qui n'est pas la derniere, 8 Mio laisse une marge
/// confortable. La taille totale du flux (sortie compressee zstd d'une
/// session en cours de rejeu) n'est jamais connue a l'avance : un
/// televersement multipart, qui ne requiert que la taille de chaque part
/// individuelle, est la seule methode `put_object` compatible avec un
/// corps de requete de taille inconnue (un simple `put_object` avec un
/// corps en streaming echoue sur RustFS/S3 avec « Only request bodies with
/// a known size can be aws-chunked encoded »).
const MULTIPART_PART_SIZE: usize = 8 * 1024 * 1024;

#[async_trait::async_trait]
impl StorageBackend for S3StorageBackend {
    async fn upload_stream(
        &self,
        bucket: &str,
        key: &str,
        mut stream: BoxedAsyncRead,
    ) -> Result<()> {
        let create = self
            .client
            .create_multipart_upload()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .with_context(|| {
                format!(
                    "ouverture du televersement multipart S3 echouee (bucket={bucket}, cle={key})"
                )
            })?;
        let upload_id = create
            .upload_id()
            .context("reponse S3 sans upload_id pour le televersement multipart")?
            .to_string();

        let parts = self
            .upload_multipart_parts(bucket, key, &upload_id, &mut stream)
            .await;
        let parts = match parts {
            Ok(parts) => parts,
            Err(err) => {
                // Best-effort : un multipart upload jamais complete/aborte
                // reste facturable/liste indefiniment cote serveur S3.
                if let Err(abort_err) = self
                    .client
                    .abort_multipart_upload()
                    .bucket(bucket)
                    .key(key)
                    .upload_id(&upload_id)
                    .send()
                    .await
                {
                    tracing::warn!(
                        %bucket, %key, %upload_id,
                        "echec de l'annulation du televersement multipart apres erreur : {abort_err:#}"
                    );
                }
                return Err(err);
            }
        };

        if parts.is_empty() {
            // Un flux vide n'a produit aucune part : un multipart upload ne
            // peut pas se completer sans au moins une part, on annule et on
            // retombe sur un `put_object` classique (corps vide, taille
            // connue).
            self.client
                .abort_multipart_upload()
                .bucket(bucket)
                .key(key)
                .upload_id(&upload_id)
                .send()
                .await
                .with_context(|| {
                    format!("annulation du televersement multipart vide echouee (bucket={bucket}, cle={key})")
                })?;
            self.client
                .put_object()
                .bucket(bucket)
                .key(key)
                .body(ByteStream::from_static(&[]))
                .send()
                .await
                .with_context(|| {
                    format!("televersement d'un objet vide echoue (bucket={bucket}, cle={key})")
                })?;
            return Ok(());
        }

        self.client
            .complete_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(&upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(parts))
                    .build(),
            )
            .send()
            .await
            .with_context(|| {
                format!("finalisation du televersement multipart S3 echouee (bucket={bucket}, cle={key})")
            })?;
        Ok(())
    }

    async fn download_stream(&self, bucket: &str, key: &str) -> Result<BoxedAsyncRead> {
        let output = self
            .client
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .with_context(|| format!("recuperation S3 echouee (bucket={bucket}, cle={key})"))?;
        Ok(Box::pin(output.body.into_async_read()))
    }

    async fn delete_object(&self, bucket: &str, key: &str) -> Result<()> {
        self.client
            .delete_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .with_context(|| format!("suppression S3 echouee (bucket={bucket}, cle={key})"))?;
        Ok(())
    }
}
