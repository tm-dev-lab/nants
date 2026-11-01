# Nants architecture

*Software built by agents. Steered by you.*

You describe the idea. The agents handle the engineering.

## Overview

Nants is a single executable (`nants`) with a local server (Rust, axum) and the web interface
built in (React). The server keeps the global configuration, orchestrates the Architect, the
Developer and the Reviewer over the `.sdd/` folder of each project, and publishes everything over
REST and SSE. Usage is described in `README.md`.

The interface is a desktop: a top bar and a work area with free floating windows that reaches the
bottom edge (no space is reserved below the windows). The N button, rendered by `Desktop` in the
top bar `header`, opens the Nants menu (`NMenu`): search, the grid of apps (the same `kinds` the
`Desktop` shows), the open and minimized windows and the shortcuts (Alt+0 opens and closes it,
Alt+1 to Alt+9 open the apps in its order, Ctrl+K stays the launcher).

- **Spaces**: one per project. The URL is `/?space=<id>`; any other path is normalized with
  `replaceState`. Only the windows of the active space are mounted.
- **Windows**: Conversation, Plan, Board, Activity, Files, App, Costs, Report, Knowledge, Nanty,
  Settings, Project and Getting started, one per type, each with a live status subtitle and an
  empty state. Nanty is a system window: it opens in any space, even with no project.
- **Layouts per phase**: planning, execution and done, with the current arrangement and "my
  layout" per category; switching layouts can be undone. A saved arrangement carries a version
  (`LAYOUT_VERSION`); one saved without it, for the older and shorter work area, goes once through
  `migrateLayout` (`presets.ts`), which extends the windows that reached the old bottom edge.
- **Saved state**: `/api/ui-state` (global) and `/api/projects/:id/ui-state`, with `rev`, PUT per
  section and 409 on conflict; `BroadcastChannel` between tabs; the phone only writes the `mobile`
  section.
- **Needs you**: `/api/attention` feeds the bell, the floating notices, the app badges (in
  the Nants menu, with a dot on the N, and on the phone), the tab title and the favicon.
- **Getting started**: `/api/readiness` defines what is missing (machine, connection, agents); the
  server blocks actions with 412 `prereq_missing` and the interface explains why and leads to the
  fix.
- **Projects folder**: New project asks only for the name and the server creates
  `~/NantsProjects/<slug>` (configurable root) with Git.
- **Embedded app**: the App window runs the app in Docker and embeds it in a sandboxed iframe; the
  CSP carries `frame-src` only for the page host.
- **Phone** (under 1024 px): one window at a time, a compact bar, tabs per phase and the Switcher.
- **Appearance**: Dark, Light and Automatic themes; Dots, Nebula, Aurora and Plain wallpapers.

## Nanty

Nanty is the system agent above the projects (`src/server/nanty/`, configuration in the `[nanty]`
section, `src/config/nanty.rs`). Its own section keeps it out of the agents block, which
`PUT /api/config/agents` replaces whole; only what differs from the defaults is written.

```
event bus / periodic scan
  -> observe.rs   deterministic observers, no model (attention, runner stop codes, cost alert,
                  free disk and temporary space, Docker resources with nants.project, milestones)
  -> filter.rs    group by project and type, wait, repeat interval, inhibition, silences, quiet
                  hours; fixed urgency table; the why line of each notice comes from the dictionary
  -> act.rs       a card per offer (token, expiry, plan hash), the level of each catalog id
                  (actions.rs), automatic actions, atomic decision, state check, undo
  -> turn.rs      proactive model turn in a single queue, inside the daily budget: structured
                  signals only (signals.rs)
  -> telegram/    long polling, pairing, outbox (one message per second, edits, quiet sends, speech
                  outside the queue), format.rs (plain text, redaction, limits); voice/ for speech
                  and transcription

person message (Telegram or window)
  -> chat.rs      command, or free text; forwarded text and third party quotes are marked
  -> agent.rs     loop with the same turn queue and budget: the model picks tools until the text
  -> tools.rs     read only tools, ids checked against the watched projects; proposals and mute
  -> silence.rs   mute applied at once, or a card when the turn is tainted
  -> voice/       the text enters the conversation, then the speech joins the same message
```

