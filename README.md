<p align="center">
  <img src="docs/images/nants-symbol.svg" width="96" alt="Nants symbol">
</p>

<h1 align="center">Nants</h1>

<p align="center"><strong>Software built by agents. Steered by you.</strong></p>

<p align="center">You describe the idea. The agents handle the engineering.</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="License: Apache-2.0"></a>
  <a href="https://github.com/tm-dev-lab/nants/releases"><img src="https://img.shields.io/github/v/release/tm-dev-lab/nants?include_prereleases&sort=semver&label=version&color=8b5cf6" alt="Latest version"></a>
  <a href="https://github.com/tm-dev-lab/nants/actions/workflows/ci.yml"><img src="https://github.com/tm-dev-lab/nants/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
</p>

<p align="center">
  <a href="https://nants.tmdevlab.com">Website</a> &middot;
  <a href="#install">Install</a> &middot;
  <a href="https://github.com/tm-dev-lab/nants/issues/new/choose">Report an issue</a>
</p>

<p align="center">
  <img src="docs/images/desktop-en.webp" width="900" alt="Nants running a finished project: the final report, the generated app and the conversation with the agents">
</p>

## What is Nants

Building software with AI agents still means babysitting prompts, reviewing every change by hand
and hoping the result runs. Nants turns that into an engineering process: an Architect agent turns
your idea into a specification, a Developer builds it task by task and a Reviewer checks every
change against the acceptance criteria. It all runs on your computer, in your own Git repository,
and you decide at every step.

## How it works

1. **Describe** what you want to build in the Conversation.
2. **The Architect asks and plans.** It asks the questions that matter and writes a specification
   with tasks, acceptance criteria and the test command.
3. **You approve.** Nothing is written until you approve the plan.
4. **The Developer builds and the Reviewer checks.** Each task is built in an isolated worktree,
   tested and reviewed; only approved work reaches your project.
5. **Run the app.** The Architect writes a final report and Nants runs the result so you can try it,
   whatever kind of program it is: a web app, a service, a command line tool or a script.

<p align="center">
  <img src="docs/images/conversation-en.webp" width="640" alt="The Architect asking questions with suggested answers">
</p>

## Features

- **Agents with roles.** Architect, Developer and Reviewer work from a specification with
  acceptance criteria, and every task runs in an isolated Git worktree.
- **The whole spec for every role.** The Architect writes in each task the titles of the spec
  sections it implements. Each Developer and Reviewer briefing carries the spec's index (every
  heading and its size) and whole sections, never cut in the middle: first the ones the task cites,
  then the assumptions, then the rest. A section that does not fit stays in the index, and the
  Developer, the Reviewer and the final check read it whole with a read-only tool, shown in the
  Activity as "reading the spec section".
- **Docker for every project.** Tests, checks and the running app live in containers, with
  nothing installed on your system and no `sudo`. Each command runs in a fresh container, and the
  home folder lives inside the task folder (`.nants-home/`, with `~/.local/bin` on the `PATH`), so
  whatever a tool installs there is still there for the next command, in any language. It stays
  out of Git, of the formatters and of the task's files, starts empty in the final clean run
  (only the download cache is kept) and goes away with the task folder. Before the first task of
  a spec, Nants runs the install command once and lists what it wrote outside the project folder;
  if that goes past a small limit, the conversation, the spec and every Developer briefing get a
  warning with the main paths, since those files are lost between commands. An install that
  fails leaves that check inconclusive: it is never recorded as done.
- **Review findings with a history.** Each Reviewer finding keeps its id from one review to the
  next and shows as new, still open (for how many reviews) or resolved, and the Developer gets the
  oldest open ones first. Before delivering, the Developer answers each one: fixed, test added or
  contested with a reason. The Architect rules on a contest once: keep the finding, adjust the
  criterion to what the environment can prove, or turn it into a documented manual check. The
  ruling is recorded in the spec and the Reviewer follows it. A finding that comes back is the same
  one, reopened with its old id, when it points to the same files or when the Reviewer says it
  continues an earlier one. From the third review on, a new MEDIUM or LOW gap in code that has not
  changed since the last rejection, on a criterion that was already met, no longer holds the task
  back: it becomes a note for the final check. A finding with a concrete failure scenario (an input
  and the wrong result) still blocks. The Developer also gets the spec's guidance for the Reviewer,
  and for each criterion it marks as met it names the evidence (a test, or a file and line).
