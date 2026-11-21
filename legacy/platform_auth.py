"""Small, stdlib-only multi-user authentication for the Lug console.

GitHub credentials and access tokens never leave the server. Immutable GitHub
user IDs identify each workspace, independently of changeable usernames.
"""
import base64
import hashlib
import hmac
import json
import os
import secrets
import threading
import time
from urllib import error, parse, request

CALLBACK_PATH = "/f/a/o/platform/auth/github/callback"
SESSION_COOKIE = "lug_session"
STATE_COOKIE = "lug_oauth_state"
STATE_TTL = 600


class AuthConfig:
    def __init__(self, env=None):
        env = os.environ if env is None else env
        self.client_id = env.get("GITHUB_CLIENT_ID", "").strip()
        self.client_secret = env.get("GITHUB_CLIENT_SECRET", "").strip()
        self.legacy_owner_id = env.get("LUG_LEGACY_OWNER_GITHUB_ID", "").strip()
        self.public_url = env.get("LUG_PUBLIC_URL", "").strip().rstrip("/")
        if self.public_url:
            url = parse.urlsplit(self.public_url)
            local = url.hostname in ("localhost", "127.0.0.1", "::1")
            if (not url.hostname or url.username or url.password or url.path or
                    url.query or url.fragment or
                    not (url.scheme == "https" or (local and url.scheme == "http"))):
                raise ValueError("LUG_PUBLIC_URL must be an HTTPS origin (HTTP is allowed only on loopback).")
        if self.legacy_owner_id and (not self.legacy_owner_id.isascii() or
                                     not self.legacy_owner_id.isdecimal() or int(self.legacy_owner_id) <= 0):
            raise ValueError("LUG_LEGACY_OWNER_GITHUB_ID must be a positive numeric GitHub user ID.")
        if self.legacy_owner_id:
            self.legacy_owner_id = str(int(self.legacy_owner_id))

    @property
    def github_enabled(self):
        return bool(self.client_id and self.client_secret and self.public_url)

    @property
    def redirect_uri(self):
        return self.public_url + CALLBACK_PATH


def owner_id(user):
    if user.get("provider") != "github" or not str(user.get("id", "")).isascii() or not str(user.get("id", "")).isdecimal() or int(user["id"]) <= 0:
        raise ValueError("A verified GitHub identity is required.")
    return "github:" + str(int(user["id"]))


class SessionStore:
    """Opaque, expiring, revocable sessions. Stored only in server memory."""
    def __init__(self, ttl):
        self.ttl = ttl
        self.lock = threading.Lock()
        self.sessions = {}

    def create(self, user):
        owner_id(user)  # Never issue anonymous or local/admin sessions.
        token = secrets.token_urlsafe(32)
        now = time.time()
        with self.lock:
            self.sessions = {k: v for k, v in self.sessions.items() if v["expires"] > now}
            if len(self.sessions) >= 4096:
                self.sessions.pop(next(iter(self.sessions)))
            self.sessions[token] = {
                "expires": now + self.ttl,
                "csrf_token": secrets.token_urlsafe(32),
                "user": dict(user),
            }
        return token

    def get(self, token):
        with self.lock:
            record = self.sessions.get(token)
            if record and record["expires"] > time.time():
                return {**record, "user": dict(record["user"])}
            self.sessions.pop(token, None)
        return None

    def validate(self, token):
        return self.get(token) is not None

    def revoke(self, token):
        with self.lock:
            self.sessions.pop(token, None)


class OAuthError(Exception):
    pass


class _NoRedirect(request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        # Never forward credentials/Authorization to a redirected destination.
        return None


class GitHubOAuth:
    """GitHub App web application flow (user access token, not installation auth).

    GitHub Apps use permissions configured on the registration, not OAuth scopes.
    Only GET /user is called; neither access nor refresh tokens are persisted.
    """
    def __init__(self, config):
        self.config = config
        self.states = {}
        self.lock = threading.Lock()
        self.http = request.build_opener(_NoRedirect)

    def begin(self):
        if not self.config.github_enabled:
            raise OAuthError("github_not_configured")
        state = secrets.token_urlsafe(32)
        verifier = secrets.token_urlsafe(48)
        challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
        with self.lock:
            self.states = {k: v for k, v in self.states.items() if v["expires"] > time.time()}
            if len(self.states) >= 2048:
                self.states.pop(next(iter(self.states)))
            self.states[state] = {"verifier": verifier, "expires": time.time() + STATE_TTL}
        url = "https://github.com/login/oauth/authorize?" + parse.urlencode({
            "client_id": self.config.client_id,
            "redirect_uri": self.config.redirect_uri,
            "state": state,
            "code_challenge": challenge,
            "code_challenge_method": "S256",
        })
        return state, url

    def consume(self, state, browser_state):
        if not state or not browser_state or not hmac.compare_digest(state.encode(), browser_state.encode()):
            raise OAuthError("invalid_oauth_state")
        with self.lock:
            pending = self.states.pop(state, None)
        if not pending or pending["expires"] <= time.time():
            raise OAuthError("invalid_oauth_state")
        return pending["verifier"]

    def _json(self, req):
        try:
            with self.http.open(req, timeout=10) as response:
                raw = response.read(65537)
                if len(raw) > 65536:
                    raise OAuthError("github_unavailable")
                data = json.loads(raw)
                if not isinstance(data, dict):
                    raise OAuthError("github_unavailable")
                return data
        except (error.URLError, OSError, ValueError):
            raise OAuthError("github_unavailable") from None

    def exchange(self, code, verifier):
        payload = parse.urlencode({
            "client_id": self.config.client_id,
            "client_secret": self.config.client_secret,
            "code": code,
            "redirect_uri": self.config.redirect_uri,
            "code_verifier": verifier,
        }).encode()
        token = self._json(request.Request("https://github.com/login/oauth/access_token", data=payload, headers={
            "Accept": "application/json", "Content-Type": "application/x-www-form-urlencoded", "User-Agent": "Lug",
        }))
        access_token = token.get("access_token")
        if not isinstance(access_token, str) or not access_token or token.get("error"):
            raise OAuthError("github_unavailable")
        profile = self._json(request.Request("https://api.github.com/user", headers={
            "Accept": "application/vnd.github+json", "Authorization": "Bearer " + access_token,
            "User-Agent": "Lug", "X-GitHub-Api-Version": "2022-11-28",
        }))
        uid, login = profile.get("id"), profile.get("login")
        if type(uid) is not int or uid <= 0 or not isinstance(login, str) or not login:
            raise OAuthError("github_unavailable")
        user = {"provider": "github", "id": str(uid), "login": login[:100]}
        # GitHub's access token is deliberately not persisted or returned.
        return user
