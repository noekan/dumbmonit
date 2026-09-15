#!/bin/sh
# Vérification isolée, TEMPORAIRE — à supprimer avant livraison.
#
# Plusieurs agents travaillent en parallèle sur ce dépôt et laissent par moments
# `notify/`, `api/` et `collectors/agent` dans un état qui ne compile pas. Ce
# script recopie l'arbre dans /work à l'intérieur du conteneur, y réduit la
# bibliothèque au seul module des collecteurs, et n'exécute que les tests du
# collecteur Synology. Le dépôt réel n'est jamais modifié.
set -e

rm -rf /work
mkdir -p /work
(cd /build && tar --exclude=./target -cf - .) | (cd /work && tar xf -)

printf 'pub mod collectors;\n' > /work/crates/server/src/lib.rs
printf 'pub mod synology;\n'   > /work/crates/server/src/collectors/mod.rs

cd /work
# La commande cargo est passée en argument : « test --lib synology » par défaut.
CARGO_TARGET_DIR=/build/target cargo ${@:-test --lib synology}
