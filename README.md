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
5. **Run the app.** The Architect writes a final report and Nants runs the result so you can try it.

<p align="center">
  <img src="docs/images/conversation-en.webp" width="640" alt="The Architect asking questions with suggested answers">
</p>

## Features

- **Agents with roles.** Architect, Developer and Reviewer work from a specification with
  acceptance criteria, and every task runs in an isolated Git worktree.
- **Docker for every project.** Tests, checks and the running app live in containers, with
  nothing installed on your system and no `sudo`.
- **Browser testing.** The generated app is opened in a real browser, with screenshots that you
  and the Reviewer can see.
- **Cost per task.** Token usage and an estimated cost in dollars for each spec, task and agent.
- **English and Portuguese.** The whole interface in both languages; the agents follow your choice.
- **Your provider.** Any OpenAI compatible provider, with one model per role.
- **No telemetry.** Your keys and your code stay on your computer.

## Requirements

- Linux, or Windows with WSL2 (x86_64 or aarch64)
- [Docker](https://docs.docker.com/engine/install/) with Compose v2, usable without `sudo`
- Git
- An API key for an OpenAI compatible provider

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

## Use it from your phone (local network)

By default, Nants opens only on your computer. To open it from your phone or another device on
the same network, turn on **Local network access** in **Settings > Network access**. The page
shows what your environment still needs:

- **Linux:** nothing else. If you use a firewall, allow the Nants port and the preview ports.
- **WSL (NAT, the default):** run the **Script to enable it on Windows** once, in PowerShell as
  administrator. It forwards the ports to WSL and creates the Nants firewall rule. The WSL IP
  changes when Windows restarts; if so, run it again.
- **WSL (mirrored):** the same script, which here only creates the Windows firewall rule.

The option is saved and applies from the next start: stop Nants with Ctrl+C and run `nants`
again. To turn it off, switch it off on the same page and, on WSL, run the **Script to undo it
on Windows**. `nants --lan` turns it on for a single run, without saving.

Use it only on a network you trust: Nants has no login, so any device on the network can
control it.

## Troubleshooting

**The page does not open in the Windows browser.** An old Windows port redirect, set up for
network access, takes the Nants port. Nants detects it, starts on the next free port and
explains it in the terminal. To undo it, run the **Script to undo it on Windows** from
**Settings > Network access**, or turn local network access on if you want to keep it.
`nants doctor` shows your environment and whether Windows matches the saved option.

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

After changing `web/`, rebuild the interface with `(cd web && npm run build)` and then the
executable. See [ARCHITECTURE.md](ARCHITECTURE.md) for how the code is organized.

## Privacy

- **No telemetry.** Nants sends nothing to us. It talks to the providers you configure, to
  `nants.tmdevlab.com` to check for a new version and, for cost estimates, downloads a public
  price catalog.
- **Your keys stay local.** API keys are stored only in your configuration file and are never
  shown in full, logged or sent to the browser.
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