- **Recovery with more options.** When a task gets stuck, the Architect can retry with a new
  strategy, split the task into smaller ones (keeping the work already on the branch), adjust the
  criteria to what the environment proves, or ask you a question in the Conversation.
- **A plan the agents agree on.** Before the plan reaches you, the Reviewer checks it and the
  Architect answers each finding: it adjusts the plan, or keeps it with a reason (for example, a
  check the sandbox cannot run, or one that only adds criteria). The Reviewer remembers those
  answers and does not raise a declined point again without a new argument. They go back and forth
  for at most 3 rounds. The plan card then shows a summary: how many findings there were, how many
  were adjusted and kept (with the Architect's reasons, folded), the size of the plan against its
  first version, and any point still open or in dispute, with both positions. **Ask for a fix**
  sends only those points to the Architect, or you approve anyway. With nothing open, the plan
  shows as ready. When the back and forth changed the plan, the Conversation also shows the final
  list of tasks.
- **A plan that says how to see it working.** No field the Architect writes in the spec header is
  dropped in silence: one that would be lost (for example, a `run_command` over 500 characters) is
  refused with the reason in the same turn. With no `run_command`, the Architect is asked how you
  will see the product working, and asks you when it does not know, unless the plan records that
  there is nothing to run (`nothing_to_run`, for example a library used only by its tests). A
  criterion that cites a spec field the plan does not declare is flagged too; during the run, such
  a criterion becomes a note for the final check and never goes back to the Developer.
- **Tasks numbered in run order.** The plan lists the tasks in the order they will run. When you
  approve, they are renumbered in that order (T01 runs first), every reference follows, and the
  map from old to new ids goes into the spec and the Conversation.
- **Task size and cost.** The plan review flags tasks that are too large, such as many criteria or
  server, client and end to end tests in one task, and suggests how to split them. The Architect
  splits them during the same back and forth; what is still too large shows up highlighted on the
  approval card, and you can approve anyway. When a task
  spends far more than the others (3 times the median of the finished tasks of the spec, or a fixed
  threshold of 8 million tokens or 400 calls), the Board and the Activity show a quiet notice and
  the Architect gets the signal in its next decision. It is only a signal: the run never stops for it.
- **Tests that fit the environment.** Nants measures what the test environment can prove (GPU or
  video device, display, cores, memory and how long the test suite takes) and tells the agents, so
  a criterion that needs something missing becomes a documented manual check. When the suite gets
  close to its time limit, you get a warning to split or focus it. A failed round is run once more
  with no change, and if it passes the task goes on with a flaky test alert.
- **Tests that are not repeated for nothing.** When the Developer runs the project's exact quick
  test command or a blocking check and it passes on the code it delivers, Nants skips that run and
  says so in the task and the conversation. The full suite always runs once more by Nants, with
  the environment recreated, before review: that clean run is the one that counts, and the one
  integration relies on. Checks taken from the Developer run again at integration. "The same code" means the same tree
  hash: every file Git would see in the task folder, tracked or not, with its content and mode,
  except what `.gitignore` ignores (installed dependencies, build output), `.sdd/` and
  `.nants-outputs/`. It also has to be the same environment, the run must have passed within the
  time limit, and no run of that command on that tree may have failed. When in doubt, Nants runs it.
  For a slow suite, a project can have a quick `test_command` for every attempt and a
  `full_test_command` that runs before review and at integration. With `install_command` and
  `lockfiles`, dependencies are installed before every test or check run only when a lock file
  changes, both in the task worktree and on the base branch; the final clean run before review
  always installs from scratch, and the evidence says how the install went.
- **Browser testing.** The generated app is opened in a real browser, with screenshots that you
  and the Reviewer can see.
- **Any kind of project runs.** The App window follows what the program does, never the language
  or framework. If it opens a web port, you see the page. If it keeps running without a page (a
  worker, a service, an API with no screen), you see the live log, the port and a stop button. If
  it runs and ends (a command line tool, a script, a job), you see the command, the exit code, the
  output and the files it created or changed, each one with **Open** (in Files) and **Copy**. A
  program that ends with code 0 is not a failure. For anything else there is the project
  **Console**: you type a project command and it runs inside the approved `workspace` container,
  one command at a time, after a confirmation, with no interactive input, a time limit, live output
  and a local history. When the plan declares no command to start the product (no `run_command`),
  the App and Run windows open straight in the Console, and the final report says the product is
  used there. The window is never just an error: it shows the final report's "How to use"
  and what was proven (tasks, final check and screenshots). When the environment does not start,
  it shows the end of the log and **Ask the Architect to fix it**, which sends that diagnosis to
  the Architect in the Conversation; any environment change still goes through your approval. An
  app that cannot run in the sandbox (a native desktop window, a native mobile app) is declared in
  the plan, and the window explains it and shows how to run it on your computer. See
  [SECURITY.md](SECURITY.md#project-console) for the Console's protections.
- **Visual quality.** Projects with an interface follow a short visual baseline (use of the desktop
  width, styled form controls, clear type and spacing, finished empty and error states, contrast,
  nothing cut off on mobile): the Architect adds it to the plan as a global criterion, the
  Developer checks its screenshots against it, and the Reviewer raises at most one consolidated
  visual finding per review.
- **Questions to the Architect.** When the Developer asks the Architect something during a task,
  the answer shows in the Conversation. If the Architect cannot answer (for example, the provider
  refused the request), the Conversation shows the reason, and the task record notes it too.
- **The project's language.** The labels the agents and Nants write in messages (criteria, met or
  not met, findings, resolved, screenshots reviewed) follow the project's language, and so do the
  automatic approval line and the list of plan changes. The standard headings of `spec.md` and of
  the task files show in the interface language; the files themselves do not change. Older
  records, in Portuguese or English, are still read.
- **A project that runs without Nants.** The generated `.nants/compose.yaml` mounts the project
  with `${NANTS_WORKTREE:-..}`, so from the project root
  `docker compose -f .nants/compose.yaml run --rm workspace sh -lc '<command>'` works with no
  Nants variable to export. Inside Nants, the full path of the task folder is always mounted. The
  test command does not repeat the install command, which Nants already runs before it: when a
  plan or a recovery proposes one that does, the Architect gets the rule right away and the run
  shows a warning.
- **Dropped connections.** If the connection to the provider drops before the Architect finishes a
  turn, the Conversation says so with a **Try again** button, and a plan left with no tasks shows
  as **Incomplete plan** on its card, with a button that asks the Architect to finish it.
- **Cost per task.** Token usage and an estimated cost in dollars for each spec, task and agent.
  Nants also trims and summarizes the agents' history on its own, so they spend less without
  losing what matters, and the Context area of the Costs window shows the effect, with an
  estimate of the tokens that later calls no longer resend.
- **English and Portuguese.** The whole interface in both languages; the agents follow your choice.
- **Your provider.** Any OpenAI compatible provider, Anthropic (Claude), Google Gemini (AI Studio)
  or your ChatGPT plan, with one model per role.
- **No telemetry.** Your keys and your code stay on your computer.

## Requirements

- Linux, or Windows with WSL2 (x86_64 or aarch64)
- [Docker](https://docs.docker.com/engine/install/) with Compose v2, usable without `sudo`
- Git
- An API key for an OpenAI compatible provider, Anthropic or Google AI Studio, or a ChatGPT Plus
  or Pro plan

## Install

```sh
curl -fsSL https://nants.tmdevlab.com/install.sh | sh
```

The installer verifies the SHA-256 of the package and installs the executable to
`~/.local/bin/nants`, without `sudo`. You can also download the packages from the
[Releases](https://github.com/tm-dev-lab/nants/releases) page. Then:

```sh
nants                     # start Nants and open it in your browser
nants doctor              # check Docker, Compose, Git, the port and disk space
nants --lan               # also accept access from your local network, for this run only
                          # (to keep it on: Settings > Network access)
nants --help              # all commands and options
```

- **Update:** run the install command again. `nants doctor` tells you when a new version is out.
- **Uninstall:** `nants uninstall` removes the executable; `nants uninstall --purge` also removes
  the settings and data. Your projects in `~/NantsProjects` are never deleted.

### Verify a download

Each release is built from this repository by GitHub Actions, with a SHA-256 file and a build
provenance attestation for every package:

```sh
sha256sum -c nants-X.Y.Z-x86_64-linux.tar.gz.sha256
gh attestation verify nants-X.Y.Z-x86_64-linux.tar.gz --repo tm-dev-lab/nants
```

## Quick start

1. Run `nants`. The **Getting started** guide checks your computer and asks for your provider and
   the model for each agent.
2. Create a project: just give it a name. Nants creates a Git repository in `~/NantsProjects`.
3. Describe your idea in the Conversation, answer the Architect and approve the plan.

## Connect Anthropic or Google Gemini

Nants talks to the Anthropic API (Claude) and to the Google Gemini API (AI Studio) natively, with
reasoning, tools, images, web search for the Architect, prompt cache and costs.

1. Get a key: Anthropic at <https://console.anthropic.com/settings/keys>, Google AI Studio at
   <https://aistudio.google.com/apikey>.
2. In **Settings**, click **New connection** and pick **Anthropic (Claude)** or
   **Google Gemini (AI Studio)**. The same choice is in **Getting started**.
3. Give it a name and paste the key, or an `env:NAME` reference (for example
   `env:ANTHROPIC_API_KEY`), so the key is read from an environment variable. No address or
   protocol is needed: **Advanced address** is only for a company proxy.
4. Click **Test** to see the models the key can reach, then **Save connection**. On the first
   connection of a provider, Nants suggests a model for each role: the lowest cost model in the
   price catalog among those with reasoning and images (without prices, the first in the
   provider's list), and you can change any of them.

The effort keeps its three levels: on Anthropic it sets how much Claude reasons, and on Gemini it
becomes the thinking level. To price the cache writes that Anthropic bills separately, fill
**Cache write** under **Settings > Pricing**; the Costs window shows the tokens read from and
written to the cache.

When the Anthropic account reaches its spend limit, or the Gemini daily quota runs out, the run
pauses and the Conversation says what to do: raise the limit in the Anthropic console, or wait for
the quota to renew or switch the connection, then click **Continue**.

**Gemini through the OpenAI compatible layer.** An API key connection to
`https://generativelanguage.googleapis.com/v1beta/openai/` (protocol Chat Completions) also works,
but without web search and without the reasoning on screen. The recommended path is the native
**Google Gemini (AI Studio)** connection.

## Use your ChatGPT plan

With a ChatGPT Plus or Pro subscription, a connection can use the plan on your account instead of
an API key. This feature is in preview.

1. In **Settings**, click **New connection** and pick **ChatGPT plan**. The same
   choice is in **Getting started**.
2. Click **Continue with ChatGPT** and sign in. The sign-in happens in the browser of the computer
   running Nants, because OpenAI returns to `127.0.0.1`. In network mode, that includes the browser
   of this computer that signed in with the terminal link, even on WSL, where it reaches Nants
   through the Windows address. On a phone or another device added by code, the screen asks you to
   do this step on that computer.
3. Back in Settings, the connection is ready, named after your account e-mail, with the models of
   your account. Pick them under **Agents**.

Usage counts against your plan, with no cost in dollars; the Costs window shows it as
**ChatGPT plan**, in tokens. On Plus, the 5-hour usage limit is shared with your other apps that
use the plan. Check it with **Manage usage** (<https://chatgpt.com/settings/usage>). When the limit
is reached, the run pauses until you click **Continue**.

The sign-in is stored in the Nants data folder (`~/.local/share/nants`), in files readable only by
your user. **Sign out**, on the connection row, revokes the access at OpenAI and deletes the
sign-in from this computer. If Nants asks you to sign in again, use **Reconnect**.

## Use it from your phone (local network)

By default, Nants opens only on your computer. To open it from your phone or another device on
the same network, go to **Settings > Network access**. The page guides you in steps and checks
each one before enabling the next:

- **WSL (NAT, the default):**
  1. Turn on **Local network access** and restart Nants (Ctrl+C, then `nants`).
  2. Run the **Script to enable it on Windows** once, in PowerShell as administrator. It forwards
     the ports to WSL and creates the Nants firewall rule, for any network type and only for the
     local subnet. The WSL IP changes when Windows restarts; the step goes back to pending and you
     run the script again.
  3. Add your devices.
- **WSL (mirrored):** the same steps; the script only creates the Windows firewall rule.
- **Linux:** turn it on, restart and add your devices. If you use a firewall, allow the Nants
  port and the preview ports.

**This computer.** In network mode, the terminal prints a sign-in link for this computer and
opens it in the browser. It works once and for 10 minutes. On a real loopback connection, no link
is needed.

**Add a device.** Click **Add device**. Nants shows a 6-digit code that changes every 60 seconds,
the address to open and a QR code of that address. On the other device, open the address or scan
the QR code and type the code. The code only works while that window is open, up to 10 minutes;
after 5 wrong attempts, generate another one.

**Authorized devices.** The list shows each device with when it joined and its last access. You
can rename or revoke it; revoking cuts its open sessions right away. **Revoke all** is there too.
If you revoke this computer, sign in again with the terminal link.

**Back where you were.** A refresh, or a tab opened from another app, a bookmark or the history,
keeps an authorized device signed in. If a page ever lands on the sign-in screen, Nants takes you
back to that page after you sign in.

To turn it off, switch it off on the same page and, on WSL, run the **Script to undo it on
Windows**. The device list is kept. `nants --lan` turns it on for a single run, without saving.

Only devices you add can open Nants. This holds for any address outside loopback, including
`--host` with `--allow-remote`. The preview ports (3001 to 3010) serve the generated app, not
Nants, and do not ask for a device. On the local network, traffic is plain HTTP, not encrypted: for
the most security, use it on a network you trust. See [SECURITY.md](SECURITY.md).

## Troubleshooting

**The page does not open in the Windows browser.** An old Windows port redirect, set up for
network access, takes the Nants port. Nants detects it, starts on the next free port and
explains it in the terminal. To undo it, run the **Script to undo it on Windows** from
**Settings > Network access**, or turn local network access on if you want to keep it.
`nants doctor` shows your environment and whether Windows matches the saved option.

**The sign-in link expired.** The link works once and for 10 minutes. Restart Nants for a new
one, or add this browser as a device with a code from a device that is already authorized.

**It does not open on the other device?** Common causes:

- the device is on another network or on a guest network;
- the router isolates devices from each other;
- step 2 is still pending (on WSL, the IP may have changed after a Windows restart);
- the address was typed wrong; prefer the QR code.

If the sign-in page opens on the device, the network is fine.

## Build from source

You need Rust stable. The web interface is already built in `web/dist` and embedded into the
executable, so Node is only needed to change the interface (Node 24).

```sh
cargo build --release            # executable in target/release/nants
```

Run the tests through `scripts/test.sh`, which gives each run its own temporary folder and removes
it at the end:

```sh
scripts/test.sh cargo test
(cd web && npm ci && ../scripts/test.sh npx vitest run)
```

The optional live tests against Anthropic and Gemini, with your own key and a spend ceiling, are
described in [docs/live-tests.md](docs/live-tests.md).

After changing `web/`, rebuild the interface with `(cd web && npm run build)` and then the
executable. See [ARCHITECTURE.md](ARCHITECTURE.md) for how the code is organized.

## Privacy

- **No telemetry.** Nants sends nothing to us. It talks to the providers you configure, to
  `nants.tmdevlab.com` to check for a new version and, for cost estimates, downloads a public
  price catalog.
- **Your keys stay local.** API keys, including the Anthropic and Google Gemini ones, are stored
  only in your configuration file and are never shown in full, logged or sent to the browser. The ChatGPT plan sign-in stays in the data folder,
  readable only by your user, and is never logged or sent to the browser.
- The server listens only on your computer (`127.0.0.1`) by default.

To report a vulnerability, see [SECURITY.md](SECURITY.md).

## Contributing

Nants is developed through an internal process that we are still consolidating. For now, we
welcome issues (bug reports and suggestions), but we don't accept pull requests. This repository
is a published mirror of the product; changes are made upstream and published here with each
release. See [CONTRIBUTING.md](CONTRIBUTING.md) for how to open a good issue.

## License

Licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE).

---

Nants by [TM Dev Lab](https://www.tmdevlab.com) · [nants.tmdevlab.com](https://nants.tmdevlab.com)
