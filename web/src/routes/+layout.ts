/**
 * The UI is a single-page app served statically by the Rust binary. Everything
 * depends on the API at display time: nothing is server-rendered or prerendered.
 */
export const ssr = false;
export const prerender = false;
