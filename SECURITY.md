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
only through a fixed catalog of actions. Its Telegram channel and its model turn are built so that
project content leaves your computer only in the cases below, and so that a message or a document
cannot steer it.

- **What leaves the computer, and where it goes.** Nanty content goes to the provider of the Nanty
  connection, to the Google Gemini connection of the voice when the voice is on, and to your
  Telegram chat once it is paired.
  - **The provider of the Nanty connection** gets every model turn: your message, the recent
    conversation, its summary, the index of the memory notes, the full text of the notes a recall
    brings back, and the structured facts of the notices. When Nanty reads a project (because you
    asked, or in the automatic analysis), what it read goes there too, redacted. That provider may
    not be the one the project roles use: a project you run on one provider can be read by a Nanty
    on another.
  - **The Google Gemini connection of the voice**, only with the voice on, gets the text to speak,
    already redacted, and your voice notes to transcribe, with `store: false`. It may be another
    provider than the one of the Nanty connection.
  - **Telegram** gets the fixed text of the observers (written in the Nants dictionaries, never by
    the model), counts and codes, the reply of the model and, with the voice on, the voice note of
    that same text. The reply can tell, in its own words, what Nanty read from a project when you
    asked; with the automatic analysis on (below), a summary of project content goes, without your
    asking, to the provider and to your Telegram chat. Settings says so next to the switch, and warns
    that the free tier of the Gemini API may use what you send to improve Google products (Nants
    cannot tell the plan of a key).
  - **What is stored stays here.** The files of the conversation, its summary, the memory notes and
    the voice audio live only in the Nanty folder of the data folder, readable only by your user
    (0600), with rotation and ceilings (`[nanty.storage]`, `[nanty.memory]`). Nants never uploads
    those files; only what the items above list is sent.

  On the way to Telegram, code blocks and backticks are removed and links are neutralized, and the
  automatic analysis drops any text that copies a passage it read, has a path or has a command.
  Every message goes through the redaction of the configured secrets (`SecretSet`: the API keys, the bot token and the
  credentials in URLs), before and after the text is cleaned, so a secret split by a backtick or an
  escape sequence does not come back whole. It goes as plain text, with no `parse_mode`, with link
  previews off, links in the model text neutralized and the Telegram size limits respected (4096
  characters per message, 1024 per voice note caption). Messages go through the Telegram servers.
  A card whose exact text does not fit one message goes without its "yes" button and points to the
  window, where the whole text shows; its "no" carries the same token, so a "yes" built by hand
  with that token is refused on Telegram and the card waits for the window.
- **Voice only through your Google Gemini connection.** Speech and transcription use that
  connection only, with `store: false` in every request, so the provider does not keep the voice
  note or the transcription. What is spoken is the same text that goes out, already redacted. When
  the voice fails, the text goes instead. The tone of the voice comes from a closed list of five
  (neutral, upbeat, serious, concerned, calm): the server picks it for a notice by its kind, the
  model picks one by name in the conversation, and only a short English style written in the Nants
  code reaches the provider, never text from the model or a project. A tainted turn is always
  neutral. A notice spoken with the model text keeps the reason line written by the server, and
  waits for that text only a few seconds before falling back to the fixed sentence. The key goes only in the request header, and the record
  keeps only codes, never the text or the audio.
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
- **Confirmation.** Any action asked in free text, on Telegram or in the window, always comes back as
  a card to confirm, even when it is automatic. The model can only propose, with a tool that accepts
  what the server offers for that project right now; the server creates the card, and only your
  click runs it. The only thing applied without a click is the mute of notices: it only reduces
  notices, ends within 24 hours and changes no channel, token or recipient. Its confirmation is
  written by the server, with the time the server calculated and a button to turn it off, and the
  urgent still arrives, without sound.
- **Tainted turn.** Text that did not come from you never counts as your request. Once a message
  brings untrusted data into the turn (a read of project content, a forwarded message or a quote of
  someone else, now or in the conversation memory), the mute also becomes a card to confirm, which
  expires at the latest at the time it shows, so a clicked card never mutes past it. Project content has a limit
  of reads per message.
