//! Device plugin Kubernetes (kubelet API v1beta1) pour `/dev/kvm` (et
//! `/dev/net/tun`, alloue dans le meme lot — un pod qui a besoin de l'un a
//! toujours besoin de l'autre dans ce projet). Objectif : permettre a
//! `vm-supervisor`/`image-builder` de demander ces devices via
//! `resources.limits`, sans `securityContext.privileged: true` — le
//! blocage constate auparavant ("Operation not permitted" malgre
//! permissions/capabilities correctes sur un pod non privilegie) venait du
//! device cgroup controller de Kubernetes/containerd, qui n'autorise
//! l'ouverture d'un device node hostPath que si le pod est privilegie OU si
//! kubelet l'a explicitement ajoute a la whitelist du cgroup — ce que fait
//! precisement le mecanisme "device plugin" via les `DeviceSpec` renvoyes
//! par `Allocate`.
//!
//! Protocole (proto vendore dans `proto/api.proto`, sous-ensemble de
//! `k8s.io/kubelet/pkg/apis/deviceplugin/v1beta1`) : ce plugin cree son
//! propre socket UNIX dans `/var/lib/kubelet/device-plugins/`, puis
//! s'enregistre une fois aupres de `kubelet.sock` (service `Registration`)
//! — c'est ensuite le kubelet qui rappelle ce plugin sur son propre socket
//! (`ListAndWatch`, `Allocate`).

use anyhow::{Context, Result};
use pluginapi::device_plugin_server::{DevicePlugin, DevicePluginServer};
use pluginapi::registration_client::RegistrationClient;
use pluginapi::{
    AllocateRequest, AllocateResponse, ContainerAllocateResponse, Device, DevicePluginOptions,
    DeviceSpec, Empty, ListAndWatchResponse, PreStartContainerRequest, PreStartContainerResponse,
    RegisterRequest,
};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio_stream::wrappers::UnixListenerStream;
use tokio_stream::Stream;
use tonic::transport::{Endpoint, Server, Uri};
use tonic::{Request, Response, Status};

mod pluginapi {
    include!("pluginapi.rs");
}

const KUBELET_SOCKET: &str = "/var/lib/kubelet/device-plugins/kubelet.sock";
const PLUGIN_DIR: &str = "/var/lib/kubelet/device-plugins";
const ENDPOINT_FILE: &str = "atelier-kvm.sock";
const API_VERSION: &str = "v1beta1";

fn resource_name() -> String {
    std::env::var("ATELIER_KVM_RESOURCE_NAME").unwrap_or_else(|_| "atelier.dev/kvm".to_string())
}

fn device_count() -> usize {
    std::env::var("ATELIER_KVM_DEVICE_COUNT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32)
}

fn kvm_path() -> PathBuf {
    std::env::var("ATELIER_KVM_DEVICE_PATH")
        .unwrap_or_else(|_| "/dev/kvm".to_string())
        .into()
}

fn tun_path() -> PathBuf {
    std::env::var("ATELIER_TUN_DEVICE_PATH")
        .unwrap_or_else(|_| "/dev/net/tun".to_string())
        .into()
}

struct KvmDevicePlugin {
    device_ids: Vec<String>,
    kvm_path: PathBuf,
    tun_path: PathBuf,
    health_rx: watch::Receiver<bool>,
}

#[tonic::async_trait]
impl DevicePlugin for KvmDevicePlugin {
    async fn get_device_plugin_options(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<DevicePluginOptions>, Status> {
        Ok(Response::new(DevicePluginOptions {
            pre_start_required: false,
            get_preferred_allocation_available: false,
        }))
    }

    type ListAndWatchStream =
        Pin<Box<dyn Stream<Item = Result<ListAndWatchResponse, Status>> + Send + 'static>>;

    async fn list_and_watch(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Self::ListAndWatchStream>, Status> {
        let device_ids = self.device_ids.clone();
        let mut health_rx = self.health_rx.clone();
        let stream = async_stream::stream! {
            loop {
                let healthy = *health_rx.borrow();
                let health = if healthy { "Healthy" } else { "Unhealthy" };
                let devices = device_ids
                    .iter()
                    .map(|id| Device { id: id.clone(), health: health.to_string() })
                    .collect();
                yield Ok(ListAndWatchResponse { devices });
                if health_rx.changed().await.is_err() {
                    break;
                }
            }
        };
        Ok(Response::new(Box::pin(stream)))
    }

    async fn allocate(
        &self,
        request: Request<AllocateRequest>,
    ) -> Result<Response<AllocateResponse>, Status> {
        let container_responses = request
            .into_inner()
            .container_requests
            .into_iter()
            .map(|_| ContainerAllocateResponse {
                envs: Default::default(),
                mounts: vec![],
                devices: vec![
                    DeviceSpec {
                        container_path: self.kvm_path.to_string_lossy().into_owned(),
                        host_path: self.kvm_path.to_string_lossy().into_owned(),
                        permissions: "rw".to_string(),
                    },
                    DeviceSpec {
                        container_path: self.tun_path.to_string_lossy().into_owned(),
                        host_path: self.tun_path.to_string_lossy().into_owned(),
                        permissions: "rw".to_string(),
                    },
                ],
                annotations: Default::default(),
            })
            .collect();
        Ok(Response::new(AllocateResponse {
            container_responses,
        }))
    }

    async fn pre_start_container(
        &self,
        _request: Request<PreStartContainerRequest>,
    ) -> Result<Response<PreStartContainerResponse>, Status> {
        Ok(Response::new(PreStartContainerResponse {}))
    }
}

/// Sonde l'existence de `/dev/kvm` toutes les 30s et publie le changement
/// d'etat de sante sur le canal `watch` que `ListAndWatch` observe —
/// kubelet doit voir un device disparaitre s'il n'est brutalement plus la
/// (panne materielle, module `kvm` decharge), pas juste au demarrage du
/// plugin.
fn spawn_health_watcher(kvm_path: PathBuf) -> watch::Receiver<bool> {
    let initial = kvm_path.exists();
    let (tx, rx) = watch::channel(initial);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let healthy = kvm_path.exists();
            if healthy != *tx.borrow() {
                tracing::warn!(healthy, path = %kvm_path.display(), "changement de sante /dev/kvm");
                let _ = tx.send(healthy);
            }
        }
    });
    rx
}

