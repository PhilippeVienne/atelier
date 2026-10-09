//! Taille du disque racine d'une microVM (spec
//! `docs/specs/19-sessions-pour-apprenants.md`, §3.1, tache 14.1).
//!
//! `image-builder` produit un `rootfs.ext4` dimensionne sur son contenu
//! plus une marge fixe : l'invite n'avait donc jamais que quelques
//! centaines de Mio libres, quelle que soit la valeur de
//! `Workshop.spec.resources.disk`. Le disque de CHAQUE microVM est une
//! copie privee de cette image (`MovedResourceType::Copied`) : c'est cette
//! copie qu'on agrandit ici, juste apres qu'elle a ete placee dans le jail
//! et avant que Firecracker ne la voie.
//!
//! Agrandir = allonger le fichier (creux : rien n'est consomme sur le
//! noeud tant que l'invite n'ecrit pas) puis etendre le systeme de
//! fichiers ext4 a la taille du fichier. Jamais de reduction : une taille
//! demandee inferieure a celle de l'image laisse l'image telle quelle.

use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use tokio::process::Command;

const MIB: u64 = 1024 * 1024;

/// Ce que [`grow_ext4`] a fait du fichier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resized {
    /// Systeme de fichiers etendu a cette taille (Mio).
    Grown { from_mib: u64, to_mib: u64 },
    /// Taille demandee inferieure ou egale a celle de l'image : inchange.
    AlreadyLargeEnough { size_mib: u64 },
}

/// Etend le systeme de fichiers ext4 contenu dans `image` a `size_mib`
/// Mio. Sans effet si l'image fait deja cette taille ou plus.
///
/// Necessite `e2fsck` et `resize2fs` (paquet `e2fsprogs`) dans le
/// conteneur appelant. `resize2fs` refuse d'etendre un systeme de fichiers
/// qui n'a pas ete verifie depuis sa derniere modification, d'ou le
/// `e2fsck -f` prealable : sur une image fraichement produite par
/// `mke2fs -d` il ne trouve rien et prend une fraction de seconde.
pub async fn grow_ext4(image: &Path, size_mib: u64) -> Result<Resized> {
    let current = tokio::fs::metadata(image)
        .await
        .with_context(|| format!("lecture de la taille de {image:?}"))?
        .len();
    let target = size_mib
        .checked_mul(MIB)
        .context("taille de disque demandee trop grande")?;
    if target <= current {
        return Ok(Resized::AlreadyLargeEnough {
            size_mib: current / MIB,
        });
    }

    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(image)
        .await
        .with_context(|| format!("ouverture de {image:?} en ecriture"))?;
    file.set_len(target)
        .await
        .with_context(|| format!("allongement de {image:?} a {size_mib} Mio"))?;
    drop(file);

    // Codes de sortie de e2fsck : 0 = rien a corriger, 1 = erreurs
    // corrigees ; 4 et au-dela = systeme de fichiers encore en erreur ou
    // echec de l'outil lui-meme.
    let check = Command::new("e2fsck")
        .args(["-f", "-y"])
        .arg(image)
        .output()
        .await
        .context("lancement de e2fsck (paquet e2fsprogs requis)")?;
    if !matches!(check.status.code(), Some(0 | 1)) {
        bail!(
            "e2fsck a echoue sur {image:?} ({}) : {}",
            check.status,
            String::from_utf8_lossy(&check.stderr).trim()
        );
    }

    // Sans argument de taille, resize2fs etend jusqu'a la taille du
    // support, ici le fichier qu'on vient d'allonger.
    let resize = Command::new("resize2fs")
        .arg(image)
        .output()
        .await
        .context("lancement de resize2fs (paquet e2fsprogs requis)")?;
    ensure!(
        resize.status.success(),
        "resize2fs a echoue sur {image:?} ({}) : {}",
        resize.status,
        String::from_utf8_lossy(&resize.stderr).trim()
    );

    Ok(Resized::Grown {
        from_mib: current / MIB,
        to_mib: size_mib,
    })
}