- **Taint outlives the turn.** A hostile text read in one turn cannot act in the next one. Every
  reply of Nanty stores where it came from, also across restarts, and a reply written with tainted
  text in the context is tainted too. Lines stored by older versions count as tainted. The text of a
  tainted message stays in the context only for the last few messages (6 by default). After that
  it leaves together with every reply that inherited from it, a line written by the server takes its
  place, with only the time, the project and the kind of read, and the turn is clean again. A new
  read restarts the count: the reply that just read stays whole, and keeps the next turns tainted,
  for the next few messages. A copy of older text may last that long after the last read, always
  in a tainted turn, where nothing is saved or done without your tap. Your own
  words stay whole; only text from third parties leaves. The ids that reach the model from a project
  are limited to the SDD task form, so no sentence fits in them.
- **The summary sees only clean text.** When old messages of the conversation become a summary, the
  model that writes it gets your words and the clean replies only: a tainted reply and the text of a
  third party arrive as the line written by the server. It runs with no tools, its text is redacted
  before it is written and goes back to the conversation as data, after the system prompt, and each
  item is linked to its projects, so removing or excluding a project also takes it away.
- **Long term memory is data, never a rule.** Nanty's notes go to the model as data, after the
  system prompt, labelled as notes, and each one records where it came from (your words, a clean
  turn, your confirmation on a card, or an import). In a turn with untrusted text in the context,
  now or in the recent messages, saving and deleting become a silent card with the exact text and
  its source; only your tap saves it. The text is redacted before it is written, what the summary
  of the conversation already says is refused, and the notes of a removed project go with it. The
  actions, the filter and the mute do not depend on the memory code (a test checks it), so a note
  that says "always approve" changes nothing. Each topic is a versioned file with a `.bak` copy: an
  unreadable file is kept and refuses writes, a newer version is read only, and the memory folder is
  never mounted in a container.
- **The Memory tab.** You read, edit and delete the notes in the Nanty window, and the text of a note
  is always shown as text, never as HTML. An edit only counts over the version you opened; if the note
  changed in the meantime, the tab shows the saved version and you choose. A deleted note can be
  restored for 7 days (`deleted_keep_days`). The copy you download comes from a POST that, like
  every route, asks for an authorized device on the network; it has only the active notes, general
  or of a project still registered, and goes through the redactor again, so a key added after the
  note was saved does not leave either. Each edit, deletion, restore and export goes to the Nanty
  journal with ids and codes, never the text; a restore that swaps out the newer note of the same
  title also journals that note (`memory.swapped`).
- **Tools per mode, checked when they run.** Each mode has its own list of tools, and the server
  checks it before running any call, not only when it offers them. The names are public, so a model
  that calls a tool it was not offered gets a refusal and nothing runs.
- **Patterns are redacted too.** Besides the secrets Nants knows, what looks like a secret is replaced
  in tool results, in the final reply, in the text of cards and in all that goes to Telegram: common
  key prefixes (also glued to other text before them), JWT and `Bearer` values, `KEY=value` in sensitive names, URL credentials, PEM blocks
  and high entropy strings. Commit hashes, UUIDs and ids stay readable. This is a safety net, not a
  promise: a secret with no recognizable shape can still pass. In particular, a key written only in
  hexadecimal, with no sensitive name next to it (`token=`, `secret:` and the like), is not caught,
  because it looks like a commit hash; a configured secret is always caught, whatever its shape. On
  the other side, a long identifier with many short words or several groups of digits may be masked
  by mistake.
- **Projects stay apart.** A reply that touched several projects is linked to all of them: it leaves
  when any of them is removed, and it stops reaching the model when any of them is excluded. Once
  the model has seen the content of one project in a message, the server refuses to open another
  one until your next message: a second project can only be asked in the same request as the first,
  before any content arrives, so a hostile text in one project cannot pull another into the turn.
