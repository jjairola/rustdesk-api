# rustdesk-api

A minimal self-hosted API server for [RustDesk](https://rustdesk.com).

Users log in from the RustDesk client and **every workstation registered with
this server appears in their address book automatically**. No manual entry, no
sharing IDs around, no per-user setup.

That is the whole point of it. It is not a reimplementation of RustDesk Server
Pro — there are no groups, permissions, session recordings, audit logs, or web
console.

## How it works

RustDesk clients talk to two separate things: the **ID/relay server** (`hbbs` /
`hbbr`, which handles the actual connections) and an **API server** (this).
This one only does accounts and address books; you still need `hbbs`/`hbbr`
running for connections to work.

Two mechanisms combine:

1. **Workstations register themselves.** A client configured with an API server
   posts its ID, hostname, OS and username to `/api/sysinfo`, then heartbeats
   every ~15 seconds. Each machine becomes a row in the `devices` table. This
   happens with no user logged in — it is how the address book fills itself.

2. **Everyone reads the same shared book.** Logging in gets you two address
   books:

   | Book | Contents | Writable |
   |---|---|---|
   | **All Workstations** | every registered device, generated live | no (`rule: 1`) |
   | **My address book** | whatever you put in it | yes |

   "All Workstations" is identical for every user and cannot be edited — it's a
   view over the devices table, so it can't drift from reality. Personal books
   are private per user; aliases, tags and saved passwords persist there.

The client's **Accessible devices** tab shows the same set, as a single device
group also called "All Workstations". The user list there is deliberately empty:
the client nests peers under a user by matching `user_name` against an account
name, and since no machine belongs to a particular account here, account rows
would only ever select down to nothing.

Anything a machine reports is trusted, so **run this on a private network, a
VPN, or behind a firewall**. Any client that can reach the URL will list itself.

## Quick start

### Local

```sh
cargo build --release
./target/release/rustdesk-api user add alice     # prompts for a password
./target/release/rustdesk-api serve
```

### Docker

```sh
docker compose up -d --build
docker compose exec rustdesk-api rustdesk-api user add alice
```

The database lives in the `rustdesk-api-data` volume. For a first account
without a shell, set `RDAPI_ADMIN_USER` / `RDAPI_ADMIN_PASSWORD` in
`docker-compose.yml` — they apply on first boot only and are ignored once any
user exists, so they cannot silently reset a password later.

### Point the clients at it

In the RustDesk client: **Settings → Network → ID/Relay Server → API Server**,
set to `http://your-host:21114`. Then **Settings → Account → Login**.

Workstations only need the API Server field set — they register themselves
without anyone logging in. Machines you want to *appear* in the address book
need the setting; machines you connect *from* need it too, to log in.

## Managing it

```sh
rustdesk-api user add alice --admin --email alice@example.com
rustdesk-api user list
rustdesk-api user passwd alice
rustdesk-api user rm alice          # also drops their sessions + personal book

rustdesk-api device list            # every workstation that has reported in
rustdesk-api device rm 123456789    # returns if that machine reports again
```

Under Docker, prefix with `docker compose exec rustdesk-api`.

Passwords are hashed with Argon2id. Sessions are opaque 256-bit tokens stored
server-side, so `user rm` and `user passwd` take effect immediately.

## Configuration

All via environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `RDAPI_BIND` | `0.0.0.0:21114` | Listen address |
| `RDAPI_DB` | `sqlite://rustdesk-api.db` | SQLite path (`sqlite:///data/x.db` for absolute) |
| `RDAPI_TOKEN_TTL_DAYS` | `30` | Session lifetime |
| `RDAPI_DEVICE_STALE_DAYS` | `0` | Hide devices not seen in N days; `0` shows all |
| `RDAPI_ADMIN_USER` | — | First-boot admin, ignored once any user exists |
| `RDAPI_ADMIN_PASSWORD` | — | First-boot admin password |
| `RUST_LOG` | `rustdesk_api=info,tower_http=warn` | Log filter |

`RDAPI_DEVICE_STALE_DAYS` is worth setting if machines get reimaged often —
otherwise decommissioned workstations linger in the list until you
`device rm` them.

## Deployment notes

Three client-side quirks that will waste your afternoon if you hit them blind:

- **Plain HTTP is fine.** The client supports it, and defaults to it when
  deriving the API URL from a custom ID server.
- **Do not serve HTTPS on port 21114.** The client silently strips the port from
  `https://host:21114` unless its `allow-https-21114` option is set. Use
  `https://host` on 443 behind a reverse proxy, or plain HTTP on 21114.
- **Do not host it under a `rustdesk.com` domain.** The client treats those
  hosts as the public service and disables heartbeats entirely, so nothing would
  ever register.

Self-signed certificates work — the client retries with verification disabled
and caches that decision.

## Exposing it to the internet

Registration (`/api/sysinfo`) is **unauthenticated** — the RustDesk client sends
no credentials there, so anyone who can reach the URL can add a device to the
shared address book. On a LAN that's the whole point; on the public internet it
is not what you want.

Put a reverse proxy in front (you need one anyway — the client can't do TLS on
21114). `deploy/nginx.conf.example` and `deploy/Caddyfile.example` are complete,
commented examples that:

- terminate **TLS** on 443 and proxy to the app on `127.0.0.1:21114`;
- **rate-limit** `/api/login` against brute force;
- restrict device registration to **allowed networks** (e.g. `192.168.1.0/24`
  plus your VPN subnet), so strangers can't inject address-book entries.

Both belong at the proxy, not in the app: the proxy sees the real client IP
unspoofably, whereas the app behind it only sees `127.0.0.1` unless it trusts an
`X-Forwarded-For` header — which an attacker can forge. So keep IP allowlisting
and rate limiting at the edge.

Set `RDAPI_BIND=127.0.0.1:21114` so the app is reachable only through the proxy,
never directly. And remember the split: registration is locked to trusted
networks, but `/api/login` and the address-book reads stay open to the world —
that is remote access working — so those are protected by rate limiting, strong
passwords and TLS, not by the allowlist. A machine you want to be *controllable*
must therefore reach the server from an allowed network (i.e. over the VPN);
machines that only connect *out* don't register and are unaffected.

## The logged-in connection bug (and the shim that fixes it)

If you self-host **OSS** hbbs, logging a client in to *any* API server — this one
or another — breaks outgoing connections. Every attempt fails after exactly 18
seconds with:

```
Failed to secure tcp: deadline has elapsed: Please try later
```

Log out and connections work again. This is an upstream client bug, not an
API-server problem ([#13053](https://github.com/rustdesk/rustdesk/issues/13053),
[#12875](https://github.com/rustdesk/rustdesk/issues/12875),
[ProxmoxVE#12079](https://github.com/community-scripts/ProxmoxVE/issues/12079)).

**Cause.** `src/client.rs:429` in client 1.4.9:

```rust
if !key.is_empty() && !token.is_empty() {
    secure_tcp(&mut socket, &key).await   // waits READ_TIMEOUT = 18_000 ms
}
```

`key` never *is* empty (it falls back to a built-in), so the condition reduces
to "is the user logged in?". `secure_tcp` then waits for the rendezvous server
to send a `KeyExchange` first — which only hbbs **Pro** implements. OSS hbbs
waits for the client instead, so both sides wait and the deadline expires. The
request never leaves the machine, which is why the peer sees nothing at all and
the relay logs no request.

**Fix.** `rendezvous-shim/` is a ~60-line, zero-dependency TCP shim. The client
does not actually require encryption — a non-`KeyExchange` or even unparseable
greeting lands in its `_ => {}` arm and it proceeds unencrypted. So the shim
writes one 4-byte frame on connect and then pipes to hbbs.

Deploy it next to hbbs (see the compose service and systemd unit in that
directory). hbbs owns TCP *and* UDP 21116 and needs host networking to see real
client IPs, so the shim takes only inbound TCP via a DNAT rule:

```
client --TCP 21116--> [DNAT] --> shim :21126 --> 127.0.0.1:21116 (hbbs)
client --UDP 21116--------------------------->  hbbs, untouched
```

The control channel stays plaintext — exactly as it already is for logged-out
clients. End-to-end encryption between peers is separate and unaffected.

**Caveats.** It relies on the client tolerating a non-`KeyExchange` greeting; if
upstream tightens `secure_tcp`, it stops working. It also corrupts RustDesk's
rarely-used "TCP proxy fallback" (API calls tunnelled over 21116), which is
already broken against OSS hbbs anyway.

**Alternative**, no server changes: set `allow-websocket = 'Y'` on every client.
`secure_tcp` returns immediately when that is on. The cost is that WebSocket
mode sets `force_relay` (`src/client.rs:1863`), so all traffic is relayed
instead of going peer-to-peer.

## Tests

```sh
./tests/e2e.sh
```

Boots a real server on a temporary database and drives it with the exact request
shapes a RustDesk client sends, asserting the details the client is strict
about: `type: "access_token"` verbatim, `currentUser` returning a *bare*
payload, `total` on paged responses, `/api/ab/tags/{guid}` returning a bare
array, successful mutations returning a *zero-length* body, `forceAlwaysRelay`
as the string `"false"`, `SYSINFO_UPDATED` as literal text, and 401 only where
a client logout is actually intended.

## Endpoints

Implemented against the RustDesk 1.2.4+ protocol.

**Account** — `POST /api/login`, `POST /api/logout`, `POST /api/currentUser`,
`GET /api/login-options`

**Device registration** (unauthenticated by design; the client sends no
credentials on these) — `POST /api/sysinfo`, `POST /api/sysinfo_ver`,
`POST /api/heartbeat`

**Address book** — `/api/ab/personal`, `/api/ab/settings`,
`/api/ab/shared/profiles`, `/api/ab/peers`, `/api/ab/tags/{guid}`,
`/api/ab/peer/add/{guid}`, `/api/ab/peer/update/{guid}`, `/api/ab/peer/{guid}`,
`/api/ab/tag/add/{guid}`, `/api/ab/tag/update/{guid}`,
`/api/ab/tag/rename/{guid}`, `/api/ab/tag/{guid}`

**Accessible devices tab** — `GET /api/device-group/accessible`,
`GET /api/users`, `GET /api/peers`

**Accepted and discarded** — `/api/audit/conn`, `/api/audit/file`,
`/api/audit/alarm`, `PUT /api/audit`

**Operational** — `GET /health`

Requests to anything else are logged at `WARN`, which is the first place to look
if a future RustDesk release starts calling something new.

## Not supported

Legacy clients (1.2.3 and older) use a different address book protocol and will
not work. Shared-book write permissions, user groups, device groups, 2FA, OIDC
and web management are all absent by design.

## License

[AGPL-3.0](LICENSE), matching upstream RustDesk. This is an independent
reimplementation of the API the RustDesk client talks to — it contains no
RustDesk source code — but AGPL is used to stay aligned with the ecosystem. If
you run a modified version as a network service, you must make your source
available to its users.
