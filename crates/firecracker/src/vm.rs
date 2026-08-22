//! Cycle de vie d'une microVM Firecracker : boot depuis un kernel+rootfs,
//! snapshot, restauration depuis un snapshot. Construit sur
//! [`fctools`](https://docs.rs/fctools), le SDK Rust le plus complet pour
//! piloter Firecracker (executor jailer inclus), plutot qu'un client HTTP
//! maison sur le socket Unix de l'API.
//!
//! Toujours jaile (`JailedVmmExecutor`) : c'est le point de cette
//! implementation par rapport a l'iteration precedente (Firecracker nu).
//! Le binaire `jailer` a besoin de privileges (chroot, cgroups, unshare de
//! namespace de montage) mais **pas de root complet** : on lui attribue les
//! capabilities Linux necessaires directement sur le fichier binaire (une
//! fois, via `setcap`, hors de ce code) plutot que de l'invoquer via `sudo`.
//! `sudo` a ete essaye et abandonne : le spawner `sudo`-based de `fctools`
//! invoque toujours `sudo -S -s <bin> ...`, et le flag `-s` fait autoriser
//! le *shell* par sudoers plutot que le binaire jailer lui-meme — impossible
//! de scoper une regle NOPASSWD finement dans ce cas sans autoriser un shell
//! root arbitraire, ce qu'on ne veut pas. Voir le commentaire sur
//! `setcap` requis :
//! `sudo setcap cap_sys_admin,cap_sys_resource,cap_sys_chroot,cap_setuid,\
//! cap_setgid,cap_mknod,cap_dac_override+eip <chemin-vers-jailer>`
//!
//! Ce crate est partage par `vm-supervisor` (VM de l'agent, sans reseau) et
//! par la microVM "builder" (isolation d'`envbuilder`, avec reseau — voir
//! [`crate::network`]).

use crate::network::NetworkSetup;
use anyhow::{ensure, Context, Result};
use fctools::process_spawner::DirectProcessSpawner;
use fctools::runtime::tokio::TokioRuntime;
use fctools::vm::api::VmApi;
use fctools::vm::configuration::{InitMethod, VmConfiguration, VmConfigurationData};
use fctools::vm::models::{
    BootSource, CreateSnapshot, Drive, LoadSnapshot, MachineConfiguration, MemoryBackend,
    MemoryBackendType, NetworkInterface, SnapshotType, VsockDevice,
};
use fctools::vm::shutdown::{VmShutdownAction, VmShutdownMethod};
use fctools::vm::snapshot::{PrepareVmFromSnapshotOptions, VmSnapshot};
use fctools::vmm::arguments::jailer::JailerArguments;
use fctools::vmm::arguments::{VmmApiSocket, VmmArguments};
use fctools::vmm::executor::jailed::{FlatVirtualPathResolver, JailedVmmExecutor};
use fctools::vmm::id::VmmId;
use fctools::vmm::installation::VmmInstallation;
use fctools::vmm::ownership::VmmOwnershipModel;
use fctools::vmm::resource::system::ResourceSystem;
use fctools::vmm::resource::{MovedResourceType, ResourceType};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::compat::FuturesAsyncReadCompatExt;

/// Certaines erreurs de `fctools` (`VmError`, `VmApiError`, `VmShutdownError`)
/// n'implementent pas `Sync`, requis par `anyhow::Context`. Elles
/// implementent en revanche toutes `Display`, d'ou cette conversion manuelle.
fn to_anyhow<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> anyhow::Error {
    move |e| anyhow::anyhow!("{context}: {e}")
}

type Executor = JailedVmmExecutor<FlatVirtualPathResolver>;
type Spawner = DirectProcessSpawner;
type FcVm = fctools::vm::Vm<Executor, Spawner, TokioRuntime>;

