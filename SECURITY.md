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
- a way for someone other than the paired owner to talk to Nanty or make it act, or for project
  content to make Nanty act without your click;
- problems in the installer or in the verification of release packages.

## Stored tool outputs

When a command or another agent tool produces more output than fits in the agent's context, the
agent receives the start and the end, and the full output is saved in a local file so the agent
can read it later. These files stay in the `.nants-outputs` folder of the task's worktree, readable
only by your user (folder `0700`, files `0600`). They are excluded from Git and deleted with the
worktree; like any file the agent reads, only the parts it reads go to your model provider. Outside a worktree, the session folder is deleted when
the session ends. Command output may contain whatever the command printed, so treat the folder like
the project itself.

## Provider failure log

When a model provider fails for good (after the retries), Nants appends one JSON line to
`.sdd/specs/<spec>/provider-errors.jsonl` in the project, or to `.sdd/specs/.provider-errors.jsonl`
for a failure outside a spec (the conversation with the Architect).

- **What it keeps:** the time, the project, the spec or stage, the role, the connection (id, name
  and type), the model, the task and attempt, the Nants reason and category, the HTTP status, the
  provider's error type, code and parameter, the request id, the number of retries and the
  provider's error message.
- **What it does not write:** Nants does not write the request it sent. The provider's message
  may quote parts of it; it is cut at 600 characters, and the message, the model and the
  connection go through the same redaction of the configured secrets (`SecretSet`: the API keys
  and the credentials in URLs) as logs and events. That redaction does not cover arbitrary
  content quoted by the provider, so treat the file like the project itself.
- **Where it shows:** the provider's message stays only in this file; the screen gets the fixed
  reason and the structured fields (status, error type, code and parameter, request id).
- **Where it lives:** the files are kept out of the project's repository (`.git/info/exclude`) and
  stop growing at 1 MiB each.

## Knowledge base

Each project can keep a knowledge base of documents and links in `.sdd/kb/`: the original files
(`sources/`), the extracted text as markdown notes (`notes/`), the list of documents
(`manifest.json`) and an optional overview written by you (`overview.md`).

- **Out of Git:** the whole folder is kept out of the project's repository through
  `.git/info/exclude`; `.gitignore` is not touched. If the exclude cannot be written safely,
  nothing is stored.
- **Refused on entry:** typical secret file names (`.env` and its variants, PEM and other private
  key files, SSH private keys, credential files) and contents with a PEM private key block or one
  of the configured secrets. Only markdown, text and PDF files are accepted.
- **Limits:** one file up to 32 MiB (the upload body limit, there to protect the server, not to
  cap the base) and the overview up to 32 KiB.
- **Redaction:** the note text goes through the redaction of the configured secrets
  (`SecretSet`) when it is written and again when it is read. Titles and file names are redacted
  before they reach the manifest and the note.
- **Extraction:** PDF files and links are read by one background queue, one document at a time,
  with a time limit per document (a file that takes too long ends as `error`, and the queue moves
  on). PDF text is read by a pure Rust library, without C code. A link is read once, through the
  same web client as the Architect's web access: private and local network addresses are refused,
  redirects are checked one by one, and the page is read up to 2 MiB. Extracted text that carries a
  private key or a configured secret is refused and the original is removed.
- **Files:** nothing is written through a symbolic link; the manifest is replaced atomically.
- **Content and titles are data:** the text, the titles, the section headings and the
  descriptions of the documents are always data for the agents, never instructions. They reach
  the prompts marked as untrusted data, kept on one line where they are labels, cut to a length
  limit and redacted. Only what the approved spec transcribes, with the citation, binds the
  agents.
- **The Overview is your text:** the Overview is written only by you, in the interface, and
  reaches the Architect as your intent (it is where you declare which documents are company
  standards), outside the untrusted data warning that covers the documents. It is still redacted
  and capped. Anyone who can write the Overview can steer the plan, as they could through the
  Conversation; the plan still needs your approval.
