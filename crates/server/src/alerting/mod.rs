//! Moteur d'alerting : seuils et anti-bruit (jalon 4), anomalie et prédictif
//! (jalon 5).
//!
//! L'organisation suit une seule ligne directrice : séparer le raisonnement des
//! entrées/sorties. [`cycle::plan_cycle`] décide de tout — transitions, suppression,
//! silences, regroupement — à partir de données qu'on lui remet, et ne connaît ni
//! SQLite ni VictoriaMetrics. [`engine`] va chercher ces données et applique les
//! décisions. Le comportement du produit est donc entièrement testable hors ligne.
//!
//! Enchaînement d'un cycle :
//!
//! 1. chaque règle active est une requête MetricsQL instantanée ([`source`]) ;
//! 2. chaque série obtenue devient une empreinte, dont l'état avance selon `for`
//!    ([`machine`]) ;
//! 3. les alertes des équipements situés derrière un équipement injoignable sont
//!    supprimées ([`suppress`]) ;
//! 4. les fenêtres de maintenance font taire ce qu'elles couvrent ([`silence`]) ;
//! 5. ce qui reste est regroupé par hôte, dédupliqué, éventuellement rappelé ou
//!    escaladé ([`group`]), puis mis en forme et envoyé ([`crate::notify`]).
//!
//! Les règles d'anomalie ([`baseline`]) suivent le même chemin ; seule l'étape 1
//! change, la comparaison à un seuil étant remplacée par un écart à la baseline
//! saisonnière de la série.

pub mod baseline;
pub mod cycle;
pub mod engine;
pub mod group;
pub mod machine;
pub mod model;
pub mod rules;
pub mod silence;
pub mod source;
pub mod suppress;

pub use engine::{AlertingConfig, EvalReport, evaluate_once, spawn};
pub use group::{AlertGroup, AlertOutcome, NotifyReason};
pub use machine::{AlertState, EffectivePhase, Phase};
pub use model::{Operator, Rule, RuleKind, Severity, TargetSelector};
pub use silence::{Schedule, Silence};