/// Declare les ressources kernel/rootfs et construit la configuration de la
/// VM — commun a un boot normal ([`Vm::boot`]/[`Vm::boot_with_network`]) et
/// a une restauration depuis un snapshot persiste ([`Vm::restore_persisted`]) :
/// dans les deux cas, la configuration ne depend que de ces memes parametres
/// d'entree, jamais d'un etat runtime prealable.
fn build_configuration_data(
    resource_system: &mut ResourceSystem<Spawner, TokioRuntime>,
    config: &VmConfig,
    kernel_path: &Path,
    rootfs_path: &Path,
    network: Option<&NetworkSetup>,
) -> Result<VmConfigurationData> {
    let kernel = resource_system
        .create_resource(kernel_path.to_path_buf(), ResourceType::Moved(MovedResourceType::Copied))
        .context("declaration de la ressource kernel")?;
    let rootfs = resource_system
        .create_resource(rootfs_path.to_path_buf(), ResourceType::Moved(MovedResourceType::Copied))
        .context("declaration de la ressource rootfs")?;

    let network_interfaces = network
        .map(|net| {
            vec![NetworkInterface {
                iface_id: net.iface_id.clone(),
                host_dev_name: net.tap_name.clone(),
                guest_mac: Some(net.guest_mac.clone()),
                rx_rate_limiter: None,
                tx_rate_limiter: None,
            }]
        })
        .unwrap_or_default();

    // Meme remarque que pour les ressources `Produced` d'un snapshot (voir
    // `Vm::snapshot`) : le chemin donne a Firecracker doit etre relatif au
    // jail ("/vsock.sock"), c'est lui qui cree ce fichier au demarrage
    // (uds "principal", cote hote — un sibling process qui veut recevoir
    // les connexions initiees par le guest doit lui-meme lier un UDS a
    // "<uds_path>_<port>", convention Firecracker, voir `crates/mcp-gateway`).
    let vsock_device = config
        .vsock
        .as_ref()
        .map(|vsock| -> Result<VsockDevice> {
            let uds = resource_system
                .create_resource(PathBuf::from(&vsock.uds_relative_path), ResourceType::Produced)
                .context("declaration de la ressource uds vsock")?;
            Ok(VsockDevice { guest_cid: vsock.guest_cid, uds })
        })
        .transpose()?;

    Ok(VmConfigurationData {
        boot_source: BootSource {
            kernel_image: kernel,
            boot_args: Some(config.boot_args.clone()),
            initrd: None,
        },
        drives: vec![Drive {
            drive_id: "rootfs".to_string(),
            is_root_device: true,
            cache_type: None,
            partuuid: None,
            is_read_only: Some(false),
            block: Some(rootfs),
            rate_limiter: None,
            io_engine: None,
            socket: None,
        }],
        pmem_devices: vec![],
        machine_configuration: MachineConfiguration {
            vcpu_count: config.vcpu_count,
            mem_size_mib: config.mem_mib,
            smt: None,
            track_dirty_pages: Some(true),
            huge_pages: None,
        },
        cpu_template: None,
        network_interfaces,
        balloon_device: None,
        vsock_device,
        logger_system: None,
        metrics_system: None,
        memory_hotplug_configuration: None,
        mmds_configuration: None,
        entropy_device: None,
    })
}

pub struct VmConfig {
    pub firecracker_bin: PathBuf,
    pub jailer_bin: PathBuf,
    pub snapshot_editor_bin: PathBuf,
    /// Base des jails (`--chroot-base-dir` du jailer), ex: `/srv/jailer`.
    pub chroot_base_dir: PathBuf,
    /// Identifiant du jail, doit etre alphanumerique/`-`, 1-64 caracteres.
    pub jail_id: String,
    /// UID/GID sous lequel le process Firecracker tourne une fois jaile
    /// (le jailer lui-meme tourne toujours root, c'est une contrainte du
    /// binaire, mais downgrade immediatement vers cet utilisateur).
    pub uid: u32,
    pub gid: u32,
    pub vcpu_count: u8,
    pub mem_mib: usize,
    pub boot_args: String,
    /// Device `AF_VSOCK` optionnel : canal guest<->hote a l'interieur du
    /// meme pod, plus bas niveau que le chemin via `net-proxy` (pas de TAP,
    /// pas d'iptables, pas d'allowlist egress a traverser) — utilise par
    /// `mcp-gateway` pour les connexions initiees par le guest. Absent par
    /// defaut (`None`), la microVM builder et les tests existants n'en ont
    /// pas besoin.
    pub vsock: Option<VsockConfig>,
}

/// `guest_cid` : identifiant du guest sur le "reseau" vsock, doit etre >= 3
/// (0/1/2 sont reserves, 2 = l'hote). `uds_relative_path` : chemin **relatif
/// au jail** (ex: `/vsock.sock`) du socket Unix "principal" que Firecracker
/// cree lui-meme au demarrage (ressource `Produced`, voir
/// `build_configuration_data`) — le chemin hote reel resulte du jail
/// (`chroot_base_dir/jail_id/root/<nom>`).
#[derive(Debug, Clone)]
pub struct VsockConfig {
    pub guest_cid: u32,
    pub uds_relative_path: String,
}

