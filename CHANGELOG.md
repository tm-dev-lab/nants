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
  use code. Its spend has a daily budget and shows apart in Costs: US$ 1.00 by default, or 600,000
  tokens for a model with no price or a ChatGPT plan connection, sized from measured use for about
  70 messages with voice a day plus the automatic analyses, also after Gemini prices double on
  2027-01-01. The voice always counts, and a value you set yourself stays. Nanty sleeps while Nants is
  closed, and what waited comes back as old cards that never decide anything.
- **Nanty remembers where each message came from.** A hostile text read in one turn cannot act in
  the next one: replies store their source across restarts, tainted text leaves the context after a
  few messages, each mode has its own tools, checked when they run, and what looks like a secret is
  redacted, not only the secrets Nants knows.
- **Nanty remembers the conversation for longer.** It now keeps the last 40 to 60 messages with no
  time limit, each with its date and time, and turns the oldest 20 into a short summary at a time,
  written only from clean text: what a project file or a forwarded text said never goes into it.
  The summary never makes you wait for a reply and counts in the daily budget; with no model or no
  budget, a line written by the server takes its place.
- **Nanty is a partner for ideas.** Talk about where a project should go, an idea, the architecture
  or the roadmap, and Nanty says what it would do and why, points out the risk and disagrees when it
  has a reason, without opening with praise, keeping what it saw apart from what it supposes. When
  a question needs real thought, the model itself asks to think again with more effort, once per
  message and on the same model, inside the daily budget and the ceiling per message; with no room
  for it, the reply comes at the normal effort, with no error.
- **Nanty takes an idea to the Architect as a draft.** When an idea is ready, Nanty offers to take
  it to the Architect of that project, on a card with the whole text. Your tap only leaves the text
  as a draft in the Architect conversation of the project, where you edit it, send it or discard it;
  nothing reaches the Architect without you. If the conversation read untrusted content, the draft
  comes marked and, while it is pending, your messages to that Architect run without internet;
  discarding it goes back to normal. A conversation that read
  another project cannot send anything to this one.
- **Nanty's long replies arrive whole.** A long reply starts with what matters and is kept whole in
  the window. On Telegram it comes in up to three messages, split between paragraphs or sentences,
  with the buttons and the server sentence in the last one, and the rest pointed to the window when
  it is longer. With voice on, the audio speaks only the start of the first paragraph, the caption
  shows the same part, and the rest follows as text.
- **Nanty has a long term memory.** It keeps short notes by topic (how you work together, the way
  you want it to be, how to run the system, runbooks and decisions) that survive restarts and
  updates. It saves on its own only in a clean conversation, and the reply says what it saved, with
  a button to delete it. After reading project content or a forwarded text, nothing is saved
  without you: a silent card shows the exact text and where it came from. A card too long for one
  Telegram message is confirmed only in the window, where the whole text shows. Secrets are redacted
  before saving, and a note is never an action, a permission, a channel or a token.
- **A Memory tab in the Nanty window.** Read the notes by topic, with who said each one, where and
  when; fix one (if it changed while you edited, you see the saved version and choose), delete it and
  restore it within 7 days. Restoring the older version of a replaced note swaps the two. Notes that left the index for lack of use are marked, a runbook shows
  the exact command with a copy button, and "Download a copy" saves the active notes as a JSON file,
  redacted again.
- **Nanty reads what the agents did when you ask.** Ask why a run got stuck, what went wrong or for
  a report, and Nanty reads the end of the agents' dialog and of their tool trail, the test output
  and the recovery analyses of a task, the spec and the tasks, the last commits and any file of the
  project, such as the architecture or the roadmap. It reads only the projects it watches, through a
  reader that never follows a link or opens secrets, private keys, `.git` or dependency folders, and
  Git runs with the repository settings that could run programs turned off. All of it arrives as
  untrusted data, redacted and with a ceiling per message, taints the turn, and a second project in
  the same message only when both are asked together.
- **Automatic analysis of important notices.** Turn it on in Nanty > Settings and, after a blocked
  run, a review loop or the end of a spec, Nanty reads that project on its own and sends a separate
  message with what happened, the likely cause, always as a hypothesis, and a suggestion. It only
  reads that project, never acts and has no buttons; the notice never waits for it. The card says
  where the text goes and warns about the free tier of the Gemini API. It is off by default, stays
  out of quiet hours, of the summary and of a burst, never repeats a case and has a daily ceiling, a
  ceiling per spec and a share of the daily budget. A text that copies what it read, has a path
  or a ready to paste command is not sent: a short line says so, and you can ask in the
  conversation.
- **Nanty keeps more of the conversation on disk.** The conversation history grows to about 3 MB
  with a fixed ceiling, and old messages leave only after they are in a summary kept in its own
  file. Voice audio has its own limits (32 MB and 14 days by default, in `[nanty.storage]`): the
  audio of an old message goes when it passes the age or the size limit, or with its message, so
  the text can stay after its audio is gone.
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
- **Nanty offers only what it can do, and brings you to Nants.** Notices and replies offer only what
  exists at that moment. When you say "open it", Telegram gets a button to Nants, and the window
  opens the project screen. The address is the last one an authorized device used, or the new
  optional **Nants address for your phone** in Settings; with no address, Nanty tells you to open
  Nants on the computer. If something can't be done, it says why and what you can do instead.
- **Mute in a sentence.** Ask for quiet in your own words, for some hours or until an exact time, up
  to 24 hours. The server confirms with the end time and a button to turn it off. The urgent still
  arrives, without sound, and the rest goes into the summary.
- **Voice, whenever it is on.** With the voice on, everything Nanty says goes as audio with the text:
  replies, notices with their buttons and the morning summary on Telegram, and replies in the
  window, with a player that never starts on its own. Voice goes both ways only through Google
  Gemini, then text, with storage off at the provider. An older configuration with OpenAI in the
  voice keeps working: the first Gemini step becomes the voice connection.
- **Nanty's voice has a tone.** The voice speaks neutral, upbeat, serious, concerned or calm: Nants
  picks the tone of a notice by its kind, and in the conversation the model picks one from the same
  list. After reading something untrusted, the voice stays neutral.
- **A spoken notice says what Nanty wrote.** With a model and budget, the voice note of a notice
  speaks Nanty's own words about it, in a single message, with the reason line still written by
  Nants. If the text takes more than a few seconds, the note speaks the fixed sentence and the text
  follows, so the notice is never late.
- **Git repositories made inside a task are committed as files.** When an agent runs `git init`
  inside the project, the history of that inner repository is kept out of the tree, its files go
  into the commit as regular files, and the conversation says so. A setting placed in that
  repository never runs a program on your computer.
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
