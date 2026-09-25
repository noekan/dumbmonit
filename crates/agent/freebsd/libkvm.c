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

/* /lib/libkvm.so.7 — table des processus et du swap, lue par sysinfo et libc. */
void *kvm_openfiles(const char *execfile, const char *corefile, const char *swapfile,
                    int flags, char *errbuf) {
    (void)execfile; (void)corefile; (void)swapfile; (void)flags; (void)errbuf;
    return 0;
}
int kvm_close(void *kd) { (void)kd; return -1; }
void *kvm_getprocs(void *kd, int op, int arg, int *count) {
    (void)kd; (void)op; (void)arg; (void)count;
    return 0;
}
char **kvm_getargv(void *kd, const void *proc, int nchr) {
    (void)kd; (void)proc; (void)nchr;
    return 0;
}
char **kvm_getenvv(void *kd, const void *proc, int nchr) {
    (void)kd; (void)proc; (void)nchr;
    return 0;
}
int kvm_getswapinfo(void *kd, void *info, int maxswap, int flags) {
    (void)kd; (void)info; (void)maxswap; (void)flags;
    return -1;
}
