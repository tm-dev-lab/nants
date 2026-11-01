# Changelog

All notable changes to Nants for the people who use it are listed here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Nanty, the agent above the projects.** Nanty watches your projects and the machine (a plan
  waiting for you, a paused or blocked run, unusual cost, low disk, leftover Docker resources and
  the milestones) with no model and no cost, and tells you in its own window and on Telegram, with
  the reason of every notice. It suggests the next step with buttons and does on its own only the
  safe actions you allow: each action of a fixed catalog is off, ask first or automatic, the
  actions that are only yours stay locked, and any action you ask for in your own words comes back
  as a card to confirm. From the first day it tries again after a passing provider failure, switches a role
  to the reserve you chose when its credit runs out, and cleans leftover containers and networks
  of stopped projects. On Telegram, you create your own bot in BotFather and pair it with a single
  use code. Its spend has a daily budget and shows apart in Costs. Nanty sleeps while Nants is
  closed, and what waited comes back as old cards that never decide anything.
- **Talk to Nanty in plain words.** You talk to it as you would to someone on your team, by text or
  voice, with no phrase to learn: it looks up what it needs with read only tools, remembers the last
  messages and works out the project from a number, part of the name or its state ("the one that
  got stuck"), and it asks when in doubt. On Telegram, you see "typing" while it works. Commands
  became optional shortcuts. Replies, notices and the morning summary are written by the model when
  there is one, in natural sentences, with the reason of each notice still written by Nants. In the
  conversation, Nanty's words come first and a sentence from the server closes the message; in a
  summary with model text, the title and the reasons open it. With no model or no budget, the fixed replies were rewritten
  to sound natural too.
- **Language of the person, and Nanty's own.** Nanty answers in the language you wrote or spoke in,
  whatever the language of the interface. Notices and the morning summary follow the new **Nanty
  language** option in Settings: follow the conversation (the default), Portuguese or English.
- **Mute in a sentence.** Ask for quiet in your own words, for some hours or until an exact time, up
  to 24 hours. The server confirms with the end time and a button to turn it off. The urgent still
  arrives, without sound, and the rest goes into the summary.
- **Voice, whenever it is on.** With the voice on, everything Nanty says goes as audio with the text:
  replies, notices with their buttons and the morning summary on Telegram, and replies in the
  window, with a player that never starts on its own. Voice goes both ways only through Google
  Gemini, then text, with storage off at the provider. An older configuration with OpenAI in the
  voice keeps working: the first Gemini step becomes the voice connection.
- **Clearer Nanty settings.** The Voice card has one **Gemini connection for voice** selector and
  shows whether voice is ready, has no key, had its key refused or has no connection. The model
  fields have the same autocomplete as the rest of Nants, the speech field lists only speech models
  and the voice list shows the voices in Portuguese and English.
- **Defense against steered conversations.** A forwarded message or a quote of someone else is
  treated as untrusted data, and after any untrusted content in the turn, even a mute needs your
  click.
- **No more cycles with no progress.** When the same failure repeats in two cycles in a row, the task
  goes to recovery right away instead of spending the 5 cycles, and the step of the test command
  that failed goes to the Developer and to recovery.
- **A test battery that can run from the start.** The plan review points out a step of the test
  command that only becomes runnable after the first task that runs it, and a step that already
  ended with exit code 126 or 127. It is a finding: it never blocks your approval.
- **Docker resources, explained.** **Settings > Machine** and the docs show how to see and remove
  the resources with the `nants.project` label yourself, and why to check first: they may belong
  to another Nants installation.
- **The Nants menu.** The N at the top left opens a menu with search, the grid of apps with their
  badges, the open and minimized windows and the shortcuts. A pink dot on the N means an app has
  something new. **Alt+0** opens and closes it; **Alt+1** to **Alt+9** and **Ctrl+K** work as
  before. Nanty has a line icon there, like every other app, and its colored ant stays in its own
  window and on Telegram.
- **Who is working now.** While a run is going, the Conversation footer shows a short line with the
  role working right now, its task and its connection, next to the Architect you talk to.
- **Offline round.** A criterion that starts with `(offline)` is proven by running the test suite
  once more without internet access, after the regular round passes. The project's own services,
  such as a database, stay reachable.
- **A service you can call from the Console.** When the app is a service without a page, the
  approved environment brings a tool to make HTTP requests. When a task changes the project's
  README, the first word of every command in its "How to run" and "How to test" sections is checked
  inside the environment, and a command the image does not have is a finding in that task's
  review. A command that ends with code 127
  in the Console says it does not exist in the project image.

### Changed

- **No more dock.** Its place is the Nants menu, and the windows use the whole screen down to the
  bottom edge. Saved layouts are adjusted once to the new space.
- **The top bar stays on one line** at any desktop width; only the status text shortens, with the
  full text on hover.
- **Reviews that end.** A finding the Architect already ruled on that comes back with no progress
  goes to the Architect for a final decision instead of back to the Developer.
- **Developer warnings** are kept for what the Developer could not check and what the result
  depends on, and rules copied from the knowledge base say where they apply (for example, only the
  production code).
- **Text in the project's language.** Messages Nants records, such as a stopped run, and the
  specialist profiles follow the project's language.

### Fixed

- **A blank screen when opening a project** whose recovery offered to revise a task. The
  screen now reads that option in both formats, and an error inside one window shows in that window
  only, with **Reload the window**, while the rest of Nants keeps working.
- **Tests that start the `nants` executable** now use their own temporary home, data and
  configuration folders, so a test run never touches your Nants data.
- **The App window in Console mode** keeps its title bar controls on the right, and no text or log
  line spills out of its columns or of the window.
- **Evidence checks** no longer read an image reference (such as `mcr.microsoft.com/openjdk/jdk:21`),
  a host or a package name as a file path, so valid evidence is not marked as not verified.

## [0.1.0] - 2026-10-22

First public release.

### Added

- **You describe the idea, the agents handle the engineering.** You talk to the Architect to turn the
  idea into a specification with tasks and acceptance criteria, and you approve the plan before any
  code is written.
- **Developer and Reviewer work for you.** Each task is built by the Developer in an isolated copy of
  the project, then tested and checked by the Reviewer; only approved work reaches your project.
- **Everything runs on your computer.** Projects are local Git repositories in `~/NantsProjects`, and
  Nants is a single executable with its interface in the browser.
- **Docker projects.** Tests, checks and the running app live in containers, with nothing installed
  on your system and no `sudo`.
- **Browser testing.** The generated app is opened in a real browser (Playwright), with screenshots
  that you and the Reviewer can see.
- **Follow along and decide.** Your browser shows the conversation, the plan, the tasks,
  the board, the files, the running app and the cost of each step, and you can stop, approve or ask
  for changes at any time.
- **Use the provider you prefer.** Connections to OpenAI compatible providers, one model per role and
  a cost estimate in dollars.
- **The `nants` command.** Opens Nants in the browser on a free port; `nants doctor` checks Docker,
  Compose, Git, the port and disk space, shows the command to fix anything missing and tells you when
  a new version is out.
- **One command install** on Linux and on Windows through WSL:
  `curl -fsSL https://nants.tmdevlab.com/install.sh | sh`.
- **English and Portuguese.** The whole interface in both languages, picked from your browser and
  switchable at any time in Settings. The agents talk, plan and write the report in the chosen
  language; the generated app follows the language of your request.
- **Uninstall.** `nants uninstall` removes the executable; with `--purge`, also the configuration and
  data, always asking first and never touching your projects.
- **Privacy.** No telemetry; API keys stay on your computer.
- **Open source.** Nants is licensed under the Apache License 2.0.

[0.1.0]: https://nants.tmdevlab.com