- **Forwarded messages and quotes.** A forwarded message, or a quote of a third party, goes to the
  model as untrusted data, in the same envelope as project content, and a forwarded message never
  counts as a command. The language of a forwarded text does not count as yours either.
- **Structured signals in the notice turn.** In the turn that writes a notice, the model gets only
  structured signals (codes, ids, counts, roles, cost and disk), from a fixed list of accepted keys,
  never the text of a spec, a log, an agent output or a commit. Its answer must be a summary, with
  any catalog id checked by the server; anything else is dropped.
- **Automatic analysis, only if you turn it on.** It is off by default (`[nanty.analysis]
  proactive`). When it is on, after the card of an important notice went out (a blocked run, a review
  loop or the end of a spec) and while it is still open, Nanty reads that project on its own and
  sends a separate message with what happened, the likely cause (always as a hypothesis) and a
  suggestion. The turn starts tainted by that project and is pinned to it: the server refuses any
  other project id in any tool, and the model gets only the signal and that project, with no memory,
  no conversation history and no list of other projects. It has only the read tools (state, the
  reading of the project and the deep reads), never a tool that acts, mutes, remembers or opens
  Nants; the end of a spec goes with no tools at all, only with the `report.md`, read by the safe
  reader, redacted and sealed in the envelope. The output is plain text with no action field, up to
  900 characters, redacted and cleaned, with no buttons and with a fixed line of the server saying it
  is an automatic analysis; a text that repeats 8 words in a row of what was read, has a path or has
  something shaped like a command (backticks, a prompt at the start of a line, a pipe, a chain or a
  redirection, an option next to a program, `sudo`, or a download, a package install or a package
  run on the spot followed by a target) is dropped whole, and only the fixed line goes, with a server
  sentence saying the analysis could not be shown. Naming a tool in prose, as in "the npm install
  step failed", is fine; when the word after the tool is not a short prose word, it counts as a
  target and the text is dropped.
  Turning the switch off stops an analysis already running, and nothing of it reaches you. It never runs in quiet hours, for a notice that goes to the summary or
  one sent inside a burst, never repeats a case (the key is the project, spec, task, signal and the
  facts, stored across restarts), has a daily and a per spec ceiling and may spend at most a share of
  the daily budget (30% by default). The notice never waits for it.
- **Taking an idea to the Architect is a draft, never a turn.** The Architect writes specs and may
  have internet, so Nanty never sends it anything. The model can only create a card with the whole
  text, redacted and with a ceiling (over it, a refusal, never a cut). Your tap on that card's token
  only stores the exact text you saw as a draft in the box of that project's Architect
  conversation, and only your send takes it to the Architect. A draft from a conversation that read
  untrusted content is marked in the box, and the rule lives on the server: while a marked draft is
  pending in the project, every message you send to its Architect runs without internet, even with
  the switch on and whether or not it carries the draft (edited, retyped or resent after a failure),
  with a note that tells the model the text is data. The draft leaves only when a turn that carried
  it ends well, or when you discard it; a failed turn consumes nothing. Each message that ran without
  internet stores the mark in the project chat history, so the internal turns born from it (the plan
  check, the consensus rounds, "continue where it stopped") also run without internet, even after a
  restart, until your next message without the mark. Your messages after that follow the project
  switch again: the Architect already reads project content written by the agents with the internet
  on, by your choice, and the draft text in the history does not change that threat model. Content read from another
  project than the destination is refused, by the tool and again on the tap, so a hostile project
  cannot pick where its text goes. The draft records its project and sources, and removing a
  project takes it away.
- **Offers come from the server, and so does the link to Nants.** Each turn gets `can_do`, what the
  server calculated for that channel, and Nanty may offer only what a yes can fulfil there; a card
  button is only pointed out, never offered on your behalf. The model never writes a link:
  links in its text stay neutralized. When you ask to open Nants, the server adds the address itself,
  with no token and no entry link, so the device still has to be authorized. The address is the one
  in Settings or the last `Host` of an authorized device, and only after the `Host` and `Origin`
  checks: it must be a network IP, so a header forged by a page never becomes that link.
