# mnml-msg-gmail

A terminal browser + composer for [Gmail](https://mail.google.com/) — list the inbox, sent, starred, and labels, search with Gmail's full query syntax, read message bodies, archive, star, and compose new mail. The first **messaging** sibling in the mnml family.

Runs **standalone in any terminal**. v0.2 will add blit-host mode so mnml can host it as a native pane (see [Not yet supported](#not-yet-supported) below).

```
┌─ gmail ───────────────────────────────────────────────────────────────┐
│ ▸1.inbox (24)  2.sent (50)  3.starred (12)  4.labels (37)  5.search   │
└───────────────────────────────────────────────────────────────────────┘
┌─ inbox (24) ──────────────────┐ ┌─ detail ────────────────────────────┐
│ ▸ Alice                       │ │ Subject  Welcome to the team        │
│   Bob                         │ │ From     "Alice" <alice@ex.com>     │
│   GitHub                      │ │ Date     Thu, 1 Jan 2026 12:34:56   │
│   Stripe                      │ │ Labels   INBOX, UNREAD              │
│   …                           │ │ ID       18f8a8c2c1234567           │
│                               │ │                                     │
│                               │ │  Body                               │
│                               │ │  Hi! Welcome aboard…                │
└───────────────────────────────┘ └─────────────────────────────────────┘
  1-9 tab · ↑↓/jk move · Enter open · / search · c compose · r refresh
  · y URL · D archive · ! star · q quit
```

## Install

```sh
cargo install --git https://github.com/chris-mclennan/mnml-msg-gmail
```

## Auth setup (one-time, ~5 minutes)

**Important:** Gmail requires OAuth 2.0, and Google **does not allow shared client IDs** for Gmail scopes the way Microsoft does for Azure CLI. **Every user has to create their own Google Cloud project + OAuth credentials.** It's the price of admission. The setup is a one-time thing.

1. **Create a Google Cloud project.**
   Go to <https://console.cloud.google.com/projectcreate> and create a new project. Any name; the project just holds the OAuth credentials.

2. **Enable the Gmail API.**
   Go to <https://console.cloud.google.com/apis/library/gmail.googleapis.com> and click "Enable" (make sure your new project is selected in the top bar).

3. **Configure the OAuth consent screen.**
   Go to <https://console.cloud.google.com/apis/credentials/consent>. Pick **User Type: External**. Fill in the App name (e.g. "mnml-msg-gmail"), your email, and required developer contact email. **Keep the app in "Testing" mode** — you don't need to publish it. (Testing mode is fine because you're the only user, but it has a caveat — see [Refresh token expiry](#refresh-token-expiry) below.)

4. **Add yourself as a Test user.**
   On the OAuth consent screen, scroll to "Test users" and add your own Gmail address. Without this, sign-in will fail with `Error 403: access_denied`.

5. **Create OAuth credentials.**
   Go to <https://console.cloud.google.com/apis/credentials> → **Create Credentials** → **OAuth client ID** → **Application type: Desktop app**. Name it anything ("mnml-msg-gmail"). Click Create. Copy the **Client ID** and **Client secret** from the modal.

6. **Export the env vars.**
   ```sh
   export GMAIL_CLIENT_ID=123456-abcdefg.apps.googleusercontent.com
   export GMAIL_CLIENT_SECRET=GOCSPX-xxxxxxxxxxxxxxxxxxxx
   ```
   (Put them in your shell rc so they survive a restart.)

7. **Run the loopback consent flow:**
   ```sh
   mnml-msg-gmail auth
   ```
   This binds a one-shot listener on `localhost:<random-port>`, opens your browser to Google's consent screen, accepts the redirect, exchanges the auth code for a refresh token, and persists everything to `~/.config/mnml-msg-gmail/token.json` (mode 0600).

   When the browser shows "You're signed in", come back to the terminal.

8. **Verify:**
   ```sh
   mnml-msg-gmail --check
   ```
   Should print your email address + message totals.

9. **Run it:**
   ```sh
   mnml-msg-gmail
   ```

### Refresh token expiry

Google enforces this rule: **refresh tokens for "Testing" mode OAuth apps expire after 7 days.** If you keep your app in Testing mode (recommended for personal use — no Google review required), you'll need to re-run `mnml-msg-gmail auth` every 7 days. The flow is fast (browser redirect, ~3 seconds end-to-end), and the token file persists so you only re-consent.

If you'd rather skip the weekly reauth, switch the OAuth consent screen to "Production" via the "Publish App" button. For Gmail scopes (`gmail.modify`, `gmail.send`), Google requires a verification review for Production apps that aren't internal to a Workspace — it takes weeks and asks for a privacy policy, a homepage, and a demo video. Not worth it for a personal tool.

## Auth flow shape

Loopback redirect (RFC 8252). Google [deprecated](https://developers.googleblog.com/en/oauth-out-of-band-oob-flow-will-be-deprecated/) the device-code grant for installed apps in 2023, so the recommended flow now is:

1. Bind a TCP listener on `localhost:0` (OS picks the port).
2. Open the browser to `https://accounts.google.com/o/oauth2/v2/auth?...&redirect_uri=http://localhost:<port>`.
3. After the user consents, Google redirects to that loopback URL with `?code=<auth_code>`.
4. The listener accepts the redirect, parses the code, exchanges it at `https://oauth2.googleapis.com/token` for an access + refresh token, and persists both.
5. The access token rotates ~hourly; the refresh token is used silently when the access token expires.

Scopes requested: `gmail.modify` (read inbox + archive + star) + `gmail.send` (compose + send).

## Subcommands

| Command | What it does |
|---|---|
| `mnml-msg-gmail` | Launch the TUI |
| `mnml-msg-gmail auth` | Run the loopback consent flow + persist the token |
| `mnml-msg-gmail auth --logout` | Delete `~/.config/mnml-msg-gmail/token.json` |
| `mnml-msg-gmail --check` | Print env vars + token state + (if valid) probe `/me/profile` |

## Config

```toml
refresh_interval_secs = 120

[[tabs]]
name = "inbox"
kind = "inbox"

[[tabs]]
name = "sent"
kind = "sent"

[[tabs]]
name = "starred"
kind = "starred"

[[tabs]]
name = "labels"
kind = "labels"

[[tabs]]
name = "search"
kind = "search"
```

### Tab kinds

| `kind` | What it shows | Notes |
|---|---|---|
| `inbox` | Messages with the `INBOX` label | Unread bold, starred yellow |
| `sent` | Messages with the `SENT` label | |
| `starred` | Messages with the `STARRED` label | |
| `labels` | All your labels with unread + total counts | Press Enter on a label to scope a transient view |
| `search` | Interactive Gmail search | `/` focuses the query input |

## Keys

| Chord | Action |
|---|---|
| `1`-`9` | Switch to that tab |
| `Tab` / `BackTab` | Cycle tabs |
| `↑` / `k`, `↓` / `j` | Move selection |
| `PgUp` / `PgDn` | Jump 10 rows |
| `g` / `G` | Top / bottom |
| `Enter` | Open focused message (loads full body) — or, on the `labels` tab, scope into that label |
| `o` | Open the focused message in the Gmail web UI |
| `y` | Yank the Gmail URL for the focused message |
| `/` | Jump to search tab + start editing the query |
| `c` | Compose new message (overlay) |
| `r` | Refresh active tab |
| `D` | Archive focused message (`y/n` to confirm) |
| `!` | Toggle STARRED on focused message |
| `q` / `Esc` / `Ctrl+C` | Quit |

### Compose overlay

| Chord | Action |
|---|---|
| `Tab` / `BackTab` | Next / prev field (To → Subject → Body) |
| `Enter` | Insert newline (in Body) or jump to next field (in To / Subject) |
| `Ctrl+Enter` or `Ctrl+S` | Send |
| `Esc` | Cancel + close overlay |
| typing | Edit the focused field |

### Search editing

| Chord | Action |
|---|---|
| typing | Edit the query |
| `Enter` | Run the search |
| `Esc` | Cancel + exit edit mode |

The query is plain Gmail search syntax — `from:alice has:attachment newer_than:7d`, `subject:invoice -from:spammer`, `in:sent to:bob@example.com`, etc.

## API endpoints used

| Tab / action | Endpoint |
|---|---|
| inbox / sent / starred | `GET /gmail/v1/users/me/messages?labelIds=...&maxResults=50` |
| label list | `GET /gmail/v1/users/me/labels` |
| search | `GET /gmail/v1/users/me/messages?q=<query>&maxResults=50` |
| per-row metadata | `GET /gmail/v1/users/me/messages/{id}?format=metadata&metadataHeaders=From,To,Subject,Date,Cc` |
| full body | `GET /gmail/v1/users/me/messages/{id}?format=full` |
| archive | `POST /gmail/v1/users/me/messages/{id}/modify` `{"removeLabelIds":["INBOX"]}` |
| star / unstar | `POST /gmail/v1/users/me/messages/{id}/modify` `{"addLabelIds":["STARRED"]}` or `{"removeLabelIds":["STARRED"]}` |
| send | `POST /gmail/v1/users/me/messages/send` `{"raw":"<urlsafe-base64 RFC 822>"}` |
| profile probe | `GET /gmail/v1/users/me/profile` |
| token exchange | `POST https://oauth2.googleapis.com/token` (`authorization_code` and `refresh_token` grants) |

All requests use bearer auth: `Authorization: Bearer <access_token>`. The access token rotates ~hourly; the client refreshes silently on a 60s skew window and on any `401 UNAUTHENTICATED` response.

## Body rendering

Gmail message bodies come back urlsafe-base64-encoded in `payload.body.data` (or, more often, inside `payload.parts[].body.data` for multipart messages). The walker prefers `text/plain`, falling back to `text/html` with a tag-strip + entity-decode pass (no full HTML parser — the same shape `mnml-msg-teams` would do).

## Layout

- **Tab strip:** one tab per `[[tabs]]` entry, with per-tab count badge.
- **Items table (left, 45%):**
  - **Messages:** `<sender name>  <subject> · <age>`. Unread → bold white; starred → yellow.
  - **Labels:** `<name>  <N unread · system|user · M total>`.
- **Detail panel (right, 55%):**
  - **Message:** Subject / From / To / Cc / Date / Labels / ID + decoded body.
  - **Label:** name / id / type / unread + total counts + an "Enter to view" hint.

## Security

- The persisted token at `~/.config/mnml-msg-gmail/token.json` grants **indefinite** Gmail access on the consented scopes (`gmail.modify` + `gmail.send`) — until you revoke at <https://myaccount.google.com/permissions> or run `mnml-msg-gmail auth --logout`.
- The token file is written with mode `0600` (owner read/write only) on Unix.
- `GMAIL_CLIENT_SECRET` is, despite the name, [not actually a secret for installed apps](https://developers.google.com/identity/protocols/oauth2#installed) — Google's docs spell out that desktop apps are expected to bundle / distribute the secret. The security model relies on the loopback redirect URI + the user's consent, not on secrecy of the client_secret. That said, treat it like any other API token — don't paste it into a public Slack channel.
- `gmail.modify` does NOT grant access to other Google Workspace surfaces (Drive, Calendar, Contacts) — it's strictly the user's own Gmail inbox.

## Not yet supported

Held back for v0.2+:

- **Attachments** — Gmail body parts of MIME type `application/*` aren't parsed or downloaded; only `text/plain` + `text/html` are surfaced.
- **Threading view** — v0.1 lists individual messages; `threadId` is present in the model but not rendered.
- **Draft save** — `c` composes a message and `Ctrl+Enter` sends it immediately; no `drafts.create` step.
- **Label management** — v0.1 lists labels and scopes into them; creating, renaming, and deleting labels is left to the web UI.
- **Multi-account** — single account at a time. Switching means moving the token file aside + re-running `auth`.
- **IMAP push / live tail** — polls every `refresh_interval_secs` (default 120s). No `users.watch` Pub/Sub subscription.
- **Reply / forward** — `c` opens a blank compose; the focused message context isn't piped in.
- **Pagination beyond `maxResults=50`** — v0.1 caps each list at 50 messages.
- **Blit-host pane mode** so mnml can host it as a native pane (the v0.1 priority follow-up).

## Run modes

### Standalone

```sh
mnml-msg-gmail
```

### Blit-host (hosted by mnml)

Not yet — v0.1 is standalone-only. v0.2 will add the `--blit <socket>` mode so mnml can launch it as a native pane (the same shape the AWS family already supports). Until then, run it in a sibling tmnl tab.

## Wire it into mnml's left rail

`mnml-msg-gmail` will ship as a default chip in mnml's rail under **INTEGRATIONS** once blit-host mode lands. For v0.1, the standalone binary is on `$PATH` after `cargo install` and the integration overlay picks it up.

## Status

**v0.1** — inbox / sent / starred / labels / search tabs · message metadata + full body (text/plain + html-strip) · archive, star, compose-and-send · loopback OAuth with refresh + 0600 token persistence · Gmail URL yank. Standalone only.

## Source

[github.com/chris-mclennan/mnml-msg-gmail](https://github.com/chris-mclennan/mnml-msg-gmail). MIT.
