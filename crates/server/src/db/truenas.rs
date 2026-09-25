//! Dernière vue de chaque sonde TrueNAS.
//!
//! Le collecteur TrueNAS livre à chaque interrogation ce qu'il a vu des pools,
//! des jeux de données, des disques, des tâches de protection et des alertes
//! (`dumbmonit_collectors::truenas::ProbeView`). La vue est réécrite entière :
//! rien ne s'accumule ici, l'historique vit dans la base de séries.

use anyhow::{Context, Result};
use dumbmonit_collectors::truenas::ProbeView;
use dumbmonit_proto::TargetId;
use sqlx::{Row, SqlitePool};

/// Enregistre ce qu'une interrogation a vu, en remplaçant la vue précédente.
pub async fn record_probe(pool: &SqlitePool, target_id: TargetId, view: &ProbeView) -> Result<()> {
    let json = serde_json::to_string(view).context("sérialisation de la vue TrueNAS")?;
    sqlx::query(
        "INSERT INTO truenas_probe_view (target_id, probed_at, view) VALUES (?, ?, ?)
         ON CONFLICT(target_id) DO UPDATE SET
            probed_at = excluded.probed_at,
            view = excluded.view",
    )
    .bind(target_id)
    .bind(view.probed_at)
    .bind(json)
    .execute(pool)
    .await
    .context("enregistrement de la vue TrueNAS")?;
    Ok(())
}

/// La dernière vue d'une cible ; `None` avant la première interrogation réussie.
pub async fn load_view(pool: &SqlitePool, target_id: TargetId) -> Result<Option<ProbeView>> {
    let row = sqlx::query("SELECT view FROM truenas_probe_view WHERE target_id = ?")
        .bind(target_id)
        .fetch_optional(pool)
        .await
        .context("lecture de la vue TrueNAS")?;
    row.map(|row| {
        let json: String = row.get("view");
        serde_json::from_str(&json).context("vue TrueNAS illisible")
    })
    .transpose()
}