- **Modules.** `mod.rs` (the loop, the lock and the bus subscriber), `observe.rs`, `filter.rs`,
  `actions.rs` (the fixed catalog) and `act.rs` (offers and execution through the existing
  handlers, called in process, never over HTTP), `signals.rs`, `turn.rs`, `digest.rs` (the morning
  summary), `chat.rs` (commands, free text and the reserve with no model), `agent.rs` (the
  conversation loop), `tools.rs` (the conversation tools), `silence.rs` (the mute of everything),
  `language.rs` (the language of the person and of Nanty), `read.rs` (the reading on request, from
  the project snapshot, as untrusted data), `render.rs` (`code` plus `params` to text), `store.rs`
  (cards, silences, the conversation, the spend of the day), `journal.rs` and `jsonl.rs` (the record,
  JSONL with rotation), `clock.rs` (local time through `localtime_r`, injectable in tests),
  `instance.rs`, `settings.rs` and `api.rs` (the `/api/nanty/*` routes and the window events),
  `telegram/` (`client.rs`, `pairing.rs`, `outbox.rs`, `format.rs`, `api.rs`) and `voice/`
  (`provider.rs` for the Google Gemini calls: speech, transcription with `store: false`, the voice
  list and the speech models; `encode.rs` for the Opus voice note in pure Rust).
- **One source of truth.** `observe::project_facts(state, cfg, id)` gives the facts of a project with
  one phase of explicit precedence (paused, stopped by the provider, plan waiting, running, stopped
  with nothing pending), read from the same attention context as the bell. The observer and the
  `project_status` tool both use it, so the conversation and the notices cannot disagree. Zeros and
  default values are not facts.
- **Conversation loop.** `agent.rs` reuses the call id functions of the agents, the `ChatBackend` and
  the `provider_items`, resent as they came. Limits come from `[nanty.chat]` (rounds, deadline, calls
  per reply, tokens per message, memory, `project_details` per turn), written only when they change;
  the budget is checked before each round, every call gets a result and the last round has no tools.
  The system prompt and the tools form a stable prefix with a fixed `cache_key`; the project list,
  the notices and the memory come after it, in the person's message. The final text is plain text
  and goes through `format::clean`; the JSON answer is only for notices and summaries.
- **Tools.** `project_status`, `system_status` and `nanty_activity` are trusted; `project_details`
  is not: it goes in the untrusted data envelope and taints the turn (`Session::tainted`), as does a
  forwarded message or a third party quote. `propose_action` accepts only what `act::requestable`
  offers. `set_silence` takes `hours` or `until` (`HH:MM`, its next occurrence, at most 24 hours away)
  and applies at once in a clean turn; in a tainted turn it becomes a card that keeps the same
  request and expires at the latest at the time it shows. The urgent still arrives during a mute, without sound:
  `filter.rs` treats it like quiet hours.
- **Text of a message.** With model text, `render::message_text` puts Nanty's words first and the
  server sentence last (`render::closing`), so the cut of the caption and of the speech keeps it.
  Only the summary (`digest::LEAD`) opens with the title and the kept whys. The why of each notice always comes
  from the dictionary.
- **Voice.** Gemini only, then text; the OpenAI steps of an old `order` are read, dropped with a line
  in the record and never written. `voice::deliver` stores the text first, then generates the speech
  outside the send queue, one at a time and with a deadline, and sends the message once.
  `GET /api/nanty/voices` serves the Settings: `voices` (filtered to Portuguese and English, keeping
  the ones with no language stated) and `speech_models` (the names with a `tts` part), with a short
  cache.
- **Lifecycle.** The loop starts in `main.rs`, like the startup sweep, never in `AppState::new`
  (tests build the state without it), and stops on `shutdown_signal()`. A `Lagged` from the event
  bus triggers a full sweep. With Nants closed, Nanty sleeps; on return, what Telegram kept becomes
  cards marked as old, never an immediate action.
- **Instance lock.** A `flock` on `nanty/instance.lock` in the data folder: only the process that
  holds it runs the loop, the Telegram reader and the outbox; another process on the same data
  folder shows Nanty as active in another instance. Installations with different data folders
  never see each other; the only possible conflict is the same bot token in two of them, which
  Telegram reports as a conflict (409).
- **Language.** What comes from the dictionary is stored as `code` and `params`, and the web
  translates it; the Telegram copy is built with `tr(lang, ...)` at send time, in the `lang` stored
  in each message. The conversation answers in the language of the person (`language::person_lang`:
  the text, then the last language of the person, the Telegram `language_code` and the Nanty
  language; leaving the last language takes more than one clue, so a project name does not switch
  it). Notices and summaries follow `[nanty] language`: `follow` (the default), `pt-BR` or `en`. The
  `server::lang` middleware remembers only the interface language, which `follow` uses as the last
  resort.

