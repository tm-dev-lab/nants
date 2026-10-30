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
- **Windows**: Conversation, Plan, Board, Activity, Files, App, Costs, Report, Settings, Project
  and Getting started, one per type, each with a live status subtitle and an empty state.
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

## Stack

- Server: Rust stable, tokio, axum, reqwest (rustls), rust-embed (embeds `web/dist`).
- Interface: React 19, TypeScript, Tailwind CSS 4, Vite, vitest; local Inter and JetBrains Mono
  fonts; CodeMirror 6 (read only) in Files.

## Structure

```
src/server/         REST/SSE API, ui-state, attention, readiness, static SPA (static_ui.rs)
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
- Every project runs in Docker; the embedded app probe always uses `127.0.0.1`.
