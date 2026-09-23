//! Sauvegarde et restauration d'une instance.
//!
//! Deux choses distinctes vivent ici, et il ne faut pas les confondre :
//!
//! * le **lot de configuration** ([`bundle`], [`export`], [`import`]) : un
//!   fichier unique, chiffré par une phrase de passe choisie par l'opérateur,
//!   qui contient les équipements et leurs identifiants, les règles, les canaux
//!   et leurs secrets, les pages de statut, les silences, les comptes et les
//!   jetons. Il est fait pour être restauré sur une instance **neuve** : c'est
//!   pour cela que les secrets y sont rechiffrés avec la phrase de passe et non
//!   avec la clé d'instance, qui n'existe pas encore là-bas ;
//! * les **sauvegardes locales planifiées** ([`local`]) : une copie en ligne et
//!   cohérente de la base SQLite, accompagnée de `secret.key`, écrite dans
//!   `/data/backups/` à intervalle régulier et tournée. C'est la reprise après
//!   incident sur la même machine, sans rien saisir.
//!
//! Dans les deux cas, la règle à dire et à redire : `/data/secret.key` est ce
//! qui déchiffre les identifiants des équipements. Une sauvegarde de la base
//! sans lui restaure une instance qui ne peut plus parler à rien.

pub mod bundle;
pub mod export;
pub mod import;
pub mod local;

pub use bundle::{Bundle, Envelope, FORMAT, MIN_PASSPHRASE_LEN, VERSION};
pub use export::{ExportOptions, collect};
pub use import::{RestoreOutcome, RestoreReport, SectionReport, restore};