## Runs that do not spin

- **Gate with no progress.** The runner signs each failure of the test round with the exit code, the
  step of the `&&` command that failed and the normalized error lines (no times, ports or temporary
  paths). The same signature in two cycles in a row goes to recovery on the second cycle, with the
  same `BlockKind`, count and evidence as the limit of 5 cycles; a tree with no change is extra
  evidence, and the Developer's prose is never read. The failed step is a fact for the Developer and
  for recovery.
- **Plan review.** `src/server/plan_review.rs` sends the steps of the test commands to the model,
  which answers with an `infeasible_test_command` finding when a step only becomes runnable after the
  first task that runs the command, with no list of tools in the code. The objective check,
  `command_not_runnable`, is a point of the server, read from the exit 126 or 127 the runner already
  measured on a step of the battery under review. Both go through the Architect and never block the
  approval.

## Stack

- Server: Rust stable, tokio, axum, reqwest (rustls), rust-embed (embeds `web/dist`).
- Interface: React 19, TypeScript, Tailwind CSS 4, Vite, vitest; local Inter and JetBrains Mono
  fonts; CodeMirror 6 (read only) in Files.

## Structure

```
src/server/         REST/SSE API, ui-state, attention, readiness, static SPA (static_ui.rs)
src/server/nanty/   Nanty, the system agent: observers, filter, actions, model turn, Telegram, voice
src/agents/         LLM clients and the specification runner
src/harness/        worktrees, allowlist, tools, Docker Compose
src/sdd/            specifications, tasks and SDD states
web/src/main.tsx    mounts the Shell
web/src/desktop/    Shell, Desktop, Window, NMenu (the Nants menu), top bar, launcher, phone,
                    window manager (windows.ts, presets.ts, shortcuts.ts, focus.ts,
                    gestures.ts, animation.ts), app state and badges (appState.ts), ui-state
                    (uiState.ts) and tokens (theme/)
web/src/apps/       one folder per window, registered in apps/registry.ts; apps/shared/ for
                    what several windows use
web/src/            API client (api/), store, event stream (liveStream.ts) and pure rules
```

A window is an `AppDefinition` in `apps/registry.ts`: `Content` (in `memo`, reads data through
`useSpace()`/`useShell()` and store selectors) and `summarize` (a pure function that gives the
subtitle, the app badge shown in the Nants menu and on the phone, and the empty state). Space data comes from `useSpaceData`, called once
by the Shell; high frequency slices (messages, activities) are read through selectors.

## Commands

```sh
scripts/test.sh cargo test                     # server
(cd web && ../scripts/test.sh npx vitest run)  # interface
(cd web && npm run build)                      # tsc --noEmit + vite build (writes web/dist)
```

## Patterns and conventions

- No `unwrap()` in Rust production code; tests live next to the code.
- Colors only in `web/src/desktop/theme/tokens.ts`; SVG icons from `Icon`, never characters.
- Every window is `role="region"` with a label; shortcuts come from the single table in
  `shortcuts.ts`.
- Permanent vitest checks: `desktop/checks.test.ts` (colors only in the tokens and icons across
  all of `web/src`), `desktop/theme/contrast.test.ts` (AA contrast) and `codebase.test.ts` (a
  single interface and no export without an import). `tsc` runs with `noUnusedLocals` and
  `noUnusedParameters`.
- Tests always run through `scripts/test.sh`, which gives each run its own temporary folder and
  removes it at the end.

## Decisions

- One SSE connection per tab, on the global stream, so the browser's HTTP/1.1 connections are
  not exhausted.
- The interface state lives on the server; `localStorage` is only a cache for the first paint.
- During gestures, `transform` and rAF; geometry is saved only at the end of the gesture and the
  window DOM is never reordered (only `z-index`), so the App iframe does not reload.

## Constraints

- Server on loopback by default; `Host` and `Origin` guard; CSP without inline scripts or CDNs.
- API keys never leave the server (only the mask); no secret in text, events or logs.
- Nanty: Telegram only by long polling (no open port), one paired owner, plain text summaries
  only, actions only from the fixed catalog, confirmed by a card when asked in free text (the mute
  is the one exception, and becomes a card in a tainted turn), read only tools with checked ids.
- Every project runs in Docker; the embedded app probe always uses `127.0.0.1`.
