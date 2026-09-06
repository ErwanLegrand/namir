#!/usr/bin/env python3
"""Static server with cross-origin isolation, so performance.now() resolves to
5 microseconds instead of 100. The demo would NOT ship these headers -- only the
bench page needs them (D-S5.5).

Run from the spike root so both web/ and target/ are servable:
    python web/serve.py
"""
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer


class Isolated(SimpleHTTPRequestHandler):
    extensions_map = SimpleHTTPRequestHandler.extensions_map | {
        # Without this, `WebAssembly.instantiateStreaming`-style loads and even a plain
        # fetch of the module are served as application/octet-stream on some platforms.
        ".wasm": "application/wasm",
        ".js": "text/javascript",
        ".mjs": "text/javascript",
    }

    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


if __name__ == "__main__":
    print("http://127.0.0.1:8080/web/bench.html")
    # Threading: the bench page beacons its results back while a fetch may still be in
    # flight, and a single-threaded server deadlocks on that.
    ThreadingHTTPServer(("127.0.0.1", 8080), Isolated).serve_forever()