/// Copie `source` vers `destination` en conservant les trous du fichier
/// (spec 19, §3.8, tache 14.11).
///
/// Le disque d'une microVM est un fichier creux : allonge a
/// `resources.disk` par [`grow_ext4`], il n'occupe sur le noeud que ce que
/// l'invite a reellement ecrit. `tokio::fs::copy` materialiserait chaque
/// trou en zeros (plusieurs Gio ecrits pour rien, a chaque mise en veille
/// et a chaque reprise) ; `cp --sparse=always` saute les trous a la lecture
/// et les recree a l'ecriture.
///
/// Une `destination` existante est reecrite en place : elle garde son
/// proprietaire et ses droits, ce dont depend la reprise (le fichier du
/// jail appartient deja a l'utilisateur de Firecracker).
pub async fn copy_sparse(source: &Path, destination: &Path) -> Result<()> {
    let copy = Command::new("cp")
        .arg("--sparse=always")
        .arg("--")
        .arg(source)
        .arg(destination)
        .output()
        .await
        .context("lancement de cp")?;
    ensure!(
        copy.status.success(),
        "copie de {source:?} vers {destination:?} echouee ({}) : {}",
        copy.status,
        String::from_utf8_lossy(&copy.stderr).trim()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    /// Cree une vraie image ext4 de `size_mib` Mio contenant un fichier,
    /// comme le fait `image-builder` (`mke2fs -d`).
    fn ext4_image(dir: &Path, size_mib: u64) -> std::path::PathBuf {
        let content = dir.join("content");
        std::fs::create_dir_all(content.join("etc")).unwrap();
        std::fs::write(content.join("etc/hostname"), "atelier\n").unwrap();
        let image = dir.join("rootfs.ext4");
        let file = std::fs::File::create(&image).unwrap();
        file.set_len(size_mib * MIB).unwrap();
        drop(file);
        let status = StdCommand::new("mke2fs")
            .args(["-q", "-F", "-t", "ext4", "-d"])
            .arg(&content)
            .arg(&image)
            .status()
            .expect("mke2fs (paquet e2fsprogs) est requis pour ce test");
        assert!(status.success());
        image
    }

    /// Taille du systeme de fichiers d'apres son superbloc, en Mio.
    fn filesystem_size_mib(image: &Path) -> u64 {
        let output = StdCommand::new("dumpe2fs")
            .arg("-h")
            .arg(image)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let field = |name: &str| -> u64 {
            text.lines()
                .find_map(|line| line.strip_prefix(name))
                .unwrap_or_else(|| panic!("champ {name} absent de dumpe2fs"))
                .trim()
                .parse()
                .unwrap()
        };
        field("Block count:") * field("Block size:") / MIB
    }

    #[tokio::test]
    async fn grow_ext4_extends_the_filesystem_and_keeps_its_content() {
        let dir = tempfile::tempdir().unwrap();
        let image = ext4_image(dir.path(), 32);
        assert_eq!(filesystem_size_mib(&image), 32);

        let resized = grow_ext4(&image, 200).await.unwrap();
        assert_eq!(
            resized,
            Resized::Grown {
                from_mib: 32,
                to_mib: 200
            }
        );
        // Le systeme de fichiers lui-meme a grandi, pas seulement le fichier.
        assert_eq!(filesystem_size_mib(&image), 200);
        assert_eq!(std::fs::metadata(&image).unwrap().len(), 200 * MIB);

        // Le contenu de l'image est intact et le systeme de fichiers sain.
        let content = StdCommand::new("debugfs")
            .args(["-R", "cat /etc/hostname"])
            .arg(&image)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&content.stdout), "atelier\n");
        let check = StdCommand::new("e2fsck")
            .args(["-f", "-n"])
            .arg(&image)
            .output()
            .unwrap();
        assert!(check.status.success(), "{check:?}");
    }

    #[tokio::test]
    async fn grow_ext4_never_shrinks() {
        let dir = tempfile::tempdir().unwrap();
        let image = ext4_image(dir.path(), 64);
        for requested in [16, 64] {
            let resized = grow_ext4(&image, requested).await.unwrap();
            assert_eq!(resized, Resized::AlreadyLargeEnough { size_mib: 64 });
        }
        assert_eq!(filesystem_size_mib(&image), 64);
    }

    #[tokio::test]
    async fn grow_ext4_fails_on_a_file_that_is_not_ext4() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("not-ext4");
        std::fs::write(&image, vec![0u8; MIB as usize]).unwrap();
        let error = grow_ext4(&image, 8).await.unwrap_err().to_string();
        assert!(error.contains("e2fsck a echoue"), "{error}");
    }

    #[tokio::test]
    async fn copy_sparse_keeps_content_and_holes() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("disk");
        let file = std::fs::File::create(&source).unwrap();
        file.set_len(64 * MIB).unwrap();
        drop(file);
        // Quelques octets au milieu d'un fichier par ailleurs vide.
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .open(&source)
                .unwrap();
            file.seek(SeekFrom::Start(32 * MIB)).unwrap();
            file.write_all(b"atelier").unwrap();
        }

        // Destination deja presente, avec un contenu plus long a ecraser.
        let destination = dir.path().join("copy");
        std::fs::write(&destination, vec![1u8; 4096]).unwrap();
        copy_sparse(&source, &destination).await.unwrap();

        assert_eq!(
            std::fs::read(&source).unwrap(),
            std::fs::read(&destination).unwrap()
        );
        let copied = std::fs::metadata(&destination).unwrap();
        assert_eq!(copied.len(), 64 * MIB);
        // `blocks` compte des blocs de 512 octets : bien moins que 64 Mio.
        assert!(
            copied.blocks() * 512 < MIB,
            "la copie occupe {} octets, les trous n'ont pas ete conserves",
            copied.blocks() * 512
        );
    }

    #[tokio::test]
    async fn copy_sparse_reports_a_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let error = copy_sparse(&dir.path().join("absent"), &dir.path().join("copy"))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("echouee"), "{error:#}");
    }
}
