"""Shared plumbing for the lab fakes: a tiny stdlib HTTP server, JSON helpers,
scenario flags and request logging. Nothing here is specific to one product.

Each fake is a single script started as `python /fakes/<name>.py`; it imports
this module from the same directory. Python 3.12 standard library only.
"""

from __future__ import annotations

import json
import os
import sys
import time
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit


def scenarios() -> set[str]:
    """Failure scenarios enabled through `LAB_SCENARIO`, comma separated.

    An empty value is the healthy lab. Unknown flags are ignored so a typo
    does not stop the container; each fake logs which flags it honours."""
    raw = os.environ.get("LAB_SCENARIO", "")
    return {flag.strip().lower() for flag in raw.split(",") if flag.strip()}


def now() -> int:
    return int(time.time())


def log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


class Request:
    """What a handler needs: method, path, query (first values), form fields,
    headers and cookies, all already decoded."""

    def __init__(self, handler: BaseHTTPRequestHandler) -> None:
        parts = urlsplit(handler.path)
        self.method = handler.command
        self.path = parts.path
        self.query = {key: values[0] for key, values in parse_qs(parts.query).items()}
        self.headers = handler.headers
        length = int(handler.headers.get("Content-Length") or 0)
        body = handler.rfile.read(length) if length else b""
        self.form: dict[str, str] = {}
        content_type = handler.headers.get("Content-Type", "")
        if body and content_type.startswith("application/x-www-form-urlencoded"):
            self.form = {k: v[0] for k, v in parse_qs(body.decode("utf-8", "replace")).items()}
        self.cookies: dict[str, str] = {}
        for chunk in handler.headers.get("Cookie", "").split(";"):
            if "=" in chunk:
                key, value = chunk.strip().split("=", 1)
                self.cookies[key] = value

    def param(self, key: str, default: str = "") -> str:
        """A parameter from the query string or the form body, whichever has it."""
        return self.query.get(key, self.form.get(key, default))


class Response:
    def __init__(self, status: int, body: object, content_type: str = "application/json") -> None:
        self.status = status
        self.body = body
        self.content_type = content_type


def json_response(body: object, status: int = HTTPStatus.OK) -> Response:
    return Response(status, body)


def serve(name: str, port: int, route) -> None:
    """Run the HTTP server forever. `route(request) -> Response`."""

    class Handler(BaseHTTPRequestHandler):
        server_version = name
        sys_version = ""

        def _handle(self) -> None:
            request = Request(self)
            try:
                response = route(request)
            except Exception as error:  # a bug in the fake must be visible, not a hang
                log(f"[{name}] error on {request.method} {self.path}: {error!r}")
                response = Response(HTTPStatus.INTERNAL_SERVER_ERROR, {"error": repr(error)})
            payload = response.body
            if response.content_type == "application/json":
                payload = json.dumps(payload).encode("utf-8")
            elif isinstance(payload, str):
                payload = payload.encode("utf-8")
            self.send_response(response.status)
            self.send_header("Content-Type", response.content_type)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            log(f"[{name}] {request.method} {self.path} -> {response.status}")

        do_GET = _handle
        do_POST = _handle

        def log_message(self, *_args) -> None:  # our own line above is enough
            pass

    server = ThreadingHTTPServer(("0.0.0.0", port), Handler)
    log(f"[{name}] listening on :{port}, scenarios={sorted(scenarios()) or 'none'}")
    server.serve_forever()
