# Signing in

neuton signs in with a Microsoft account through the OAuth **device code** flow:
the client shows a short code, you type it into a browser once, and every launch
after that reads a cached session from disk without touching the network.

## You need your own Azure application ID

There is no built-in client ID, and that is deliberate. Every launcher that
talks to Microsoft registers its own application. Shipping someone else's ID
would put neuton's sign-ins behind their consent screen and rate limits, and
would get that ID revoked for everyone using it.

Registering one is free. It needs an **Entra directory** (formerly Azure AD),
which is not the same thing as an Azure subscription and does not cost anything.

### If you have never used Azure with this account

A personal Microsoft account has no Entra directory until one is created. Going
straight to `portal.azure.com` in that state fails with a confusing error that
looks like it is about the app you are trying to register:

```
AADSTS16000: User account '...' from identity provider 'live.com' does not
exist in tenant 'Microsoft Services' and cannot access the application
'74658136-14ec-4630-ad9b-26e160ff0fc6'(ADIbizaUX) in that tenant.
```

That application ID is the Azure Portal's own front end, not anything of yours.
The message means "this account has no directory to sign in to".

Fixes, in the order worth trying:

1. Open <https://entra.microsoft.com> instead. It is the newer entry point and
   will offer to create a directory for a personal account.
2. Use a private or incognito window. `interaction_required` in that error means
   a silent sign-in failed, and a work account already signed into the browser is
   a common cause.
3. If neither works, sign up at <https://azure.microsoft.com/free>. This creates
   a "Default Directory" for the account. A card is asked for during signup, but
   directories and app registrations are free and no subscription is needed to
   register an app.

### Registering the app

1. <https://entra.microsoft.com> → **App registrations** → **New registration**
2. Name it whatever you like
3. Supported account types: **Personal Microsoft accounts only**
4. Leave the redirect URI blank, then **Register**
5. Copy the **Application (client) ID**
6. Under **Authentication**, turn **Allow public client flows** to **Yes** and save

Step 6 is the one people miss. Without it the device code request comes back
`unauthorized_client`.

Step 3 matters too: a single-tenant setting rejects the personal account that
actually owns your Minecraft licence.

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
| `AADSTS16000` / `ADIbizaUX` | The portal, not neuton. The account has no Entra directory yet |
| `does not exist in tenant 'Microsoft Services'` | Signed in with a work account, or no directory exists |
| `no Xbox profile` | The account has never signed into Xbox; create one at xbox.com |
| `child account` | Needs adding to a Microsoft family group |
| `does not own Minecraft: Java Edition` | Signed in fine, but no game licence on this account |
