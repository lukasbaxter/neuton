# Signing in

neuton signs in with a Microsoft account using the OAuth **device code** flow:
the client shows a short code, you enter it in a browser once, and every launch
afterwards reads a cached session from disk without touching the network.

Device code was chosen over a redirect flow because it needs no embedded
browser, no local HTTP listener and no custom URI scheme registration. It
behaves identically on macOS, Windows and Linux, works over SSH, and adds
nothing to the binary.

---

## For players

```sh
neuton login
```

You will see something like:

```
  sign in at   https://www.microsoft.com/link
  enter code   FJ3KD9WM

  (opened in your browser)
  use the Microsoft account that owns Minecraft.
  waiting...
```

Sign in with the Microsoft account that owns Minecraft: Java Edition. That is
normally a **personal** account. A work or school account will be rejected,
because game licences do not live on those.

You do not need an Azure account, a developer account, or anything else. You are
signing into neuton the same way you would sign into any app.

### More than one account

Shared machines and alt accounts are supported. `login` always adds a new
account rather than replacing the current one.

```sh
neuton accounts          # list them; * marks the active one
neuton switch Miji       # choose who launches
neuton logout Miji       # sign one out
neuton logout            # sign everyone out
```

### Where the sign-in is kept

| Platform | Path |
| --- | --- |
| macOS | `~/Library/Application Support/neuton/accounts.json` |
| Linux | `~/.config/neuton/accounts.json` (or `$XDG_CONFIG_HOME`) |
| Windows | `%APPDATA%\neuton\accounts.json` |

**That file is a live credential.** Anyone who can read it can play as any
account in it until those sign-ins are revoked. It is written owner-only
(`0600` on Unix) and replaced atomically. `neuton logout` deletes it.

To revoke from Microsoft's side instead, remove the app at
<https://account.live.com/consent/Manage>.

### When something goes wrong

| Symptom | Cause |
| --- | --- |
| `does not exist in tenant 'Microsoft Services'` | Signed in with a work or school account. Use the personal one that owns the game |
| `no Xbox profile` | The account has never used Xbox. Create a profile at xbox.com, then retry |
| `child account` | Needs adding to a Microsoft family group first |
| `does not own Minecraft: Java Edition` | The sign-in worked, but there is no game licence on that account |
| The code expired | Codes last about 15 minutes. Run `neuton login` again |

---

## For whoever builds neuton

Everything below is a one-time job for the person publishing builds. Players
never do any of it.

### Register one application

neuton ships **one** Azure application ID that belongs to the project, the same
way Prism Launcher and every other third-party launcher does. Players sign in
against it with their own accounts.

This is a **public OAuth client**: there is no client secret, and none is
possible for this flow. The ID is an identifier, not a credential. It is safe in
a public repository, grants nothing on its own, and is not a licence check.
Every user still authenticates as themselves and must own the game.

1. Open <https://entra.microsoft.com> → **App registrations** → **New registration**
2. Name it `neuton` (players see this name on the consent screen)
3. Supported account types: **Personal Microsoft accounts only**
4. Leave the redirect URI blank, then **Register**
5. Copy the **Application (client) ID**
6. Under **Authentication**, set **Allow public client flows** to **Yes**, and save

Steps 3 and 6 are the ones that bite. A single-tenant setting rejects exactly
the personal accounts that own Minecraft licences, and without public client
flows the device code request returns `unauthorized_client`.

### If the portal will not let you in

A personal Microsoft account has no Entra directory until one is created, and
`portal.azure.com` in that state fails with an error that looks like it is about
your app when registration has not even started:

```
AADSTS16000: User account '...' from identity provider 'live.com' does not
exist in tenant 'Microsoft Services' and cannot access the application
'74658136-14ec-4630-ad9b-26e160ff0fc6'(ADIbizaUX) in that tenant.
```

That application ID is the Azure Portal's own front end. The message means "this
account has no directory to sign into". In order of what to try:

1. Use <https://entra.microsoft.com> rather than `portal.azure.com`. It will
   offer to create a directory for a personal account.
2. Use a private window. `interaction_required` means a silent sign-in failed,
   and a work account already logged into the browser is the usual reason.
3. Sign up at <https://azure.microsoft.com/free>, which creates a "Default
   Directory". A card is requested during signup, but directories and app
   registrations are free and no subscription is needed to register an app.

### Building with it

The ID is read from the environment at compile time:

```sh
NEUTON_CLIENT_ID=<your-application-id> cargo build --release
```

A build without it still compiles and runs; it simply cannot sign in, and says
so. That keeps contributors from needing the project's ID to work on the
renderer.

At runtime the ID resolves most-specific-first, so a contributor can point a
local build at their own registration without rebuilding:

1. `NEUTON_CLIENT_ID` environment variable
2. a `client_id` file in the config directory
3. the value compiled in at build time

### Worth checking before publishing

Microsoft and Mojang publish guidance for third-party launchers that use
Microsoft accounts, and it has changed over time. I have not verified the
current terms, so confirm what applies before distributing builds publicly
rather than treating a working sign-in as permission.

---

## The chain

```
device code ──▶ Microsoft OAuth ──▶ Xbox Live ──▶ XSTS ──▶ Minecraft services
                                                              │
                                                              ▼
                                                      profile + session token
```

Only the first launch walks it. After that the session is cached, and an expired
one refreshes in a single round trip without a browser.
