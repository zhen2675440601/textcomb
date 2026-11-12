"""Exercise default origin checks through the deployed Caddy/API stack.

Uses an unauthenticated logout request: allowed provenance must reach the
authentication layer (401); disallowed provenance must be stopped first (403).
No credentials, original documents, or model calls are involved.
"""

import http.client
import sys
from urllib.parse import urlsplit


def main():
    target = urlsplit(sys.argv[1] if len(sys.argv) > 1 else "http://localhost:3000")
    connection_type = (
        http.client.HTTPSConnection if target.scheme == "https" else http.client.HTTPConnection
    )
    cases = [
        ("HTTP IP and custom port", {"Host": "192.0.2.10:18080", "Origin": "http://192.0.2.10:18080"}, 401),
        ("HTTPS reverse proxy", {"Host": "internal.example:8080", "Origin": "https://textcomb.example", "Sec-Fetch-Site": "same-origin"}, 401),
        ("different port", {"Host": "192.0.2.10:18080", "Origin": "http://192.0.2.10:18081"}, 403),
        ("other site", {"Origin": "https://evil.example", "Sec-Fetch-Site": "cross-site"}, 403),
        ("other subdomain", {"Origin": "https://other.textcomb.example", "Sec-Fetch-Site": "same-site"}, 403),
        ("null origin", {"Origin": "null", "Sec-Fetch-Site": "same-origin"}, 403),
        ("spoofed forwarding headers", {"Host": "textcomb.example", "Origin": "https://evil.example", "X-Forwarded-Host": "evil.example", "X-Forwarded-Proto": "https"}, 403),
    ]
    for name, headers, expected in cases:
        connection = connection_type(target.hostname, target.port, timeout=10)
        try:
            connection.request("POST", "/api/v1/auth/logout", body=b"", headers=headers)
            response = connection.getresponse()
            response.read()
            if response.status != expected:
                raise SystemExit(f"{name}: expected {expected}, got {response.status}")
            if response.getheader("Access-Control-Allow-Origin") is not None:
                raise SystemExit(f"{name}: auto mode must not grant cross-origin access")
        finally:
            connection.close()
        print(f"PASS: {name}")


if __name__ == "__main__":
    main()
