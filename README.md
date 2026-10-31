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
  <img src="docs/images/desktop-en.webp" width="900" alt="The Nants desktop with the Nants menu open from the N at the top left: search, the grid of apps, the open windows and the shortcuts, over the Conversation with the Architect and the Plan">
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
- **A desktop in your browser.** Every app (Conversation, Plan, Board, Activity, Files, App, Run,
  Report, Costs, Knowledge, Nanty, Project and Settings) opens in a window you can move, snap, resize and
  minimize, and the windows use the whole screen down to the bottom edge. The **N** at the top left
  opens the Nants menu: search, the grid of apps with their badges, the open and minimized windows
  (a click focuses or restores one) and the shortcuts. A pink dot on the N means an app has
  something new. **Alt+0** opens and closes the menu, **Alt+1** to **Alt+9** open the apps in the
  menu's order, **Ctrl+K** opens the launcher and **Esc** closes the menu. On the phone, Nants keeps
  its own bottom bar.
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
- **Memory and process limits on every container.** Every container Nants starts for a project
  (the Developer's commands, tests, checks, the app, the Console and the browser tests) gets a
  memory limit of half your computer's RAM, with no extra swap, and a process limit taken from
  that memory (between 1024 and 4096 processes and threads). A lower value in the project's
  `compose.yaml` still applies; a higher one, or none, becomes the limit, and the project cannot
  turn it off. When a command goes past its time limit, Nants removes its container, with every
  process inside it. A command stopped by one of the limits comes back to the agent saying which
  one, so a runaway test is fixed instead of run again.
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
  with no change, and if it passes the task goes on with a flaky test alert. When the round fails
  with the same failure lines the Developer already saw on the same code, there is no extra round.
- **Offline round.** A property of the environment, such as "the tests do not reach the internet",
  is proven by the environment and not by guards in the code. The Architect marks such a criterion
  with `(offline)` at its start (the marker is the same in both languages). After the regular test
  round passes, the suite runs once more on an internal Docker network with no internet access:
  the project's own services, such as a database, stay reachable, and the dependency cache is the
  same. The result goes into the run record marked as offline and is the evidence of the marked
  criteria; the Reviewer asks for that round, never for code that checks the network.
- **A stopped Developer picks up where it left off.** When the Developer stops before finishing
  (the step limit, the same call repeated with no progress, or no progress at all), Nants resumes
  the work once, in a new session with what was done and what is left, before tests and review.
  The resumption does not count as an attempt.
- **Evidence that exists.** Nants checks every path the Reviewer cites as evidence against the
  reviewed tree. When a cited file or line does not exist, the list goes back to the Reviewer once;
  what is still invalid shows as **not verified** in the review and goes to the final check. The
  final check is held to the same rule, and the Conversation warns about the criteria it left not
  verified. A criterion the Developer declares not met or partly met needs an explicit answer from
  the Reviewer.
- **Developer warnings are checked.** The Developer first tries to check what it is unsure about
  with its own tools; what it still declares as a warning (for example "I did not read the error
  handler to confirm this format") goes to the Reviewer, numbered, as data to verify. The Reviewer
  answers each one: **confirmed**, with a file and line or a test (Nants checks that the path
  exists, like any other evidence); **refuted**, which becomes a HIGH finding and sends the task
  back; or **not checkable**, with the reason. A warning left without an answer gets one more
  request. The report lists the confirmed ones under **Warnings checked by the Reviewer**.
- **Known issues in the report.** The Developer warnings the Reviewer could not check, the
  criteria confirmed as not met or partly met and those the environment cannot prove go into the
  task file, and the ones the final check does not resolve show in the final report under **Known
  issues**.
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
- **A knowledge base per project.** Add your team's standards and guides (markdown, text, PDF
  with text, or a page link) or write documents right in the Knowledge window. Mention a document
  with `@` in the Conversation, see every source the plan cites, and preview it with a hover or a
  tap. The Architect decides what is a rule for the request and you approve it in the plan. See
  [Knowledge base](#knowledge-base).
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
- **A service you can call from the Console.** When the app is a service without a page (an API,
  or a worker with a port), the approved environment installs a tool to make HTTP requests with the
  image's own package manager, or uses an image that already has one. When a task changes the
  project's README, the first word of every command in its "How to run" and "How to test" sections
  is checked inside the environment, and a command the image does not have is a finding in that
  task's review. In the Console, a command that ends with
  code 127 says the command does not exist in the project image (a missing tool or a typo) and
  suggests asking the Architect to add the tool.
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
- **Nanty, the agent above the projects.** Nanty watches your projects and the machine, tells you
  on Telegram (text or voice) what needs you, suggests the next step with buttons and does on its
  own only the safe actions you allow. See [Nanty](#nanty).
- **English and Portuguese.** The whole interface in both languages; the agents follow your choice.
- **Your provider.** Any OpenAI compatible provider, Anthropic (Claude), Google Gemini (AI Studio)
  or your ChatGPT plan, with one connection and one model per role. See
  [Connections per role](#connections-per-role).
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

When the account reaches a limit (credit, billing, the Anthropic spend limit or the Gemini daily
quota), the run pauses with a card that lets you switch the connection of that role and continue.
See [Connections per role](#connections-per-role).

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
is reached, the run pauses until you click **Continue**, or until you switch the connection of the
role that reached it (see [Connections per role](#connections-per-role)).

The sign-in is stored in the Nants data folder (`~/.local/share/nants`), in files readable only by
your user. **Sign out**, on the connection row, revokes the access at OpenAI and deletes the
sign-in from this computer. If Nants asks you to sign in again, use **Reconnect**.

## Connections per role

Each role (Architect, Developer and Reviewer) has its own connection and model, set in
**Settings > Agents**, and they can be on different providers.

- **In the Conversation.** The footer shows the role you are talking to, the Architect, with its
  model and its connection (the name, and the type only when it differs from the name). While a
  run is going, a short line next to it shows the role working right now, with the task and the
  connection (for example "now: Developer · T01 · Gemini · gemini-3.1-flash-lite"); it goes away
  when the run ends. Each line fits on one line on the phone. Click the footer to open
  **Connections in use**: the three roles,
  each with its connection, model and state (available, or paused with the reason), a **Switch**
  button and, when the provider has one, a link to the account limits or billing page. A switch is
  saved in Agents and changes only that role. It applies when the run starts again (on
  **Continue** or a new start); for the Architect, on the next conversation turn, while its part
  in the run in progress changes only when the run starts again.
- **When a provider fails.** The Conversation and the run card say which role failed, in which
  task, through which connection and model, and why, in plain words: the key or sign-in was
  refused, billing, quota, rate limit, the request was refused, the conversation no longer fits
  the context, a failure on the provider side or a dropped connection. **Provider details**,
  folded, shows the HTTP status, the provider's error type, code and parameter, the request id,
  the connection type and the attempt. The provider's own message never reaches the screen.
- **Limits that pause the run.** Waiting a few seconds does not fix these, so Nants pauses instead
  of retrying: credit used up or a pending payment (any HTTP 402 and the billing error codes, at
  any provider), the Anthropic spend limit of the account, organization or workspace, the Gemini
  daily quota and the ChatGPT plan usage limit. The card names the role, the connection and the
  model, links to the provider page when the provider has one and lets you pick another connection and model for that role
  only, with **Switch and continue**, or **Continue without switching** once you fixed it at the
  provider. With no other connection ready, it points you to Settings. Rate limits per minute only
  slow the run down.
- **The failure log.** Every final provider failure is recorded in the project, in
  `.sdd/specs/<spec>/provider-errors.jsonl` (outside a spec, in
  `.sdd/specs/.provider-errors.jsonl`). Each line has the time, the role, the connection, the
  model, the task and attempt, the HTTP status, the provider's error type, code and parameter, the
  request id, the Nants reason and the retries. Nants does not write the request it sent; the
  provider's message may quote parts of it, and is kept only here, cut at 600 characters, with
  your configured keys redacted. The file stays out of the project's repository and stops growing
  at 1 MiB.

## Knowledge base

Each project can have its own knowledge base: your company's architecture standards, API
guidelines, manuals, or any reference the agents should look up. Open the **Knowledge** window
from the Nants menu (the N at the top left) or with Ctrl+K.

- **What goes in.** Markdown, plain text and PDF files with text (up to 32 MiB each, dropped on
  the window or picked with **Add files**), and links to web pages. A link is read once, when you
  add it, and is not updated later. Scanned PDFs and pages that need JavaScript show up as **No
  text**: the agents cannot read them.
- **No labels to set.** You do not mark documents as standards or references. The Architect
  reads the base, decides what is a rule for the request and copies it into the plan; you approve
  the plan. A one line **Description** per document helps the agents choose; they also see each
  document's headings and an outline of its sections.
- **The Overview is your voice.** The **Overview** tab is a short text about the whole base,
  written only by you (never by a model). This is where you say "these documents are our company
  standards" or where to look for each subject. The Architect reads it as your intent, not as
  document data.
- **Mention with `@`.** Type `@` at the start of a word in the Conversation to pick a document,
  and press Tab or the right arrow to pick one of its sections. The mention shows as a chip with
  the title and goes in the message as `kb:<id>` or `kb:<id>#<section>`. The Architect reads what
  you mention and uses it in the plan or says why not; the plan review flags a mentioned document
  the plan does not cite. Without a mention, everything works as before.
- **Sources in the plan.** The Architect cites the base with citations such as
  `kb:api-standards-3f9a1c2e#versioning`. What is a rule goes into the criteria, the Assumptions or
  the Guidance, with the citation, and binds the Developer and the Reviewer; a citation in the
  descriptive sections (technical approach, context, decisions) is only a reference. The Plan
  shows a **Knowledge sources** box with every cited document and section, each marked **Rule**
  or **Reference**, and a removed source struck through. Every citation is a link: hover it (or
  tap it on the phone) to preview the cited excerpt, and open it in the base at that section.
  Nants refuses a citation that does not exist and, after you approve, warns if the cited source
  changed.
- **Find your way around.** Press `/` in the Knowledge window to search: titles filter as you
  type, and the text of every ready document is searched too, ignoring accents and case, with the
  matches highlighted. Each document shows an outline to jump between its sections, and the
  lists **Used in plans** (the specs that cite it), **Cited by** (the documents that link to it,
  with the line around the link) and **Links to** (what it links to, in the base or outside).
- **Write in Nants.** **New document** opens an editor with a live preview beside the text (tabs
  Write and View on the phone), a formatting bar, the usual shortcuts (Ctrl+B, Ctrl+I, Ctrl+K…),
  list continuation and `[[` to link another document of the base. It saves on its own about a
  second after you stop typing, tidies trailing spaces and blank lines, keeps unsaved text on the
  device, and when another window saved first it offers **Reload** or **Keep mine**. **Edit**
  works the same on any markdown or text document up to 2 MiB; larger files, PDFs and links stay
  read only.
- **Data, never instructions.** The content and the titles of the documents are always treated as
  untrusted data in the prompts and go through the redaction of your configured secrets. Only
  what you approve in the plan binds the agents.
- **Out of Git.** Everything lives in the project's `.sdd/kb/` folder, kept out of the
  repository through `.git/info/exclude`. Typical secret files (`.env`, private keys) are refused.
- **Hints.** The window flags duplicates (the same content added twice, for example as a file and
  as a link), documents without headings (only the whole document can be cited) and internal links
  to files that are not in the base.

See [SECURITY.md](SECURITY.md#knowledge-base) for the redaction and its limits.

## Nanty

Nanty is the Nants agent that sits above the projects. It watches your projects and the machine,
tells you on Telegram what needs you, suggests the next step and does on its own only the safe
actions you allow. Open it from the Nants menu (the N at the top left), in any space, even with no
project open. The observers and the feed in the window are on from the start and cost nothing;
Telegram, the model and the voice need you to set them up in the **Settings** tab of the Nanty
window.

- **What it watches.** A plan waiting for your approval, a run blocked or paused (credit, quota or
  budget), a review going in circles, unusual cost, low disk space, low space in the temporary
  folder, leftover Docker resources of Nants and the milestones (a task delivered, a spec done,
  the app ready). The watching is done by code, with no model and no cost: a check with nothing
  new calls nothing.
- **Notices that do not flood.** Notices are grouped by project and type, wait a little before the
  first one and do not repeat for a while. A notice that follows from another (a blocked run when
  the credit ran out) stays quiet behind it. Every notice says why, in one line written by Nants
  (the observer, the facts and the time), never by the model, and a proactive card only carries
  counts and codes, never the text of the project.
- **The window.** The feed has **Waiting for you**, **Done on its own** (with **Undo** where it
  makes sense), **Planned** (including the morning summary) and **Today**, with the conversation on
  the right. Its animated ant shows what Nanty is doing: watching, thinking, talking, sleeping or
  waiting for you, and stays still when your system asks for reduced motion. The badge of the
  Nanty app counts only what the bell does not already show.

### Create the bot and pair it

Each Nants installation uses its own Telegram bot. Creating one is free.

1. On Telegram, open a chat with `@BotFather`, send `/newbot` and choose the name and the username
   of the bot.
2. Copy the token it sends and paste it in **Nanty > Settings > Telegram**. The token is kept like
   the API keys (an `env:NAME` reference works too) and never comes back whole to the screen.
3. Click **Create the pairing code** and tap **Open the bot on Telegram**: the link
   (`t.me/<bot>?start=<code>`) opens your private chat with the bot, and **Start** sends the code.
   The code works only once, only shows on that screen and changes every 2 minutes; 5 wrong
   attempts lock it. Only a private chat pairs: in a group, the bot ignores the message and leaves.
   From then on, Nanty talks only to you and ignores anyone else.
4. **Send a test message** to check it. **Unpair**, or a new token, forgets the chat.

**The bot photo.** Telegram does not accept an animated bot photo. In the same tab, click
**Download the image**, open `@BotFather`, send `/setuserpic`, pick your bot and send the image.

**One bot per installation.** Every person who installs Nants creates their own bot and pairs it
with their own Nants, so installations never share a bot. Telegram accepts only one reader per
bot: the same token in two installations at the same time shows **bot in use by another program**;
create another bot in BotFather for the other one. Two Nants processes on the same data folder
never compete for it: only one runs Nanty, and the other says that Nanty is active in another
instance.

### Rules

The **Rules** tab lists the fixed catalog of actions. Each action has one level: **off**, **ask
first** or **automatic**. Nanty never does anything outside this catalog.

- **Automatic from the first day:** try again after a passing provider failure (up to 2 times, with
  a wait between them); switch a role to the reserve connection you chose and continue, when its
  credit runs out or it reaches its spend limit; and clean the leftover containers and networks
  with the `nants.project` label of stopped projects of this installation. A plan or quota limit
  only gets a notice.
- **Ask first:** approve a plan, resume a paused run and pause a run (only when you ask). The
  approval card checks that the plan is still the one it showed you.
- **Only you:** allowing a command "always" and changing where Nanty talks to you (the channel,
  the token or the chat). This band is locked: no Nanty action does it.
- **Free text always asks.** Anything you ask in your own words comes back as a card to confirm,
  even when the action is automatic.
- **Cards.** Every card has buttons (yes, no, remind me later in 1 hour) and an expiry. The state
  is checked again at the click; the first click counts, in the window or on Telegram, and the
  other one gets "already decided". After a no, the offer only comes back with a new fact. A switch
  to the reserve can be undone for 24 hours, as long as nobody changed that role since. Every
  decision goes to the record, with who decided: automatic, Telegram or the window.

### Conversation and commands

The conversation is the same in the window and on Telegram, with where each message came from.

- **Commands**, answered with no model, in both languages: `/status` (the notices, what was done
  today and the model spend), `/summary` (the summary now), `/pause` (pauses the run in progress,
  after you confirm the card), `/quiet` (mutes the notices for 1 hour, or `/quiet 3` for 3 hours,
  up to 24) and `/help`. The Portuguese ones (`/resumo`, `/pausar`, `/silencio`, `/ajuda`) work
  too.
- **Free text** goes to the model of Nanty. To have it look at a project, mention its name: it
  reads only that project (the plan, the tasks, the report and the costs, never the logs), as
  untrusted data and with your secrets redacted. The reply to a message from the window stays in
  the window.
- **Quiet hours and the morning summary.** By default, quiet from 22:00 to 07:00 and the summary at
  08:00, in the time of the computer running Nants. In quiet hours only the urgent gets through,
  and silently; the rest goes into the summary. While a Nants tab is visible, a notice waits a few
  minutes before going to Telegram, and what you decide in the window meanwhile does not go; you
  can turn that off in Settings.

### Voice

Voice is off until you turn it on in Settings. With it on, Nanty answers a voice note you send on
Telegram with a voice note, always with the text in the caption (and the whole text in a message
of its own when the caption cut it). Text gets text, and the notices and the summary stay in text.

- **Order of the connections:** by default, your Google Gemini connections, then the OpenAI ones,
  then text only. You can set your own order.
- **Your voice notes** are transcribed only by a Google Gemini connection, up to 5 minutes, and
  the transcription shows in the conversation.
- When the voice fails or the budget is used up, the text goes anyway: the notice is never lost.

### Cost and budget

The model only runs when an observer fires or when you ask. Nanty has a daily budget for the model
and the voice, US$ 0.50 by default. A model without a price (or a ChatGPT plan connection) counts
tokens instead, 200,000 a day by default, and Settings says so; with the same plan connection as
the roles, the plan quota is the same one. When the budget runs out, the notices go on with the
fixed text of the observer, without the model summary and without voice, and the commands still
work. The **Costs** window shows the Nanty spend apart from the projects.

### Nants closed, Nanty asleep

Nanty only works while Nants is running. With Nants closed it sleeps: there are no notices, and
Telegram keeps your messages for up to 24 hours. When Nants opens again, those messages go into the
conversation as cards marked as old, and a button tapped in the meantime becomes an old card too:
an old click never decides anything, and nothing is done from a message that waited. Decide again
on the current notice.

See [SECURITY.md](SECURITY.md#nanty-and-telegram) for what leaves your computer and how Nanty is
protected against prompt injection.

## Docker resources

Everything Nants creates in Docker for a project carries the `nants.project` label, with the
project id as its value, and is removed at the end. Nanty cleans the leftovers of stopped projects
of this installation (a rule in the Nanty **Rules** tab). Resources of a project this installation
does not know are never touched: they may belong to another Nants installation (another data
folder on this computer, or another user). To see them and, if you want, remove them yourself:

```sh
docker ps -a --filter label=nants.project       # containers
docker network ls --filter label=nants.project  # networks
docker rm <id>                                  # removes a container
docker network rm <id>                          # removes a network
```

Check before you remove: these resources may belong to another Nants installation. Compare the
label value with the ids of your projects (in `~/.config/nants/config.toml`), because a resource of
another installation may still be in use.

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

The test suite never calls a real provider: the agents and the providers are simulated, so no key
is needed and nothing is spent.

After changing `web/`, rebuild the interface with `(cd web && npm run build)` and then the
executable. See [ARCHITECTURE.md](ARCHITECTURE.md) for how the code is organized.

## Privacy

- **No telemetry.** Nants sends nothing to us. It talks to the providers you configure, to
  `nants.tmdevlab.com` to check for a new version and, for cost estimates, downloads a public
  price catalog.
- **Your keys stay local.** API keys, including the Anthropic and Google Gemini ones, are stored
  only in your configuration file and are never shown in full, logged or sent to the browser. The ChatGPT plan sign-in stays in the data folder,
  readable only by your user, and is never logged or sent to the browser.
- **Provider failures stay in the project.** Each failure is recorded in
  `.sdd/specs/<spec>/provider-errors.jsonl`, out of the project's repository. Nants does not
  write the request it sent there; the provider's message may quote parts of it, with your
  configured keys redacted (see [SECURITY.md](SECURITY.md#provider-failure-log)).
- **Nanty sends only summaries.** On Telegram, Nanty sends short summaries in plain text, with no
  code, logs or secrets, and listens by long polling, with no open port. Your messages go through
  the Telegram servers.
- **The knowledge base stays in the project.** It lives in `.sdd/kb/`, out of the project's
  repository; only the excerpts the agents read go to your provider.
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
