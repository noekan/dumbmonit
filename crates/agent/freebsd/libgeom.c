/*
 * Bibliothèque factice, pour l'édition de liens croisée seulement.
 *
 * zig fournit la libc de FreeBSD, pas les autres bibliothèques de la base ;
 * `sysinfo` et `libc` en demandent quelques-unes. Compilé en bibliothèque
 * partagée au SONAME de la vraie, ce fichier permet à l'éditeur de liens
 * d'inscrire la dépendance : sur la machine FreeBSD, c'est la bibliothèque de la
 * base qui est chargée, et ce code n'est jamais exécuté. Seuls les symboles que
 * l'agent référence y figurent (voir l'étape `agent` du Dockerfile).
 */

/* /lib/libgeom.so.5 — compteurs d'E/S des disques, lus par sysinfo. */
void *geom_stats_snapshot_get(void) { return 0; }
void *geom_stats_snapshot_next(void *snapshot) { (void)snapshot; return 0; }
void geom_stats_snapshot_reset(void *snapshot) { (void)snapshot; }
void geom_stats_snapshot_free(void *snapshot) { (void)snapshot; }
int geom_stats_open(void) { return -1; }
