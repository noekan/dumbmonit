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

/* /lib/libdevstat.so.7 — version de l'interface devstat, vérifiée par sysinfo. */
int devstat_getversion(void *kd) { (void)kd; return -1; }
