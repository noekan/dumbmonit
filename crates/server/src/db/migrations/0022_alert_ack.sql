-- Acquittement d'une alerte : « je sais, ne me le rappelle plus pendant 4 h ».
--
-- Un silence couvre un équipement entier pendant une fenêtre ; l'acquittement
-- porte sur une seule alerte, le temps qu'on s'en occupe. Ce n'est pas une
-- phase de la machine à états : l'alerte reste `firing`, la condition continue
-- d'être suivie, seuls les rappels se taisent. La résolution, elle, est
-- toujours annoncée, et efface l'acquittement — une alerte qui revient plus
-- tard notifie à nouveau (voir `alerting/machine.rs` et `db/alerts.rs`).
ALTER TABLE alert_state ADD COLUMN acked_until TEXT;   -- fin de l'acquittement, RFC 3339 UTC
ALTER TABLE alert_state ADD COLUMN acked_by    TEXT;   -- compte (ou `token:…`) qui a acquitté
ALTER TABLE alert_state ADD COLUMN ack_note    TEXT;   -- pourquoi, en une ligne
