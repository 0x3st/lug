"""Playwright-only server: replace the external provider, never production auth.

Run only from Playwright's isolated /tmp/lug-e2e directory. Real callback state,
PKCE/state storage, session cookies, tenant checks and API handlers remain active.
There are no test-login routes or test flags in the application itself.
"""
from pathlib import Path

if Path.cwd().resolve() != Path("/tmp/lug-e2e").resolve():
    raise RuntimeError("E2E server must run inside /tmp/lug-e2e, not a real deployment.")

import structured_ai_api as app
from platform_auth import OAuthError


def fake_exchange(code, verifier):
    if not code.isascii() or not code.isdecimal() or int(code) <= 0:
        raise OAuthError("github_unavailable")
    return {"provider": "github", "id": code, "login": "test-user-" + code}


if __name__ == "__main__":
    app.OAUTH.exchange = fake_exchange
    app.main()
