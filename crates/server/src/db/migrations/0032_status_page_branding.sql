-- Pages de statut : habillage et abonnés.
--
-- L'habillage reste une liste fermée de réglages — jamais de CSS ni de HTML
-- libres : la page publique est servie depuis la même origine que
-- l'administration, sous la même politique de contenu.
--
-- `accent` : une teinte parmi un petit jeu vérifié en clair comme en sombre.
ALTER TABLE status_pages ADD COLUMN accent TEXT NOT NULL DEFAULT 'default';
-- Texte libre de pied de page, affiché comme du texte (jamais interprété).
ALTER TABLE status_pages ADD COLUMN footer_text TEXT NOT NULL DEFAULT '';
-- Lien vers le site de l'organisation (http ou https), vide : pas de lien.
ALTER TABLE status_pages ADD COLUMN homepage_url TEXT NOT NULL DEFAULT '';
-- Type MIME du logo déposé (`image/png`, `image/jpeg`, `image/webp`), NULL :
-- pas de logo. Le fichier vit sous `<data>/status-pages/`.
ALTER TABLE status_pages ADD COLUMN logo_type TEXT;
-- Canal SMTP qui envoie les courriels aux abonnés. NULL : pas d'abonnement,
-- la page ne propose que le flux RSS.
ALTER TABLE status_pages ADD COLUMN subscribe_channel_id INTEGER
    REFERENCES notification_channels(id) ON DELETE SET NULL;
-- Origine vue par l'administrateur quand il a enregistré la page : base des
-- liens des courriels quand aucune URL publique n'est réglée. Jamais tirée
-- d'une requête publique, dont l'en-tête `Host` est au choix du visiteur.
ALTER TABLE status_pages ADD COLUMN link_origin TEXT NOT NULL DEFAULT '';

-- Abonnés par courriel aux annonces d'une page. Double confirmation : une
-- adresse n'est prévenue qu'une fois `confirmed_at` posé par le lien reçu.
-- `token` sert à la confirmation puis au désabonnement en un clic.
CREATE TABLE status_subscribers (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    page_id      INTEGER NOT NULL REFERENCES status_pages(id) ON DELETE CASCADE,
    email        TEXT    NOT NULL,
    token        TEXT    NOT NULL UNIQUE,
    confirmed_at TEXT,
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE (page_id, email)
);

CREATE INDEX idx_status_subscribers_page ON status_subscribers(page_id, confirmed_at);
