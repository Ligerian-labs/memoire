#!/usr/bin/env python3
"""A signed-in owner, a team and an API key on a local development gateway.

    scripts/dev-owner.py [--server URL] [--origin URL] [--team SLUG] [--out FILE]

Registers a fresh synthetic account (owner-<random>@memoire.test) through
identity's routes, reads the verification token from the capture mail
driver (GET /v1/notifications/dev/captures, development gateways only),
creates a team and mints a team API key holding memoire's five
permissions. Everything secret goes to FILE (default
.memoire-dev/owner.json, mode 0600), never to argv, stdout or logs:

    {"server", "origin", "email", "password", "user_id", "team_id",
     "session_token", "csrf_token", "api_key", "api_key_id"}

Use it with `MEMOIRE_API_KEY=$(jq -r .api_key .memoire-dev/owner.json)`, a
bearer session (`Authorization: Bearer <session_token>` plus
`Merezha-Organization: <team_id>`), or sign in from a browser with the
email and password. Run it again for another owner; FILE is replaced.

--origin must be the gateway's `modules.identity.app_origin`
(http://localhost:4200 in config/dev/memoire.toml). Python 3 stdlib only.
"""
import argparse
import datetime
import json
import os
import secrets
import sys
import time
import urllib.error
import urllib.request

PERMISSIONS = [
    "memoire.databases.read",
    "memoire.databases.write",
    "memoire.databases.create",
    "memoire.databases.delete",
    "memoire.databases.snapshots",
]


def call(server, method, path, body=None, headers=None):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(server + path, data=data, method=method)
    if data is not None:
        request.add_header("Content-Type", "application/json")
    for name, value in (headers or {}).items():
        request.add_header(name, value)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            text = response.read()
            cookies = "; ".join(
                c.split(";", 1)[0] for c in response.headers.get_all("Set-Cookie") or []
            )
            call.cookies = cookies
            return response.status, json.loads(text) if text else None
    except urllib.error.HTTPError as error:
        text = error.read()
        try:
            return error.code, json.loads(text)
        except ValueError:
            return error.code, None


# The `name=value` pairs the last successful call's response set.
call.cookies = ""


def need(status, body, expected, what):
    if status != expected:
        code = (body or {}).get("error", {}).get("code") if isinstance(body, dict) else None
        sys.exit(f"dev-owner: {what} answered {status} {code or ''}".rstrip())
    return body


def token_for(server, email):
    """The token of the newest captured mail to `email`: the capture driver
    lists the newest first, with the recipient as `target`."""
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        status, mails = call(server, "GET", "/v1/notifications/dev/captures?limit=100")
        need(status, mails, 200, "the capture mail driver (is this a development gateway?)")
        for mail in mails or []:
            text = (mail.get("rendered") or {}).get("text") or ""
            if mail.get("target") == email and "#token=" in text:
                return text.split("#token=", 1)[1].split()[0]
        time.sleep(0.2)
    sys.exit("dev-owner: no verification mail was captured")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--server", default="http://127.0.0.1:28960")
    parser.add_argument("--origin", default="http://localhost:4200")
    parser.add_argument("--team", default=None, help="team slug (default dev-<random>)")
    parser.add_argument("--out", default=".memoire-dev/owner.json")
    args = parser.parse_args()
    server = args.server.rstrip("/")
    suffix = secrets.token_hex(4)
    email = f"owner-{suffix}@memoire.test"
    password = secrets.token_urlsafe(24)
    slug = args.team or f"dev-{suffix}"
    origin = {"Origin": args.origin}

    status, body = call(
        server, "POST", "/v1/identity/registrations", {"email": email, "password": password}, origin
    )
    if status >= 300:
        need(status, body, 202, "registration")
    # The registration's binding cookie: confirming without it claims the
    # account with no password.
    binding = call.cookies
    token = token_for(server, email)
    status, body = call(
        server,
        "POST",
        "/v1/identity/email-verifications/confirm",
        {"token": token, "transport": "bearer"},
        {**origin, "Cookie": binding},
    )
    body = need(status, body, 201, "email verification")
    if body["session"].get("password_unset"):
        sys.exit("dev-owner: the account has no password (binding cookie lost)")
    session = body["session"]["token"]
    user_id = body["user"]["id"]
    auth = {"Authorization": f"Bearer {session}"}

    status, body = call(
        server, "POST", "/v1/identity/organizations", {"name": slug, "slug": slug}, auth
    )
    team_id = need(status, body, 201, "team creation")["organization"]["id"]
    status, body = call(
        server, "PUT", "/v1/identity/session/organization", {"organization_id": team_id}, auth
    )
    if status >= 300:
        need(status, body, 200, "switching to the team")
    status, body = call(server, "GET", "/v1/identity/session", None, auth)
    csrf = (body or {}).get("csrf_token") if status == 200 else None

    expires = (
        datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=90)
    ).strftime("%Y-%m-%dT%H:%M:%SZ")
    status, body = call(
        server,
        "POST",
        f"/v1/identity/organizations/{team_id}/api-keys",
        {"name": "dev-owner", "permissions": PERMISSIONS, "expires_at": expires},
        auth,
    )
    body = need(status, body, 201, "API key creation")
    record = {
        "server": server,
        "origin": args.origin,
        "email": email,
        "password": password,
        "user_id": user_id,
        "team_id": team_id,
        "session_token": session,
        "csrf_token": csrf,
        "api_key": body["key"],
        "api_key_id": body["api_key"]["id"],
    }
    out = os.path.abspath(args.out)
    os.makedirs(os.path.dirname(out), mode=0o700, exist_ok=True)
    temporary = out + ".tmp"
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w") as file:
        json.dump(record, file, indent=2)
        file.write("\n")
    os.replace(temporary, out)

    status, body = call(server, "GET", "/v1/memoire/whoami", None, {"Authorization": f"Bearer {body['key']}"})
    body = need(status, body, 200, "whoami with the new key")
    print(json.dumps({"file": out, "email": email, "team_id": team_id, "whoami": body}))


if __name__ == "__main__":
    main()
