# Changelog

All notable changes to Nants for the people who use it are listed here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **The Nants menu.** The N at the top left opens a menu with search, the grid of apps with their
  badges, the open and minimized windows and the shortcuts. A pink dot on the N means an app has
  something new. **Alt+0** opens and closes it; **Alt+1** to **Alt+9** and **Ctrl+K** work as
  before.
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
