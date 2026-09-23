const puppeteer = require('/usr/src/app/node_modules/puppeteer');
// Usage (from the repo root, preview server on :4173, cookie from a curl login):
//   docker run --rm --network host -e COOKIE=<session> -e BASE=http://localhost:4173 \
//     -v "$PWD/web/tools:/work" -v "$PWD/web/shots:/work/shots" --entrypoint node \
//     zenika/alpine-chrome:with-puppeteer /work/screenshot.js [page-name ...]
const COOKIE = process.env.COOKIE || '';
const BASE = process.env.BASE || 'http://localhost:4173';
const ONLY = process.argv.slice(2);
// FULL=0 shoots the viewport only (1440×900): the size of the README / docs assets.
const FULL = process.env.FULL !== '0';
const SIZES = process.env.DESKTOP_ONLY ? [[1440, 900, 'desktop']] : [[1440, 900, 'desktop'], [390, 844, 'mobile']];
const pages = [
  ['device-pmg', '/targets/36'], ['device-pdm', '/targets/37'],
  ['device-pve', '/targets/27'], ['device-pbs', '/targets/28']
];
(async () => {
  const browser = await puppeteer.launch({ executablePath: '/usr/bin/chromium-browser', args: ['--no-sandbox', '--disable-gpu'] });
  const page = await browser.newPage();
  if (COOKIE) await page.setCookie({ name: 'dumbmonit_session', value: COOKIE, domain: 'localhost', path: '/', httpOnly: true });
  for (const theme of ['light', 'dark']) {
    for (const [w, h, tag] of SIZES) {
      await page.setViewport({ width: w, height: h, deviceScaleFactor: 1 });
      // Entry screens redirect when a session cookie is present: shoot them without COOKIE.
      const gated = new Set(['login', 'setup']);
      for (const [name, path] of pages.filter(([n]) => (ONLY.length ? ONLY.includes(n) : !(COOKIE && gated.has(n))))) {
        await page.evaluateOnNewDocument((t) => { try { localStorage.setItem('dumbmonit-theme', t); } catch {} }, theme);
        await page.goto(BASE + path, { waitUntil: 'networkidle0', timeout: 30000 }).catch(e => console.log('goto', path, e.message));
        await new Promise(r => setTimeout(r, 1500));
        try {
          await page.screenshot({ path: `/work/shots/${name}-${theme}-${tag}.png`, fullPage: FULL });
          console.log('shot', name, theme, tag);
        } catch (e) {
          console.log('failed', name, theme, tag, e.message);
        }
      }
    }
  }
  await browser.close();
})();
