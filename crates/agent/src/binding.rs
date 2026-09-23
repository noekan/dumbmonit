//! Secret de liaison de la machine à son enregistrement.
//!
//! Le jeton d'enregistrement dit que cette machine a le droit de parler ; il est
//! partagé par tout un parc et ne dit pas *laquelle* parle. La clé d'identité
//! (`/etc/machine-id`, ou le nom d'hôte) le dit, mais n'est un secret pour
//! personne : la lire sur une machine suffisait à pousser des mesures au nom
//! d'une autre et à venir prendre ses commandes Docker.
//!
//! Le secret de liaison referme cela. Le serveur l'attribue à la première
//! présentation, ne garde que son empreinte, et l'agent le conserve ici — à côté
//! de sa configuration, avec les mêmes permissions qu'elle.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, warn};

/// Nom du fichier, à côté de `agent.yaml`.
pub const SECRET_FILE: &str = "agent-secret";

/// Le secret tel que l'agent le détient, et l'endroit où il le range.
#[derive(Debug, Clone)]
pub struct Binding {
    path: PathBuf,
    secret: Option<String>,
}

impl Binding {
    /// Relit le secret sur le disque. Un fichier absent n'est pas une erreur :
    /// c'est l'état d'une machine qui n'est pas encore enrôlée.
    pub fn load(path: &Path) -> Self {
        let secret = match std::fs::read_to_string(path) {
            Ok(text) => {
                let secret = text.trim().to_string();
                if secret.is_empty() { None } else { Some(secret) }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                // Illisible plutôt qu'absent : on le dit, et on repart comme si
                // la machine n'était pas liée. Le serveur tranchera.
                warn!(path = %path.display(), %error, "binding secret unreadable, ignored");
                None
            }
        };
        Self { path: path.to_path_buf(), secret }
    }

    pub fn secret(&self) -> Option<&str> {
        self.secret.as_deref()
    }

    /// Range le secret que le serveur vient d'attribuer.
    ///
    /// Un échec d'écriture n'arrête pas l'agent : il garde le secret en mémoire
    /// et continue de remonter ses mesures. Mais il le dit fort, car au
    /// prochain démarrage la machine reviendra sans secret et devra être
    /// réautorisée à la main.
    pub fn save(&mut self, secret: &str) {
        self.secret = Some(secret.to_string());
        if let Err(error) = write_private(&self.path, secret) {
            warn!(
                path = %self.path.display(),
                error = %format!("{error:#}"),
                "cannot store the binding secret: this agent will need to be re-authorised \
                 from the DumbMonit interface after its next restart"
            );
            return;
        }
        debug!(path = %self.path.display(), "binding secret stored");
    }

    /// Oublie le secret, sur le disque comme en mémoire.
    ///
    /// Appelé quand le serveur répond que cette liaison ne lui dit rien : garder
    /// un secret qu'il a oublié ne sert à rien, et l'agent doit pouvoir se
    /// relier dès que quelqu'un ouvre la fenêtre dans l'interface.
    pub fn forget(&mut self) {
        if self.secret.take().is_none() {
            return;
        }
        match std::fs::remove_file(&self.path) {
            Ok(()) => debug!(path = %self.path.display(), "binding secret forgotten"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!(path = %self.path.display(), %error, "cannot remove the binding secret");
            }
        }
    }
}

/// Écrit un secret dans un fichier que le propriétaire seul peut lire.
///
/// Sous Unix, les permissions sont posées à la création : écrire puis corriger
/// laisserait une fenêtre, si brève soit-elle, où le secret serait lisible par
/// tout le monde. Sous Windows, le fichier hérite des droits du répertoire, que
/// l'installateur restreint à SYSTEM et aux administrateurs — il n'y a pas
/// d'équivalent direct de `0600` sans dépendance supplémentaire.
fn write_private(path: &Path, secret: &str) -> Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).with_context(|| format!("opening {}", path.display()))?;
    file.write_all(secret.as_bytes()).with_context(|| format!("writing {}", path.display()))?;
    file.write_all(b"\n").ok();
    // Le secret doit survivre à une coupure de courant : sans cela, une machine
    // qui redémarre juste après son enrôlement reviendrait non liée.
    file.sync_all().ok();
    Ok(())
}

/// Emplacement du secret pour une configuration donnée : juste à côté d'elle.
pub fn beside(config_path: &Path) -> PathBuf {
    match config_path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(SECRET_FILE),
        _ => PathBuf::from(SECRET_FILE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_secret_is_simply_an_unbound_machine() {
        let dir = tempfile::tempdir().expect("répertoire");
        let binding = Binding::load(&dir.path().join("agent-secret"));
        assert_eq!(binding.secret(), None);
    }

    #[test]
    fn a_stored_secret_comes_back_and_is_readable_by_nobody_else() {
        let dir = tempfile::tempdir().expect("répertoire");
        let path = dir.path().join("agent-secret");
        let mut binding = Binding::load(&path);
        binding.save("dmab_abcdef");
        assert_eq!(binding.secret(), Some("dmab_abcdef"));
        // Relu depuis le disque : c'est le cas du redémarrage de l'agent.
        assert_eq!(Binding::load(&path).secret(), Some("dmab_abcdef"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("métadonnées").permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "le secret ne doit être lisible que par lui");
        }
    }

    #[test]
    fn forgetting_a_secret_removes_it_from_the_disk_too() {
        let dir = tempfile::tempdir().expect("répertoire");
        let path = dir.path().join("agent-secret");
        let mut binding = Binding::load(&path);
        binding.save("dmab_abcdef");

        binding.forget();
        assert_eq!(binding.secret(), None);
        assert!(!path.exists(), "un secret oublié ne doit pas rester en clair sur le disque");
        // Oublier deux fois n'est pas une erreur.
        binding.forget();
    }

    #[test]
    fn a_secret_written_with_stray_whitespace_is_read_back_clean() {
        let dir = tempfile::tempdir().expect("répertoire");
        let path = dir.path().join("agent-secret");
        std::fs::write(&path, "  dmab_abcdef \n").expect("écriture");
        assert_eq!(Binding::load(&path).secret(), Some("dmab_abcdef"));

        std::fs::write(&path, "   \n").expect("écriture");
        assert_eq!(Binding::load(&path).secret(), None, "un fichier vide ne lie rien");
    }

    #[test]
    fn the_secret_sits_next_to_the_configuration() {
        assert_eq!(
            beside(Path::new("/etc/dumbmonit/agent.yaml")),
            PathBuf::from("/etc/dumbmonit/agent-secret")
        );
        assert_eq!(beside(Path::new("agent.yaml")), PathBuf::from("agent-secret"));
    }
}