impl VmConfig {
    fn installation(&self) -> VmmInstallation {
        VmmInstallation::new(
            self.firecracker_bin.clone(),
            self.jailer_bin.clone(),
            self.snapshot_editor_bin.clone(),
        )
    }

    fn ownership_model(&self) -> VmmOwnershipModel {
        VmmOwnershipModel::Downgraded {
            uid: self.uid,
            gid: self.gid,
        }
    }

    fn spawner(&self) -> Spawner {
        // Le jailer porte ses propres capabilities Linux (setcap), aucune
        // elevation necessaire au moment du spawn.
        DirectProcessSpawner
    }

    fn executor(&self, socket_path: &str) -> Result<Executor> {
        let jailer_args = JailerArguments::new(
            VmmId::new(self.jail_id.clone()).context("jail_id invalide")?,
        )
        .chroot_base_dir(self.chroot_base_dir.clone());

        Ok(JailedVmmExecutor::new(
            VmmArguments::new(VmmApiSocket::Enabled(socket_path.into())),
            jailer_args,
            FlatVirtualPathResolver,
        ))
    }
}

/// Draine en continu la console serie du guest (stdout/stderr du process
/// Firecracker, relies au `ttyS0` du guest via `console=ttyS0`) vers les
/// logs `tracing`. **Necessaire, pas juste utile** : un pipe Unix a un
/// buffer fini (64 Kio en pratique) ; si rien ne le lit, une ecriture du
/// guest au-dela de cette limite **bloque indefiniment tout le guest**, pas
/// seulement la sortie console — constate en pratique (une microVM avec un
/// vrai volume de sortie, ex: `envbuilder` qui clone puis construit une
/// image, se figeait sans jamais atteindre le reseau, alors que des boots
/// courts avec peu de sortie fonctionnaient tous). Ignore les erreurs de
/// lecture (pipe ferme a l'extinction de la VM, cas normal).
fn drain_console_pipes(inner: &mut FcVm) {
    let Ok(pipes) = inner.take_pipes() else {
        return;
    };
    tokio::spawn(drain_lines("stdout", pipes.stdout.compat()));
    tokio::spawn(drain_lines("stderr", pipes.stderr.compat()));
}

async fn drain_lines<R: tokio::io::AsyncRead + Unpin>(stream_name: &'static str, reader: R) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => tracing::debug!(console = stream_name, %line, "sortie console du guest"),
            Ok(None) | Err(_) => break,
        }
    }
}

/// Une microVM en cours d'execution.
pub struct Vm {
    inner: FcVm,
}

impl Vm {
    /// Demarre une nouvelle microVM jailee depuis un kernel et un rootfs,
    /// sans interface reseau (usage `vm-supervisor` aujourd'hui : l'agent
    /// n'a pas encore de sortie reseau directe).
    pub async fn boot(config: &VmConfig, kernel_path: &Path, rootfs_path: &Path) -> Result<Self> {
        Self::boot_internal(config, kernel_path, rootfs_path, None).await
    }

    /// Variante avec une interface reseau deja preparee cote hote (TAP +
    /// NAT, voir [`crate::network::setup_link_local_tap`]) : utilise par la
    /// microVM "builder", qui doit joindre un depot git et un registre OCI.
    pub async fn boot_with_network(
        config: &VmConfig,
        kernel_path: &Path,
        rootfs_path: &Path,
        network: &NetworkSetup,
    ) -> Result<Self> {
        Self::boot_internal(config, kernel_path, rootfs_path, Some(network)).await
    }

    async fn boot_internal(
        config: &VmConfig,
        kernel_path: &Path,
        rootfs_path: &Path,
        network: Option<&NetworkSetup>,
    ) -> Result<Self> {
        let mut resource_system =
            ResourceSystem::new(config.spawner(), TokioRuntime, config.ownership_model());
        let data = build_configuration_data(&mut resource_system, config, kernel_path, rootfs_path, network)?;

        let executor = config.executor("/run/firecracker.socket")?;
        let installation = config.installation();

        let mut inner = FcVm::prepare(
            executor,
            resource_system,
            installation,
            VmConfiguration::New {
                init_method: InitMethod::ViaApiCalls,
                data,
            },
        )
        .await
        .map_err(to_anyhow("preparation de la microVM (jail, ressources)"))?;

        inner
            .start(Duration::from_secs(5))
            .await
            .map_err(to_anyhow("demarrage de la microVM"))?;

        drain_console_pipes(&mut inner);

        Ok(Self { inner })
    }