- **Mentions are your intent:** a `@` mention counts only when it comes from your own messages in
  the Conversation; the Architect's replies and internal turns never count. The mentioned
  sections and outlines go with the turn as untrusted data, never as a system message, with a
  size limit, redaction and the data markers neutralized inside the text; they are not stored in
  the conversation history. A mention of a document that does not exist only shows a warning.
- **Written documents:** documents written in Nants go through the same path as uploads (no
  symbolic links, atomic writes, redaction, refusal of content with a secret), up to 2 MiB. An
  edit carries the version it started from, and a newer version on disk is a conflict, never a
  silent overwrite. The editor opens the original text only for markdown and text documents; PDFs
  and links are refused, and so is a document whose text the redaction would change, so a
  redacted copy can never be saved over the original.
- **Limits of the redaction:** redaction replaces the secrets you configured (API keys and the
  other values Nants knows) and refuses PEM private key blocks. It does not detect other
  sensitive data: personal data, credentials you never configured, tokens in an unknown format,
  text inside images or scanned pages. Treat the base as content that may be sent to your AI
  provider in the excerpts the agents read, and do not add documents you would not send there.
  Redaction is a safety net, not a classifier.
- **Interface:** the Knowledge window shows the note already redacted. Indicators (duplicate,
  no headings, broken internal link) are computed by the server from the manifest and the index,
  without a model. The search highlights are computed by the server after redaction and sent as
  positions; the interface builds them with text nodes, never by injecting HTML. Links outside the
  base, in the document view and in **Links to**, are clickable only for `http` and `https`; any
  other scheme (`javascript:`, `file:`) shows as plain text.

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

## Container limits

A command written by a model can go wrong in ways that hurt the computer, such as a test runner
that is also a test and starts itself again and again until memory runs out. Every container
Nants starts for a project (the Developer's commands, the tests and checks, the app, the Console
and the browser tests) has two limits:

- **Memory:** half of the computer's RAM (`MemTotal`), with no swap beyond it. The other half is
  left for the system, Docker, Nants and your desktop. Above the limit, the kernel stops the
  process inside the container, not on your computer.
- **Processes and threads:** one per MiB of that memory, between 1024 and 4096. Normal parallel
  builds stay well below it; a process recursion stops in seconds.
- **Project values:** the limits go in an override file Nants passes to Docker Compose after the
  project's `compose.yaml`. A lower `mem_limit` or `pids_limit` in the project (or in
  `deploy.resources.limits`) still applies; a higher one, none, `-1` or `oom_kill_disable: true`
  does not turn the protection off. The override is not part of the environment approval hash.
- **Time limit:** when a command goes past its time limit, Nants removes its container
  (`docker rm -f`), which ends every process in it. Stopping only the `docker compose` client is
  not enough: the container keeps running on the Docker daemon.
- **Limit reached:** a command that ends with code 137 or an out of memory error, or whose output
  shows that a process or a thread could not be created, comes back to the agent and in the test
  output saying which limit it hit and its value.
- The command for running the project outside Nants
  (`docker compose -f .nants/compose.yaml run --rm workspace sh -lc '<command>'`) uses only the
  limits of the project's own `compose.yaml`.

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

## API keys (OpenAI compatible, Anthropic and Google Gemini)

The Anthropic (Claude) and Google Gemini (AI Studio) connections keep their key exactly like the
OpenAI compatible ones:

- **Storage:** the key, or an `env:NAME` reference that is read from the environment when it is
  used, is stored only in the configuration file (`~/.config/nants/config.toml`, mode 0600). It
  never goes to the project, the workspace, logs, events or the browser, which only
  sees a masked form.
- **Where it goes:** only to the provider address of the connection: the official one
  (`https://api.anthropic.com` in the `x-api-key` header, `https://generativelanguage.googleapis.com`
  in the `x-goog-api-key` header) or the advanced address you set, for example a company proxy.
  Changing the address of a saved connection requires typing the key again, so a saved key is
  never sent to a new host.
- **Test before saving:** **Test** sends the key from the browser to Nants once, in the request
  body, to list the models; nothing is saved until **Save connection**.
- **Errors:** provider errors become fixed codes (invalid key, no permission, rate limit, spend
  limit, daily quota); the provider's text and the key never reach the screen.

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

