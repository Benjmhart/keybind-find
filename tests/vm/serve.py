"""tests/vm/run's file server: GET serves DIR to the VM; PUT /report saves the guest's report to REPORT.

    python3 -I tests/vm/serve.py DIR REPORT

Bound to 127.0.0.1:8765 only: QEMU's user networking takes the guest's 10.0.2.2 there, and nothing is on the LAN.
"""

import functools
import http.server
import os
import sys
import tempfile

PORT = 8765
MAX = 1 << 20  # a report is a few KB


class Handler(http.server.SimpleHTTPRequestHandler):
    timeout = 30  # a guest that stops mid-request doesn't hold a thread forever

    def do_PUT(self):
        try:
            n = int(self.headers.get("Content-Length", ""))
        except ValueError:
            n = -1
        if self.path != "/report" or not 0 <= n <= MAX:
            self.send_error(400, "PUT /report, with a Content-Length of at most 1 MiB")
            return
        data = self.rfile.read(n)
        if len(data) != n:
            self.send_error(400, "the body is shorter than its Content-Length")
            return
        tmp = None
        try:
            fd, tmp = tempfile.mkstemp(dir=os.path.dirname(REPORT), prefix=".report.")
            with os.fdopen(fd, "wb") as f:
                f.write(data)
            os.replace(tmp, REPORT)
        except OSError as e:
            if tmp:
                try:
                    os.unlink(tmp)
                except OSError:
                    pass
            self.send_error(500, f"can't save the report: {e.strerror}")
            return
        self.send_response(201)
        self.send_header("Content-Length", "0")
        self.end_headers()
        print(f"vm: the guest's report is in {REPORT}", file=sys.stderr)


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: serve.py DIR REPORT")
    REPORT = sys.argv[2]
    try:
        server = http.server.ThreadingHTTPServer(
            ("127.0.0.1", PORT), functools.partial(Handler, directory=sys.argv[1])
        )
    except OSError as e:
        sys.exit(f"vm: can't listen on 127.0.0.1:{PORT}: {e.strerror}")
    server.serve_forever()