    /// Restaure une microVM depuis un snapshot **persiste** (fichiers
    /// `snapshot.state`/`snapshot.mem` copies hors du jail d'origine, par
    /// exemple dans un cache content-addressed), dans un process qui n'a
    /// plus acces a l'objet `Vm` d'origine — contrairement a [`Vm::restore`],
    /// qui a besoin d'un `Vm` source vivant dans le meme process (l'API telle
    /// que fournie par `fctools` n'est pas concue pour survivre a un
    /// redemarrage complet du process appelant : `VmConfigurationData` ne
    /// derive que `Serialize`, pas `Deserialize`, et `Resource` encapsule un
    /// `Arc` vers l'etat interne du systeme de ressources d'origine).
    ///
    /// Contournement : `VmConfigurationData` est entierement determinee par
    /// les memes parametres qu'un boot normal (kernel, rootfs, vcpu/mem,
    /// boot_args, reseau) — elle est donc **reconstruite a l'identique**
    /// plutot que deserialisee, exactement comme le ferait un nouveau
    /// `Vm::boot`/`Vm::boot_with_network`, avec pour seule difference le
    /// `VmConfiguration::RestoredFromSnapshot` (charge l'etat/memoire figes)
    /// a la place de `VmConfiguration::New`. La coherence du chemin virtuel
    /// jaile (`FlatVirtualPathResolver`, base sur le nom de fichier, pas sur
    /// l'identite de la ressource) rend cette reconstruction valide : la
    /// configuration serialisee vers Firecracker reference "/vmlinux.bin",
    /// "/rootfs.ext4", peu importe quel objet `Resource` interne les a
    /// produits.
    pub async fn restore_persisted(
        config: &VmConfig,
        kernel_path: &Path,
        rootfs_path: &Path,
        network: Option<&NetworkSetup>,
        snapshot_path: &Path,
        mem_file_path: &Path,
    ) -> Result<Self> {
        let mut resource_system =
            ResourceSystem::new(config.spawner(), TokioRuntime, config.ownership_model());
        let data = build_configuration_data(&mut resource_system, config, kernel_path, rootfs_path, network)?;

        let snapshot = resource_system
            .create_resource(snapshot_path.to_path_buf(), ResourceType::Moved(MovedResourceType::Copied))
            .context("declaration de la ressource snapshot")?;
        let mem_file = resource_system
            .create_resource(mem_file_path.to_path_buf(), ResourceType::Moved(MovedResourceType::Copied))
            .context("declaration de la ressource memoire")?;

        let load_snapshot = LoadSnapshot {
            track_dirty_pages: Some(false),
            mem_backend: MemoryBackend {
                backend_type: MemoryBackendType::File,
                backend: mem_file,
            },
            snapshot,
            resume_vm: Some(true),
            network_overrides: Vec::new(),
        };

        let executor = config.executor("/run/firecracker.socket")?;
        let installation = config.installation();

        let mut inner = FcVm::prepare(
            executor,
            resource_system,
            installation,
            VmConfiguration::RestoredFromSnapshot { load_snapshot, data },
        )
        .await
        .map_err(to_anyhow("preparation de la microVM depuis un snapshot persiste"))?;

        inner
            .start(Duration::from_secs(5))
            .await
            .map_err(to_anyhow("restauration (snapshot/load) de la microVM"))?;

        drain_console_pipes(&mut inner);

        Ok(Self { inner })
    }

