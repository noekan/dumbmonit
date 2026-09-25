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

/* /usr/lib/libprocstat.so.1 — fichiers ouverts d'un processus, lus par sysinfo. */
void *procstat_open_sysctl(void) { return 0; }
void procstat_close(void *procstat) { (void)procstat; }
void *procstat_getfiles(void *procstat, void *kp, int mmapped) {
    (void)procstat; (void)kp; (void)mmapped;
    return 0;
}
void procstat_freefiles(void *procstat, void *head) { (void)procstat; (void)head; }
