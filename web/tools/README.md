
# Headless screenshots for the web UI

The machine has no usable local Chromium (missing libatk). Everything runs in `zenika/alpine-chrome:with-puppeteer` (Node 20 + puppeteer + chromium, `--network host`).

## 1. Serve the build
```bash
cd web && npm run build && (nohup npm run preview -- --port 4173 --host 0.0.0.0 > /tmp/preview.log 2>&1 &)
```
`vite preview` proxies `/api` to the Rust server on :8080 (`docker compose up -d`). The Vite *dev* server is too slow for headless virtual time — always use preview.

## 2. Get a session cookie
```bash
J=/tmp/cj; curl -s -c $J -X POST -H 'content-type: application/json' -d '{"password":"<password>"}' localhost:8080/api/auth/login
COOKIE=$(grep ezymonit_session $J | awk '{print $7}')
```
If the password is unknown: `EZYMONIT_RESET_PASSWORD=1 docker compose up -d ezymonit`, then `docker compose up -d ezymonit` again, then `POST /api/auth/setup` with a new password. The login route rate-limits after a few failures (429 with a delay) — never guess passwords.

## 3. Screenshot all pages
```bash
mkdir -p web/shots
docker run --rm --network host -e COOKIE=$COOKIE -v "$PWD/web/tools:/work" -v "$PWD/web/shots:/work/shots" \
  --entrypoint node zenika/alpine-chrome:with-puppeteer /work/screenshot.js            # all pages
docker run ... /work/screenshot.js overview alerts                                       # a subset
```
Output: `web/shots/<page>-<light|dark>-<desktop|mobile>.png` (full page). Read them with the Read tool and batch every finding before editing. `web/shots/` is gitignored.

## 4. Debugging a page
- DOM after render: `docker run --rm --network host zenika/alpine-chrome --no-sandbox --virtual-time-budget=8000 --dump-dom http://localhost:4173/targets`
- Console errors: add `page.on('console', m => console.log(m.type(), m.text()))` and `page.on('pageerror', e => console.log('pageerror', e.message))` in a copy of `screenshot.js`.
- API shapes: `curl -s -b /tmp/cj localhost:8080/api/<route> | python3 -m json.tool | head`.
