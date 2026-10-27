# Security policy

## Supported versions

Only the latest released version of Nants receives security fixes. Update with the install
command before reporting:

```sh
curl -fsSL https://nants.tmdevlab.com/install.sh | sh
```

| Version         | Supported |
| --------------- | --------- |
| Latest release  | Yes       |
| Older releases  | No        |

## Reporting a vulnerability

Please report vulnerabilities privately, never in a public issue or discussion.

1. Open the [Security tab](https://github.com/tm-dev-lab/nants/security) of this repository.
2. Click **Report a vulnerability** (GitHub Security Advisories).
3. Describe the issue, the affected version (`nants --version`), the steps to reproduce and the
   impact you expect.

Do not include real API keys or other secrets in the report.

We will acknowledge your report, keep you informed while we investigate and credit you in the
advisory if you wish. Please give us reasonable time to publish a fix before disclosing the issue.

## Scope

Nants runs locally. Examples of what is in scope:

- a way for another website, device or user to reach or control the local server without the
  user's consent;
- leaks of API keys, ChatGPT plan tokens or other secrets through the interface, logs, events or
  generated files;
- ways for agent commands to escape the permission checks or the project folder;
- problems in the installer or in the verification of release packages.

## Stored tool outputs

When a command or another agent tool produces more output than fits in the agent's context, the
agent receives the start and the end, and the full output is saved in a local file so the agent
can read it later. These files stay in the `.nants-outputs` folder of the task's worktree, readable
only by your user (folder `0700`, files `0600`). They are excluded from Git and deleted with the
worktree; like any file the agent reads, only the parts it reads go to your model provider. Outside a worktree, the session folder is deleted when
the session ends. Command output may contain whatever the command printed, so treat the folder like
the project itself.

## Workspace home folder

Each command in the `workspace` container runs with your user id and a home folder inside the
task folder (`.nants-home/`), so what a tool installs there is still there for the next command.

- The folder is persistent and lives on the project disk, inside the task's worktree. It is kept
  out of Git and deleted with the worktree.
- Nants never writes credentials to it: API keys, ChatGPT plan tokens and the other secrets stay
  in Nants' own configuration, outside the project.
- Like any command output, what comes out of that folder into logs, events and agent context goes
  through the same secret redaction (`SecretSet`).
- Do not point credentials (for example a token file or a credential helper of your own) at the
  workspace or its home folder: anything there can be read by the agents' commands.

## Running the app

The App window starts the project with one click and a confirmation, in the project's `-run`
Docker Compose project (the same isolation as the tasks: your user id, `.git` and `.nants` mounted
read-only). The way it shows the program comes from what the program does: a page when a port
answers HTTP, the live log when it keeps running without a page, and the output, the exit code and
the files it created or changed when it ends.

- A program that ended keeps its stopped container only to show the result: closing the window,
  stopping the app or starting it again removes it. The paths it wrote outside the project folder
  are listed, and they go away with the container.
- The output, the command and the failure log shown in the window go through the same secret
  redaction (`SecretSet`) and are limited in size.
- **Ask the Architect to fix it** sends the Architect only the diagnosis Nants itself kept (the end
  of the log, redacted and limited), never text from the request. If the Architect proposes a
  change to the environment, it goes through the plan approval as always.

## Project Console

The Console runs a command you type inside the project's approved `workspace` container. It is
as powerful as a shell in that container: the command can read and change any file of the project
folder (except `.git` and `.nants`, mounted read-only), use the network and spend resources until
it ends. It never runs on your computer directly.

- One command per click, with the same checks as starting the app: a confirmation in the request,
  the `Host` and `Origin` check, an authorized device in local network mode and the `execute`
  readiness. Only one command runs per project at a time.
- Each command runs in a new container (`compose run --rm -T`) with your user id and the home
  folder of the workspace. There is no terminal (TTY) and the input is closed: a program that waits
  for what you type gets nothing and is stopped at the time limit (5 minutes). The live output is
  limited too.
- A command that calls `docker`, `docker compose` or `podman` is refused: the Console cannot reach
  the Docker daemon of your computer.
- The container network is not restricted (that would be a limit without evidence of need): treat
  a command typed in the Console like one you would type in the project's own container.
- The stop button ends the command at once (the container is removed). Removing the project, a new
  run of it and shutting Nants down also end it.
- Nants keeps a local history of the last 50 commands of each project, in its own data folder
  (readable only by your user), with the device and the time of each command. Commands and output
  go through the same secret redaction (`SecretSet`); repeating a command from the history asks for
  confirmation again. The history is deleted when the project is removed from Nants.

## Local network access

With local network access on (or any address outside loopback, such as `--host` with
`--allow-remote`), every request that does not come from a real loopback connection needs an
authorized device:

- a device joins with the single-use sign-in link printed in the terminal (10 minutes) or with a
  6-digit code shown on a device that is already authorized (changes every 60 seconds, single use,
  only while the add window is open, up to 10 minutes, 5 wrong attempts per window);
- the device keeps a long random token in an `HttpOnly`, `SameSite=Lax` cookie; Nants stores
  only its hash, in a file readable only by your user; revoking a device cuts its sessions at once;
- the cookie is `Lax`, not `Strict`, so that a refresh or a tab opened from another app, the
  history, a bookmark or a restored tab keeps the device signed in. With `Lax`, the browser sends
  the cookie only on top-level `GET` navigations; `POST` requests and calls from other sites
  (`fetch`, forms, images, frames) still go without it. Writes stay protected by the `Host` and
  `Origin` check, which refuses every `POST`, `PUT`, `PATCH` and `DELETE` from another site, and no
  `GET` route changes anything without its own secret in the address (`/pair?t=` and
  `/auth/finish` with `state`). A test lists every `GET` route and fails on a new one that was not
  classified, or on a route registered in a way it cannot see;
- when a page is refused, Nants sends you to the sign-in screen and, after you sign in, back to the
  page you were on (only a path inside Nants is accepted). The terminal logs why the page was
  refused, without the token or the cookie;
- on WSL in NAT mode, the Windows port redirect must point to the WSL IP, never to `127.0.0.1`.
  Pointing it to `127.0.0.1` would make other devices reach Nants as loopback, without a device.
  The script shown in Nants always uses the WSL IP.

Known limits:

- traffic on the local network is plain HTTP, not encrypted; someone on the same network who
  captures packets can see the device cookie. For the most security, use a network you trust;
- the preview ports (3001 to 3010) serve the generated app, not Nants, and do not ask for a device;
- attempts to type the code are limited to one per second for everyone, because the source IP is
  not reliable. An attacker on the network who keeps trying can block the add window while they
  insist. Devices already authorized keep working, and this computer can still sign in with the
  terminal link (restart Nants for a new one).

## ChatGPT plan connection

A connection that uses the ChatGPT plan (preview) signs in with OAuth 2.0 and has no API key:

- **Sign-in:** authorization code with PKCE (S256), as a public client, with no client secret.
  Each attempt has a new `state`, `nonce` and PKCE verifier. The `state` is single use, expires in
  10 minutes and is bound to the device that started the attempt. The redirect is the loopback
  address `http://127.0.0.1:<port>/auth/callback`, so the sign-in happens in the browser of the
  computer running Nants. In network mode, that is a loopback connection or the device authorized
  by the terminal entry link (marked `this_computer` in `devices.json` when it is created, never by
  its name); on WSL with NAT, that browser arrives with the Windows address. Devices added by code
  count as remote.
- **Return page:** the return comes from another site. With the `SameSite=Lax` device cookie, the
  browser sends the cookie on this top-level `GET`, but the flow does not depend on it, and the
  callback stays open without a device. `/auth/callback` only answers a pending `state` with a
  minimal page (no script, `Referrer-Policy: no-referrer`, `Cache-Control: no-store`, no external
  resource) that consumes nothing and moves on, from Nants itself, to `/auth/finish`, which gets
  the cookie and checks the device before it uses the `state` and exchanges the code.
- **Identity:** the ID token is checked before anything is stored: the RS256 signature against the
  OpenAI keys (JWKS), the exact issuer, the audience, the expiry and the `nonce`. The account must
  have granted the scope that allows using the plan. A later sign-in that returns a different
  client id is rejected.
- **Storage:** the tokens, the client id issued at the first sign-in, the e-mail and the account id
  are kept in `chatgpt-conn-<id>.json`, and the installation identifier in `chatgpt-host-id.json`,
  both in the Nants data folder (`~/.local/share/nants`). The files have mode 0600, are written
  atomically and are never read through a symbolic link. Tokens never go to logs, events or the
  browser.
- **Renewal:** the access token is short lived and is renewed a few minutes before it expires, one
  renewal at a time per connection. The refresh token rotates: each renewal stores the new one. If
  OpenAI rejects the refresh token, Nants deletes the tokens and asks you to sign in again.
- **Sign out:** revokes the refresh token at OpenAI and deletes the local sign-in. If OpenAI does
  not confirm the revocation, the local sign-in is still deleted and Nants says so, pointing to your
  ChatGPT account settings.

## Release packages

Releases are built from this repository by GitHub Actions and published on the
[Releases](https://github.com/tm-dev-lab/nants/releases) page. Every package has a SHA-256 file
and a build provenance attestation, which you can check with
`gh attestation verify <file> --repo tm-dev-lab/nants`.