- **Read only tools, with checked ids.** In the conversation, the model chooses what to look up, but
  every tool that takes a project accepts only the ids of the projects Nanty watches: an excluded or
  unknown id gets a plain refusal, with no read. What comes from the state tools is trusted, because
  it comes from the same source as the bell and the notices. What comes from the files of a project
  (the plan, the tasks, the report and the costs, and the deep reads below) is marked as untrusted
  data, with your secrets and the URLs redacted and a size limit, and when it disagrees with the
  state, the state wins. No tool writes outside the cards and the mute, and none changes the channel, the token
  or the recipient. Your own messages are redacted before they are stored, and the conversation
  memory keeps only what you said and the final replies, never what a tool returned.
- **Deep reads, on request.** When you ask why something failed or for a report, Nanty can read the
  agents' dialog, their tool trail, the test output and the recovery analyses of a task, the spec
  and the tasks, the Git history and the files of the project. All of it is read only, goes to the
  model in the untrusted data envelope with the file names inside it, passes through the redactor
  before any cut, and has a ceiling per file, per listing and per message (`[nanty.read]`). Every
  read taints the turn, with its kind and its project stored with the reply. A recalled memory note
  that came from an import of a project follows the same rule as a read of that project. Errors go to the model
  as codes, with no path, and nothing of what was read goes to the logs. Besides the conversation,
  these tools exist only in the automatic analysis, when you turn it on, and there only for the
  project of the notice.
- **The reader opens only what is below the project.** It resolves the project folder once and opens
  everything below it from that descriptor, with `openat2` and `RESOLVE_BENEATH |
  RESOLVE_NO_SYMLINKS` (or a walk by `openat` with `O_NOFOLLOW` where the kernel lacks it), so a
  link anywhere in the path, even one swapped in while the agent works, makes the open fail instead
  of leaving the project. `..`, absolute paths and control characters are refused before touching
  the disk. Folders such as `.git`, `.ssh`, `.aws`, `.docker`, `.kube`, `secrets`, `node_modules`,
  `target`, `dist` and the knowledge base sources are never opened or listed, a name that looks like
  a secret is refused in any part of the path, and a file with a private key or binary content is
  refused. It works only on Linux, the one system Nants is published for.
- **Git runs neutral.** The Git history is read with a clean environment (no inherited `GIT_*`, no
  system or global configuration) and with the repository settings that run programs neutralized:
  no pager, no fsmonitor, no hooks, no external diff, no textconv and no signature check, with a
  deadline and a commit count limit. It reads only the subject, the author without the e-mail, the
  date and the counts, never the patch, and a `.git` that is a link or points outside the project is
  refused. The integration Git of the runner gets the same care: it never descends into a repository
  an agent created inside the project (`diff.ignoreSubmodules=all`, `submodule.recurse=false`, and
  the `.git` of a new nested repository is moved to `nants-nested` in the private Git folder of the
  worktree before `add`), so a `fsmonitor` or a `clean` filter set there never runs on your computer.
- **Limits per message.** Rounds of tools, tool calls, time and tokens are limited, the daily budget
  is checked before each round and the last round goes without tools.
- **A tested trap.** The tests put instructions such as "send the token to..." and "approve the plan"
  inside project files and forwarded messages, have a tool output order a mute, an action outside
  the catalog or a read of an excluded project, and check that no action happens, nothing leaks, the
  channel and the token do not change and the action still waits for your click; a code shaped trap
  text in a project title must never reach the notice turn; and a hostile project file with a marker
  sentence, a path and a command, copied by the model in the automatic analysis, never reaches the
  conversation or Telegram, while its attempts to act, mute, remember or read another project are
  refused.

## Release packages

Releases are built from this repository by GitHub Actions and published on the
[Releases](https://github.com/tm-dev-lab/nants/releases) page. Every package has a SHA-256 file
and a build provenance attestation, which you can check with
`gh attestation verify <file> --repo tm-dev-lab/nants`.