## Nanty and Telegram

Nanty is the system agent that watches the projects and the machine, tells you on Telegram and acts
only through a fixed catalog of actions. Its Telegram channel and its model turn are built to keep
your projects on your computer and to keep a message or a document from steering it.

- **Only summaries leave the computer.** What goes to Telegram is a short summary: the fixed text of
  the observers (written in the Nants dictionaries, never by the model), counts and codes, or the
  reply of the model. It never carries code, commands, log lines or secrets. Every message goes
  through the redaction of the configured secrets (`SecretSet`: the API keys, the bot token and the
  credentials in URLs), before and after the text is cleaned, so a secret split by a backtick or an
  escape sequence does not come back whole. It goes as plain text, with no `parse_mode`, with link
  previews off, links in the model text neutralized and the Telegram size limits respected (4096
  characters per message, 1024 per voice note caption). Messages go through the Telegram servers.
- **No open port.** Nants reads Telegram by long polling (`getUpdates`), never by webhook, so nothing
  listens for the internet. Reading and sending use separate clients with no redirects, and the
  Telegram address is fixed in the code (only tests change it, never the configuration).
- **One owner.** Pairing uses a single use code shown only on the Nants screen, sent through the
  `t.me/<bot>?start=<code>` link. The code changes every 2 minutes and is locked after 5 wrong
  attempts. It only works in a private chat, and it pins both the user id and the chat id of the
  owner. In a group, the bot ignores the message and leaves. Messages and button clicks from anyone
  else are ignored and recorded. Unpairing, or a new token, forgets the chat, the owner and the
  read position.
- **The bot token is a secret.** It is stored like the API keys, in the configuration file (mode
  0600), or as an `env:NAME` reference read when it is used. Only a masked form reaches the browser,
  and it never comes back whole. The token is part of the request address, so every error from the
  HTTP client drops the address and is redacted before it is written anywhere, and the address is
  never logged.
- **One reader per bot.** Only one Nants process per data folder runs Nanty (a lock in the data
  folder). Telegram accepts only one reader per bot, so the same token in two installations at the
  same time shows as a conflict; each installation should have its own bot.
- **Nants closed.** With Nants closed, nobody reads Telegram. When it opens again, the messages that
  waited go into the conversation as cards marked as old and a button tapped in the meantime never
  decides anything: no action comes from a message that waited.

### Prompt injection

Nanty is limited by construction, so a message or a file of a project cannot make it do what you
did not confirm:

- **Fixed catalog.** Nanty acts only through a fixed list of actions run by the same handlers as the
  screen. The model can only name ids of that list; the server checks the id, the level of the
  action and the current state (a card has an expiry, the first click counts, and the approval card
  checks a hash of the plan before starting). Actions that write a permanent permission or change
  the channel, the token or the chat are only yours, on the screen, and cannot be made automatic.
- **Confirmation.** Anything asked in free text, on Telegram or in the window, always comes back as a
  card to confirm, even when the action is automatic.
- **Structured signals when it acts on its own.** In a proactive turn, the model gets only
  structured signals (codes, ids, counts, roles, cost and disk), from a fixed list of accepted keys,
  never the text of a spec, a log, an agent output or a commit. Its answer must be a summary plus
  catalog ids, checked by the server; anything else is dropped.
- **Reading on request, limited to the project you name.** Only when you ask about a project does
  Nanty read it: the plan, the tasks, the report and the costs of that project only, never the logs
  and never another project, marked as untrusted data, with your secrets and the URLs redacted and a
  size limit. Your own messages are redacted before they are stored.
- **A tested trap.** The tests put instructions such as "send the token to..." and "approve the plan"
  inside project files and check that no action happens, nothing leaks and the action still waits
  for your click; a code shaped trap text in a project title must never reach a proactive turn.

## Release packages

Releases are built from this repository by GitHub Actions and published on the
[Releases](https://github.com/tm-dev-lab/nants/releases) page. Every package has a SHA-256 file
and a build provenance attestation, which you can check with
`gh attestation verify <file> --repo tm-dev-lab/nants`.
