-- Liaison d'un agent à sa machine, et portée des jetons d'enregistrement.
--
-- Jusqu'ici, un jeton d'enregistrement ouvrait tout : la clé d'identité que
-- l'agent présente (`/etc/machine-id` ou son nom d'hôte) n'est un secret pour
-- personne, et une machine compromise du parc pouvait donc pousser des mesures
-- au nom d'une autre, réécrire son nom d'hôte, et surtout venir chercher — donc
-- consommer — les commandes Docker destinées à sa voisine.
--
-- Le secret de liaison referme cela : le serveur l'attribue à la première
-- présentation d'une machine, n'en garde que l'empreinte, et exige ensuite de le
-- voir dans chaque requête de cette machine.

-- Empreinte SHA-256 du secret de liaison. NULL tant que la machine n'est pas
-- liée : c'est l'état des agents installés avant cette version, qui continuent
-- de remonter leurs mesures et s'affichent « not bound yet » dans l'interface.
ALTER TABLE agent_hosts ADD COLUMN secret_hash TEXT;

-- Moment de la liaison, pour que l'interface puisse dire depuis quand.
ALTER TABLE agent_hosts ADD COLUMN bound_at TEXT;

-- Vrai si le binaire de l'agent sait recevoir un secret de liaison. Un agent
-- plus ancien l'ignorerait et se verrait refuser son lot suivant : le serveur ne
-- lui en attribue donc aucun, et ce drapeau est ce qui distingue, dans
-- l'interface, « va se lier au prochain lot » de « binaire à mettre à jour ».
ALTER TABLE agent_hosts ADD COLUMN binding_supported INTEGER NOT NULL DEFAULT 0;

-- Fenêtre pendant laquelle une machine déjà liée peut se relier avec un jeton
-- valide : c'est ce qu'ouvre le bouton « Allow re-enrolment » de l'interface,
-- après une réinstallation qui a emporté le secret. Bornée dans le temps, parce
-- qu'une autorisation permanente reviendrait à ne pas lier du tout.
ALTER TABLE agent_hosts ADD COLUMN rebind_until TEXT;

-- Nombre d'enrôlements que ce jeton peut encore servir. NULL : sans limite,
-- c'est le jeton de parc, réutilisable par autant de machines qu'on veut.
-- Les jetons existants deviennent des jetons de parc : ils le sont déjà en
-- pratique, et une mise à jour du serveur ne doit pas interrompre un
-- déploiement en cours.
ALTER TABLE agent_tokens ADD COLUMN max_uses INTEGER;

-- Machines déjà enrôlées par ce jeton. Compté à l'enrôlement, jamais à chaque
-- lot : un jeton à usage unique doit continuer de laisser passer les mesures de
-- la machine qu'il a fait entrer.
ALTER TABLE agent_tokens ADD COLUMN uses INTEGER NOT NULL DEFAULT 0;

-- Date après laquelle ce jeton n'enrôle plus. Les machines déjà enrôlées
-- continuent de remonter : une échéance sert à fermer une fenêtre
-- d'installation, pas à éteindre un parc — pour cela il y a la révocation.
ALTER TABLE agent_tokens ADD COLUMN expires_at TEXT;

-- Le compteur part de la réalité : les machines déjà rattachées à un jeton
-- comptent comme des enrôlements, sans quoi lui poser une limite plus tard
-- partirait de zéro.
UPDATE agent_tokens
SET uses = (SELECT COUNT(*) FROM agent_hosts WHERE agent_hosts.token_id = agent_tokens.id);