async fn register(resource_name: &str) -> Result<()> {
    let channel = Endpoint::try_from("http://[::]:50051")
        .expect("URI factice, jamais resolue : connect_with_connector dial le socket directement")
        .connect_with_connector(tower::service_fn(|_: Uri| async {
            let stream = UnixStream::connect(KUBELET_SOCKET).await?;
            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
        }))
        .await
        .context("connexion a kubelet.sock (Registration)")?;

    let mut client = RegistrationClient::new(channel);
    client
        .register(RegisterRequest {
            version: API_VERSION.to_string(),
            endpoint: ENDPOINT_FILE.to_string(),
            resource_name: resource_name.to_string(),
            options: Some(DevicePluginOptions {
                pre_start_required: false,
                get_preferred_allocation_available: false,
            }),
        })
        .await
        .context("appel Registration.Register aupres du kubelet")?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let resource_name = resource_name();
    let kvm_path = kvm_path();
    let tun_path = tun_path();
    let device_ids: Vec<String> = (0..device_count()).map(|i| format!("kvm-{i}")).collect();

    tracing::info!(
        resource_name = %resource_name,
        devices = device_ids.len(),
        kvm_path = %kvm_path.display(),
        tun_path = %tun_path.display(),
        "atelier-kvm-device-plugin demarre"
    );

    let health_rx = spawn_health_watcher(kvm_path.clone());

    let endpoint_path = Path::new(PLUGIN_DIR).join(ENDPOINT_FILE);
    // Un socket orphelin d'un run precedent (crash, pas de nettoyage propre)
    // ferait echouer le `bind` suivant en `AddrInUse`.
    let _ = std::fs::remove_file(&endpoint_path);
    let listener = UnixListener::bind(&endpoint_path)
        .with_context(|| format!("ecoute sur {}", endpoint_path.display()))?;
    let incoming = UnixListenerStream::new(listener);

    let plugin = KvmDevicePlugin {
        device_ids,
        kvm_path,
        tun_path,
        health_rx,
    };

    let server = tokio::spawn(async move {
        if let Err(err) = Server::builder()
            .add_service(DevicePluginServer::new(plugin))
            .serve_with_incoming(incoming)
            .await
        {
            tracing::error!(%err, "serveur DevicePlugin arrete");
        }
    });

    // Le kubelet doit pouvoir composer notre socket avant l'enregistrement,
    // sans quoi son premier `ListAndWatch` echoue immediatement.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    register(&resource_name).await?;
    tracing::info!(resource_name = %resource_name, "enregistre aupres du kubelet");

    server.await.context("tache serveur DevicePlugin")?;
    Ok(())
}
