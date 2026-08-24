# Signing in

neuton signs in with a Microsoft account through the OAuth **device code** flow:
the client shows a short code, you type it into a browser once, and every launch
after that reads a cached session from disk without touching the network.

## You need your own Azure application ID

There is no built-in client ID, and that is deliberate. Every launcher that
talks to Microsoft registers its own application. Shipping someone else's ID
would put neuton's sign-ins behind their consent screen and rate limits, and
would get that ID revoked for everyone using it.

Registering one takes about two minutes and is free:

1. Go to <https://portal.azure.com> → **Microsoft Entra ID** → **App registrations** → **New registration**
2. Name it whatever you like
3. Supported account types: **Personal Microsoft accounts only**
4. Leave the redirect URI blank, then **Register**
5. Copy the **Application (client) ID**
6. Under **Authentication**, turn **Allow public client flows** to **Yes** and save

Step 6 is the one people miss. Without it the device code request comes back
`unauthorized_client`.

## Telling neuton about it

Either works; the environment variable wins:

```sh
export NEUTON_CLIENT_ID=<your-application-id>
```

```sh
# macOS
echo <your-application-id> > ~/Library/Application\ Support/neuton/client_id
# Linux
echo <your-application-id> > ~/.config/neuton/client_id
# Windows
echo <your-application-id> > %APPDATA%\neuton\client_id
```

Then:

```sh
neuton login
```

## Where the session is kept

| Platform | Path |
| --- | --- |
| macOS | `~/Library/Application Support/neuton/session.json` |
| Linux | `~/.config/neuton/session.json` (or `$XDG_CONFIG_HOME`) |
| Windows | `%APPDATA%\neuton\session.json` |

**That file is a live credential.** Anyone who can read it can play as you until
the refresh token is revoked. It is written owner-only (`0600` on Unix) and
replaced atomically. `neuton logout` deletes it. To revoke it from Microsoft's
side instead, remove the app under
<https://account.live.com/consent/Manage>.

## The chain

```
device code ──▶ Microsoft OAuth ──▶ Xbox Live ──▶ XSTS ──▶ Minecraft services
                                                              │
                                                              ▼
                                                      profile + session token
```

Failures worth recognising:

| Symptom | Cause |
| --- | --- |
| `unauthorized_client` | Step 6 above was skipped |
| `no Xbox profile` | The account has never signed into Xbox; create one at xbox.com |
| `child account` | Needs adding to a Microsoft family group |
| `does not own Minecraft: Java Edition` | Signed in fine, but no game licence on this account |
