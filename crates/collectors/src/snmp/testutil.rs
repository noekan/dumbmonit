//! Outillage partagé par les tests du collecteur SNMP.

/// Exécute une future sur un fil dédié, doté d'une pile large.
///
/// `snmp2::AsyncSession` embarque ses tampons d'émission et de réception en ligne,
/// soit cent trente kilo-octets ; les copies que la compilation de débogage insère à
/// sa construction dépassent la pile de deux mégaoctets d'un fil de test ordinaire.
/// Le correctif de fond est la fonctionnalité `heap_buffers` du crate `snmp2`, qui
/// relève du manifeste et non de ce module.
pub fn block_on_large_stack<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("construction du runtime de test")
                .block_on(future)
        })
        .expect("création du fil de test")
        .join()
        .expect("le fil de test a paniqué")
}
