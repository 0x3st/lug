"""Auth/tenant-isolation tests. No real GitHub requests or production data writes."""
import base64
import hashlib
import http.client
from http.cookies import SimpleCookie
import io
import json
import os
from pathlib import Path
import tempfile
import threading
import time
import unittest
from unittest.mock import Mock, patch
from urllib.parse import parse_qs, urlsplit

# Importing the server must not migrate a developer's real key store.
with patch.dict(os.environ, {"LUG_LEGACY_OWNER_GITHUB_ID": ""}):
    import structured_ai_api as app
from platform_auth import AuthConfig, GitHubOAuth, OAuthError, SessionStore, CALLBACK_PATH

CONFIG = {
    "LUG_PUBLIC_URL": "https://lug.example",
    "GITHUB_CLIENT_ID": "test-client",
    "GITHUB_CLIENT_SECRET": "server-only-secret",
}


class AuthUnitTests(unittest.TestCase):
    def test_configuration_is_closed_without_credentials_but_not_admin_only(self):
        self.assertFalse(hasattr(AuthConfig({"PLATFORM_PASSWORD": "ignored"}), "password"))
        self.assertFalse(AuthConfig({}).github_enabled)
        self.assertTrue(AuthConfig(CONFIG).github_enabled)
        for url in ["http://lug.example", "https://user:pass@lug.example", "https://lug.example/path", "https://lug.example?x=y"]:
            with self.assertRaises(ValueError):
                AuthConfig({"LUG_PUBLIC_URL": url})
        self.assertTrue(AuthConfig({**CONFIG, "LUG_PUBLIC_URL": "http://127.0.0.1:8125"}).github_enabled)

    def test_state_is_bound_one_time_and_uses_pkce(self):
        flow = GitHubOAuth(AuthConfig(CONFIG))
        state, url = flow.begin()
        query = parse_qs(urlsplit(url).query)
        self.assertNotIn("scope", query)  # GitHub Apps use configured permissions, not scopes.
        self.assertEqual(query["redirect_uri"], [CONFIG["LUG_PUBLIC_URL"] + CALLBACK_PATH])
        self.assertNotIn("server-only-secret", url)
        with self.assertRaises(OAuthError):
            flow.consume(state, "different-browser")
        verifier = flow.consume(state, state)
        challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
        self.assertEqual(query["code_challenge"], [challenge])
        self.assertEqual(query["code_challenge_method"], ["S256"])
        with self.assertRaises(OAuthError):
            flow.consume(state, state)
        expired, _ = flow.begin()
        flow.states[expired]["expires"] = time.time() - 1
        with self.assertRaises(OAuthError):
            flow.consume(expired, expired)
        with self.assertRaises(OAuthError):
            flow.consume("非ASCII", "different-browser")

    def test_github_exchange_accepts_different_users_and_keeps_tokens_server_side(self):
        flow = GitHubOAuth(AuthConfig(CONFIG))
        for uid in (101, 202):
            responses = [io.BytesIO(json.dumps({"access_token": "ghu_test-token", "refresh_token": "ghr_test-refresh", "expires_in": 28800, "scope": ""}).encode()),
                         io.BytesIO(json.dumps({"id": uid, "login": "developer", "email": "private@example.test"}).encode())]
            flow.http.open = Mock(side_effect=responses)
            user = flow.exchange("one-time-code", "pkce-verifier")
            self.assertEqual(user, {"provider": "github", "id": str(uid), "login": "developer"})
            token_req, profile_req = [call.args[0] for call in flow.http.open.call_args_list]
            self.assertEqual(token_req.full_url, "https://github.com/login/oauth/access_token")
            payload = parse_qs(token_req.data.decode())
            self.assertEqual(payload["client_secret"], ["server-only-secret"])
            self.assertEqual(payload["code_verifier"], ["pkce-verifier"])
            self.assertEqual(profile_req.full_url, "https://api.github.com/user")
            self.assertEqual(profile_req.get_header("Authorization"), "Bearer ghu_test-token")
            self.assertNotIn("ghu_", json.dumps(user))
            self.assertNotIn("ghr_", json.dumps(user))
            self.assertNotIn("secret", json.dumps(user))
            self.assertNotIn("email", user)

    def test_github_errors_are_sanitized(self):
        flow = GitHubOAuth(AuthConfig(CONFIG))
        for response in [{"error": "bad_verification_code", "error_description": "secret"}, [], {"access_token": None}]:
            flow.http.open = Mock(return_value=io.BytesIO(json.dumps(response).encode()))
            with self.assertRaisesRegex(OAuthError, "^github_unavailable$"):
                flow.exchange("code", "verifier")
        flow.http.open = Mock(side_effect=TimeoutError("secret from upstream"))
        with self.assertRaisesRegex(OAuthError, "^github_unavailable$"):
            flow.exchange("code", "verifier")

    def test_session_expiration_revocation_and_github_only_identity(self):
        store = SessionStore(60)
        user = {"provider": "github", "id": "101", "login": "developer"}
        token = store.create(user)
        self.assertTrue(store.validate(token))
        store.sessions[token]["expires"] = time.time() - 1
        self.assertFalse(store.validate(token))
        token = store.create(user)
        store.revoke(token)
        self.assertFalse(store.validate(token))
        for invalid in [{}, {"provider": "password", "id": "local"}, {"provider": "github", "id": "0"}]:
            with self.assertRaises(ValueError):
                store.create(invalid)

    def test_corrupt_store_is_not_silently_reset(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "keys.json"
            path.write_text("{ broken JSON", encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "refusing to overwrite"):
                app.KeyStore(str(path))
            self.assertEqual(path.read_text(), "{ broken JSON")

    def test_legacy_migration_is_explicit_and_persistent(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "keys.json"
            store = app.KeyStore(str(path))
            raw, old = store.create("old", "local")
            del old["owner_id"]  # Pre-multi-user file format.
            store._save()
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            store = app.KeyStore(str(path))
            self.assertEqual(len(store.list("local")), 1)
            self.assertEqual(store.list("github:101"), [])
            self.assertIsNone(store.revoke(old["id"], "github:101"))
            store = app.KeyStore(str(path), "101")
            self.assertEqual(len(store.list("github:101")), 1)
            self.assertEqual(store.list("local"), [])
            self.assertIsNotNone(store.validate(raw))
            store = app.KeyStore(str(path), "202")
            self.assertEqual(len(store.list("github:101")), 1)
            self.assertEqual(store.list("github:202"), [])


class Client:
    def __init__(self, port):
        self.port = port
        self.cookies = {}

    def request(self, method, path, data=None, headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        sent = {"Connection": "close"}
        if self.cookies:
            sent["Cookie"] = "; ".join(f"{k}={v}" for k, v in self.cookies.items())
        body = None
        if data is not None:
            body = json.dumps(data)
            sent["Content-Type"] = "application/json"
        sent.update(headers or {})
        connection.request(method, path, body, sent)
        response = connection.getresponse()
        status, response_headers, raw = response.status, response.getheaders(), response.read()
        connection.close()
        for name, value in response_headers:
            if name.lower() == "set-cookie":
                jar = SimpleCookie(value)
                for key, cookie in jar.items():
                    if cookie["max-age"] == "0":
                        self.cookies.pop(key, None)
                    else:
                        self.cookies[key] = cookie.value
        content_type = dict(response_headers).get("Content-Type", "")
        parsed = json.loads(raw) if "application/json" in content_type else raw.decode()
        return status, dict(response_headers), parsed


class AuthHTTPTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.config = AuthConfig(CONFIG)
        self.flow = GitHubOAuth(self.config)
        self.sessions = SessionStore(3600)
        self.keys = app.KeyStore(str(Path(self.directory.name) / "keys.json"))
        self.patches = patch.multiple(app, AUTH=self.config, OAUTH=self.flow, SESSIONS=self.sessions,
                                      KEYS=self.keys)
        self.patches.start()
        self.quiet = patch.object(app.Handler, "log_message")
        self.quiet.start()
        self.server = app.ThreadingHTTPServer(("127.0.0.1", 0), app.Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.port = self.server.server_address[1]
        self.client = Client(self.port)

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.quiet.stop()
        self.patches.stop()
        self.directory.cleanup()

    def login(self, client, uid, login="developer"):
        status, headers, _ = client.request("GET", "/f/a/o/platform/auth/github")
        self.assertEqual(status, 302)
        query = parse_qs(urlsplit(headers["Location"]).query)
        state = query["state"][0]
        with patch.object(self.flow, "exchange", return_value={"provider": "github", "id": str(uid), "login": login}):
            status, headers, _ = client.request("GET", f"{CALLBACK_PATH}?state={state}&code=code-{uid}")
        self.assertEqual(status, 302)
        self.assertEqual(headers["Location"], "/platform")
        self.assertIn("HttpOnly", headers["Set-Cookie"])
        self.assertIn("Secure", headers["Set-Cookie"])
        self.assertIn("SameSite=Lax", headers["Set-Cookie"])
        self.assertNotIn("token", headers["Location"])
        status, headers, me = client.request("GET", "/f/a/o/platform/me")
        self.assertEqual(status, 200)
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertNotIn("Access-Control-Allow-Origin", headers)
        return {"X-CSRF-Token": me["csrf_token"]}

    def test_multi_user_key_and_usage_isolation_including_idor(self):
        a, b = self.client, Client(self.port)
        ah, bh = self.login(a, 101, "alice"), self.login(b, 202, "bob")
        legacy_raw, legacy = self.keys.create("local-only", "local")
        self.assertEqual(a.request("GET", "/f/a/o/platform/keys")[2]["data"], [])
        status, _, alice = a.request("POST", "/f/a/o/platform/keys", {"name": "alice-key", "owner_id": "github:202"}, ah)
        self.assertEqual(status, 200)
        _, _, bob = b.request("POST", "/f/a/o/platform/keys", {"name": "bob-key"}, bh)
        for client, own, other, headers in [(a, alice, bob, ah), (b, bob, alice, bh)]:
            listed = client.request("GET", "/f/a/o/platform/keys")[2]["data"]
            self.assertEqual([k["id"] for k in listed], [own["id"]])
            self.assertNotIn("key_hash", listed[0])
            self.assertNotIn("owner_id", listed[0])
            for hidden_id in (other["id"], legacy["id"], "does-not-exist"):
                self.assertEqual(client.request("DELETE", "/f/a/o/platform/keys/" + hidden_id, headers=headers)[0], 404)
        for owner, expression in [(alice, "3+5*2**(1+1)"), (bob, "1+1"), (bob, "2+2")]:
            self.assertEqual(a.request("POST", "/f/a/o/calculate", {"expression": expression}, {"Authorization": "Bearer " + owner["key"]})[0], 200)
        self.assertEqual(a.request("GET", "/f/a/o/platform/usage")[2]["total"]["requests"], 1)
        self.assertEqual(b.request("GET", "/f/a/o/platform/usage")[2]["total"]["requests"], 2)
        self.assertEqual(a.request("DELETE", "/f/a/o/platform/keys/" + alice["id"], headers=ah)[0], 200)
        self.assertIsNone(self.keys.validate(alice["key"]))
        self.assertIsNotNone(self.keys.validate(bob["key"]))
        self.assertIsNotNone(self.keys.validate(legacy_raw))

    def test_workspace_survives_relogin_username_change_and_server_store_reload(self):
        headers = self.login(self.client, 101, "old-name")
        _, _, created = self.client.request("POST", "/f/a/o/platform/keys", {"name": "persistent"}, headers)
        reloaded = app.KeyStore(self.keys.path)
        self.assertEqual(reloaded.list("github:101")[0]["id"], created["id"])
        old_token = self.client.cookies["lug_session"]
        self.login(self.client, 101, "new-name")
        self.assertFalse(self.sessions.validate(old_token))
        self.assertEqual(self.client.request("GET", "/f/a/o/platform/keys")[2]["data"][0]["id"], created["id"])
        self.assertEqual(self.client.request("GET", "/f/a/o/platform/me")[2]["user"]["login"], "new-name")

    def test_cookie_writes_need_csrf_and_same_origin(self):
        headers = self.login(self.client, 101)
        for sent in [{}, {"X-CSRF-Token": "wrong"}, {**headers, "Origin": "https://evil.example"}, {**headers, "Sec-Fetch-Site": "cross-site"}]:
            self.assertIn(self.client.request("POST", "/f/a/o/platform/keys", {"name": "blocked"}, sent)[0], (401, 403))
        self.assertEqual(self.keys.list("github:101"), [])
        self.assertEqual(self.client.request("POST", "/f/a/o/platform/keys", {"name": "allowed"}, {**headers, "Origin": "https://lug.example"})[0], 200)
        # A tab from the previous account must not silently load the new account's data.
        old_headers = headers
        self.login(self.client, 202)
        self.assertEqual(self.client.request("GET", "/f/a/o/platform/keys", headers=old_headers)[0], 401)

    def test_logout_revokes_session_on_server(self):
        headers = self.login(self.client, 101)
        token = self.client.cookies["lug_session"]
        self.assertEqual(self.client.request("POST", "/f/a/o/platform/logout", headers=headers)[0], 200)
        self.assertNotIn("lug_session", self.client.cookies)
        self.assertFalse(self.sessions.validate(token))
        self.assertEqual(self.client.request("GET", "/f/a/o/platform/me", headers={"X-Session-Token": token})[0], 401)

    def test_callback_rejects_missing_wrong_and_replayed_state(self):
        with patch.object(self.flow, "exchange") as exchange:
            status, headers, _ = self.client.request("GET", CALLBACK_PATH + "?code=attacker-code&state=missing")
            self.assertEqual(status, 302)
            self.assertIn("invalid_oauth_state", headers["Location"])
            exchange.assert_not_called()
        state, _ = self.flow.begin()
        self.client.cookies["lug_oauth_state"] = state
        with patch.object(self.flow, "exchange", return_value={"provider": "github", "id": "101", "login": "user"}) as exchange:
            callback = f"{CALLBACK_PATH}?code=secret-code&state={state}"
            self.assertEqual(self.client.request("GET", callback)[1]["Location"], "/platform")
            self.client.cookies["lug_oauth_state"] = state
            self.assertIn("invalid_oauth_state", self.client.request("GET", callback)[1]["Location"])
            self.assertEqual(exchange.call_count, 1)

    def test_cancel_and_provider_outage_return_safe_errors_without_sessions(self):
        for suffix, expected in [("error=access_denied&error_description=private", "github_cancelled"), ("code=one-use-code", "github_unavailable")]:
            state, _ = self.flow.begin()
            self.client.cookies["lug_oauth_state"] = state
            with patch.object(self.flow, "exchange", side_effect=OAuthError("github_unavailable")):
                status, headers, _ = self.client.request("GET", f"{CALLBACK_PATH}?state={state}&{suffix}")
            self.assertEqual(status, 302)
            self.assertEqual(headers["Location"], "/platform?login_error=" + expected)
            self.assertNotIn("lug_session", self.client.cookies)
            self.assertNotIn("lug_oauth_state", self.client.cookies)

    def test_password_route_removed_even_with_old_environment_variable(self):
        with patch.dict(os.environ, {"PLATFORM_PASSWORD": "old-password"}):
            with patch.object(app, "AUTH", AuthConfig()):
                for password in ["old-password", "admin123"]:
                    self.assertEqual(self.client.request("POST", "/f/a/o/platform/login", {"password": password})[0], 404)
                self.assertEqual(self.client.request("GET", "/f/a/o/platform/login")[0], 404)
        self.assertNotIn("lug_session", self.client.cookies)
        self.assertEqual(self.sessions.sessions, {})

    def test_unauthenticated_access_and_public_configuration(self):
        for path in ("keys", "usage", "me"):
            self.assertEqual(self.client.request("GET", "/f/a/o/platform/" + path)[0], 401)
        self.assertEqual(self.client.request("POST", "/f/a/o/platform/keys", {"name": "unauthorized"})[0], 401)
        self.assertEqual(self.client.request("DELETE", "/f/a/o/platform/keys/anything")[0], 401)
        status, _, data = self.client.request("GET", "/f/a/o/platform/auth")
        self.assertEqual(status, 200)
        self.assertEqual(data, {"github_enabled": True})
        self.config.client_secret = ""
        self.assertIn("github_not_configured", self.client.request("GET", "/f/a/o/platform/auth/github")[1]["Location"])


if __name__ == "__main__":
    unittest.main()