    /// Restaure une microVM depuis un snapshot pris precedemment par
    /// [`Vm::snapshot`], dans un nouveau jail (un jail ne peut pas etre
    /// reutilise apres que son process ait quitte).
    pub async fn restore(
        &mut self,
        snapshot: VmSnapshot,
        config: &VmConfig,
    ) -> Result<Self> {
        let executor = config.executor("/run/firecracker.socket")?;

        let mut inner = snapshot
            .prepare_vm(
                &mut self.inner,
                PrepareVmFromSnapshotOptions {
                    executor,
                    process_spawner: config.spawner(),
                    runtime: TokioRuntime,
                    moved_resource_type: MovedResourceType::Copied,
                    ownership_model: config.ownership_model(),
                    track_dirty_pages: Some(false),
                    resume_vm: Some(true),
                    network_overrides: Vec::new(),
                },
            )
            .await
            .map_err(to_anyhow("preparation de la microVM depuis le snapshot"))?;

        inner
            .start(Duration::from_secs(5))
            .await
            .map_err(to_anyhow("restauration (snapshot/load) de la microVM"))?;

        drain_console_pipes(&mut inner);

        Ok(Self { inner })
    }

    /// Fige la VM (pause) et ecrit son etat + sa memoire complete sur
    /// disque. La VM est remise en cours d'execution apres l'appel : c'est
    /// a l'appelant de l'arreter ensuite si le pod parent va etre libere
    /// (mise en veille). Les chemins hote reels des fichiers produits sont
    /// dans le `VmSnapshot` renvoye (`snapshot_path`/`mem_file_path`).
    ///
    /// Note d'implementation : pour une ressource `Produced`, `fctools`
    /// transmet le chemin initial tel quel a Firecracker (pas de resolution
    /// virtuelle automatique comme pour les ressources `Moved`). Il faut
    /// donc lui donner un chemin **relatif au jail** ("/snapshot.state"),
    /// pas un chemin hote — sinon Firecracker (qui tourne chroote) cherche
    /// ce chemin a l'intérieur du jail et echoue avec ENOENT. Le chemin
    /// hote effectif (`jail_root/snapshot.state`) est calcule par fctools
    /// et expose ensuite via `VmSnapshot`.
    pub async fn snapshot(&mut self) -> Result<VmSnapshot> {
        self.inner.pause().await.map_err(to_anyhow("mise en pause avant snapshot"))?;

        let create_snapshot = CreateSnapshot {
            snapshot_type: Some(SnapshotType::Full),
            snapshot: self
                .inner
                .get_resource_system_mut()
                .create_resource(PathBuf::from("/snapshot.state"), ResourceType::Produced)
                .context("declaration de la ressource snapshot")?,
            mem_file: self
                .inner
                .get_resource_system_mut()
                .create_resource(PathBuf::from("/snapshot.mem"), ResourceType::Produced)
                .context("declaration de la ressource memoire")?,
        };

        let snapshot = self
            .inner
            .create_snapshot(create_snapshot)
            .await
            .map_err(to_anyhow("creation du snapshot"))?;

        self.inner.resume().await.map_err(to_anyhow("reprise apres snapshot"))?;

        Ok(snapshot)
    }

    /// La microVM est vivante et non figee (un `get_info()` reussi suffit a
    /// prouver que le process VMM repond ; `is_paused` distingue du cas ou
    /// elle vient d'etre mise en pause pour un snapshot).
    pub async fn is_running(&mut self) -> Result<bool> {
        let info = self
            .inner
            .get_info()
            .await
            .map_err(to_anyhow("lecture de l'etat de la microVM"))?;
        Ok(!info.is_paused)
    }

    /// Arrete proprement la microVM (Ctrl-Alt-Del puis kill en secours) et
    /// nettoie le jail sur disque. Le nettoyage est tente meme si la VM
    /// s'est deja arretee d'elle-meme entre-temps (auto-shutdown du guest,
    /// crash, ...) : `shutdown()` cote fctools echoue dans ce cas des la
    /// verification d'etat initiale, avant meme de tenter une action,
    /// laissant sinon le jail orphelin sur le disque.
    pub async fn shutdown(mut self) -> Result<()> {
        let shutdown_result = self
            .inner
            .shutdown([
                VmShutdownAction {
                    method: VmShutdownMethod::CtrlAltDel,
                    timeout: Some(Duration::from_secs(3)),
                    graceful: true,
                },
                VmShutdownAction {
                    method: VmShutdownMethod::Kill,
                    timeout: Some(Duration::from_secs(2)),
                    graceful: false,
                },
            ])
            .await
            .map_err(to_anyhow("arret de la microVM"));

        self.inner.cleanup().await.map_err(to_anyhow("nettoyage du jail"))?;

        let outcome = shutdown_result?;
        ensure!(
            outcome.exit_status.success() || !outcome.graceful,
            "arret de la microVM en echec: {outcome:?}"
        );
        Ok(())
    }
}
