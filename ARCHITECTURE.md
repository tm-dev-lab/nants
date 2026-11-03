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
  conversation loop), `analysis.rs` (the analysis lane), `history.rs` (the history as real messages), `summarize.rs` and `summary.rs`
  (the clean summary of the conversation and its file), `memory/` (the long term memory: the files,
  the index, the search, the tools and the cards), `tools.rs` (the conversation tools), `silence.rs` (the mute of everything),
  `language.rs` (the language of the person and of Nanty), `provenance.rs` (where each message came
  from, the quarantine and the project scope), `redact.rs` (the one pattern redactor), `read.rs` (the
  reading on request, from the project snapshot, as untrusted data), `render.rs` (`code` plus `params`
  to text), `store.rs`
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
  per reply, tokens per message, the window of the history, `project_details` per turn), written only when they change;
  the budget is checked before each round, every call gets a result and the last round has no tools.
  The system prompt and the tools form a stable prefix with a fixed `cache_key`. The final text is
  plain text and goes through `format::clean`; the JSON answer is only for notices and summaries.
- **Thinking again (spec 140 T13, C-25).** There is no partner mode detected by words: the
  conversation tool `deep_think` (`think.rs`, only in `tools::Mode::Conversation`) lets the model ask
  for more effort. It is accepted once per person message (`Session::think`); the rounds after it in
  the same message go with one level above `[nanty] effort` (`think::raise`: low to medium, medium to
  high), on the same model, connection and `cache_key`, over the same messages, so the reads already
  made and their results stay and nothing is read again, and over the same `Session`, so the taint is
  never washed. The one `ChatRequest::effort` becomes `reasoning_effort` (Chat Completions),
  `reasoning.effort` (Responses and the ChatGPT plan), `output_config.effort` with adaptive thinking
  (Anthropic) and `thinking_level` (Gemini). Before accepting, the round is estimated (the current
  input plus `think::reserve` of the level, 8,192 or 16,384 tokens) and must fit in
  `max_tokens_per_message` and in what is left of the day, in US$ or in tokens. The refusals (`max`,
  `unsupported` when the adapter already dropped the effort in this message or in a bigger round of
  this process, `message`, `budget`, `repeated`) are dictionary lines to the model, and the turn goes
  on at the normal effort. A bigger round that the adapter answers only after dropping the effort
  (`fallback_used`) returns the turn to the normal effort and is remembered for the connection and
  model (`rejected`). It spends through the same `agent::account`, in the conversation budget, and
  the turn record gets `deep_think` (the outcome) and `deep_tokens`. The prompt separates fact from
  hypothesis (the state of projects is never invented; an opinion is marked in the text) and asks
  the partner to say what it would do and why, the risk, and to disagree, with no opening praise.
- **Take it to the Architect (spec 140 T15, C-15).** `draft.rs`. The model never talks to the
  Architect: `propose_to_architect(project_id, text)` (only in `tools::Mode::Conversation`, one per
  person message) checks the project is observed, runs the text through `format::clean_whole` (the
  redactor included) and refuses, never cuts, above `[nanty.chat] architect_draft_chars`. A taint
  source of another project than the destination is refused with a dictionary line and the journal
  code `architect_draft.refused`. It creates a card (`<project>:architect_draft`, action
  `architect_draft`, outside the catalog) with the frozen text, the mark and the sources in
  `card_lines`; a new proposal replaces the card and its token. The yes (`draft::decide`, before the
  memory and `act`) checks the project and the sources again (`nanty_draft_project_gone`,
  `nanty_draft_other_project`) and writes one `ArchitectDraft` per project into `state.json`
  (`NantyState::architect_drafts`, `Scoped`, so removing a project and the sweep take it). No turn
  starts: the project window reads it from `GET /api/projects/:id/chat/draft` (and drops it with
  `POST .../chat/draft/discard`), refreshed by `nanty_changed` with `part: architect_draft`. When the
  person sends, `POST /chat` carries `nanty_draft` (the web keeps the id with the text of the tab and
  with the sent message, for the resend). `draft::for_turn` decides from the server alone: with a
  marked draft pending, every person message skips `internet_setup` and gets
  `prompts::architect_draft_note` next to the project state; it carries the draft by the id or by
  its whole text, and `draft::consume` removes it only after the turn ends with success. The person
  message that ran offline stores `offline: true` in its `ChatTurn` (`.chat.jsonl`; old lines have no
  mark), and an internal turn also skips `internet_setup` while the last person message in the
  history has it, so the chain survives a restart; the next person message without the mark ends
  it.
- **Analysis lane.** `analysis.rs` runs an analysis turn (the proactive scenario) in its own task and
  in a lane of its own, one at a time, never through the conversation queue: the conversation, the
  notice and the Telegram outbox never wait for it. A second analysis asked while one runs is
  dropped with the `busy` code, not queued, so a stale case is never analyzed late. It spends from the
  same daily budget, with its own limits in `[nanty.analysis]` (provisional: 90 s, 4 rounds, 25,000
  tokens), written only when they change; with no budget it does not start, and it stops before the
  next round when the budget runs out. A cancel condition (a token, or a closure checked between
  rounds, after each answer and before each tool) ends it with the `cancelled` code; it only returns
  the text, so nothing is written halfway. The avatar counts each thinking turn, so the end of one
  never clears the thinking state of another. The analyses may spend at most a share of the daily
  budget (`budget_pct`, provisional 30%; `Spend::analysis_*`), checked before it starts (`share`) and
  before each round, and `spawn` forces the birth of the session: the analysis mode, the taint with
  the kind and the project of the notice, and the pin to that project (`Session::pinned`, checked by
  the dispatcher in every tool).
- **Proactive scenario.** `scenario.rs` turns an important notice into a separate message, if
  `[nanty.analysis] proactive` is on (off by default). The trigger is a state, checked on every pass of
  the observer (`tick`, from `model_turns`): an open `run_blocked` or `review_loop` card, or a
  `spec_done` card, of a watched project, after the card went out (its own Telegram message, or the
  window after the settle wait) and outside quiet hours, the global mute, the summary and the burst.
  The case key is `project:spec:task:signal:fingerprint` of the stable facts, stored in
  `NantyState::scenarios` (with `Scoped`, so removing a project clears it), and a case counts only
  after `analysis::spawn` returns `Ok`; with `busy`, it waits for the next pass. Ceilings per day and
  per spec live in `[nanty.analysis]`. The input is only the signal and the project (name, structured
  facts and state); `spec_done` goes with no tools, only with the `report.md` read by the safe reader
  and sealed. The cancel condition is the card leaving. The output goes through `guard` (8 words in a
  row of what was read, a path or a text shaped like a command drop it whole, and the message goes
  with only the lead and the `nanty.scenario.withheld` line), `format::clean` and the ceiling
  (`scenario_chars`, 900), and becomes a message with `code: nanty.scenario.lead` (a lead line, before
  the text), no card ids, the taint of the session and the project ids. The outbox keeps it out of the
  burst. It has no audio: the notice already spoke.
- **History and summary.** `history.rs` sends the conversation as real messages, in a fixed order:
  system, tools, the index of the long term memory (`memory/`, as data, with its own ceiling of
  `[nanty.memory] index_max_bytes`), the summary as data, the history, and last the message with `now`, the projects, the
  notices and the text of the person. The window has no time limit and moves in steps: it holds the
  messages the summary does not cover yet, up to `[nanty.chat] window_messages` (provisional: 60);
  at the ceiling, `summarize.rs` turns the oldest `window_step_messages` (provisional: 20) into one
  item, so the start of the history changes only once every 20 messages and the provider cache
  holds. `history_max_tokens` (provisional: 12 thousand, by the estimate of the project) cuts the
  oldest first. Each message carries its date and time and its own language. A pure function joins
  turns in a row, opens the list with the person, drops control only messages and keeps a fixed
  notice to one line; the history has no `provider_items` and works for Chat Completions,
  Responses, the ChatGPT plan, Anthropic and Gemini. The summary runs after the reply, outside the
  turn queue and one at a time, with no tools, sealed input and a ceiling, and it counts in the
  budget. It sees only clean text: a tainted reply and third party text arrive as the server line.
  Its text goes through the redactor before it is written, each item keeps its `project_ids`, and
  it follows the mark of `summary.json` (in `seq` order, never back). With no model or no budget,
  the step leaves the server line instead. `summary.json` stays under 128 KiB, the oldest items
  leaving first, and the context takes the newest that fit in 6 thousand characters.
- **Tools.** `project_status`, `system_status` and `nanty_activity` are trusted; `project_details`
  is not: it goes in the untrusted data envelope and taints the turn (`Session::tainted`), as does a
  forwarded message or a third party quote. `tools::Mode` lists the tools of each mode, and the
  dispatcher checks that list before the `match`: a tool not offered, called by name, runs nothing.
  Deep reads (`deep.rs`, spec 140 T10) are four conversation tools, one per family: `agent_log`
  (the end of `dialog.jsonl` or `tools.jsonl` of a spec, read backwards from the tail, with task,
  text and `skip` filters), `sdd_docs` (specs, spec, tasks, a task, its `test_evidence` from the end
  and its recovery and diagnosis sections), `project_files` (listing and line ranges, including
  `ARCHITECTURE.md` and the roadmap) and `git_history`. Without `spec_id` or `task_id`, the phase of
  `observe::project_facts` decides, then the newest spec. Everything goes through `files::Root`;
  each field is redacted whole before its cut, the result goes in the envelope, and the sum of what
  returns to the model spends the turn `files::Budget` (`[nanty.read]`). Every admitted read adds
  its `TaintKind` (dialog, trail, test output, project file or Git) with the project to
  `Session::tainted`. Cross project isolation (C-16) lives in the dispatcher: `Session::admit` lets a
  second project in only while the model has seen no content yet (the same request), and
  `Session::end_round` closes that window when the results go back; `project_details` follows the
  same rule. Errors reach the model as `nanty_read_*` and `nanty_git_*` codes with the phrase of
  `nanty.read_error.*`.
  `propose_action` accepts only what `act::requestable`
  offers. `set_silence` takes `hours` or `until` (`HH:MM`, its next occurrence, at most 24 hours away)
  and applies at once in a clean turn; in a tainted turn it becomes a card that keeps the same
  request and expires at the latest at the time it shows. The urgent still arrives during a mute, without sound:
  `filter.rs` treats it like quiet hours.
- **Safe reader and neutral Git (spec 140 T09).** `files::Root` resolves the project folder once and
  opens everything below it from that descriptor: `openat2` with `RESOLVE_BENEATH |
  RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS`, or a component walk by `openat` with `O_NOFOLLOW`
  when the kernel refuses `openat2`, so there is no window between checking and opening. It blocks
  by folder (`files::BLOCKED_DIRS` and `.sdd/kb/sources`) and by secret name in any component,
  refuses private keys and binary content, and has ceilings per file, per listing and per turn,
  with line ranges; `Root::tail` reads the end of the agents' JSONL records. Names go inside the
  envelope, and errors are codes with no path. `git_log` runs `git log` with a clean environment,
  no system or global configuration, the settings that run programs neutralized by `-c`, a
  deadline, `-n` and a 1 MiB output cap, only the summary (no `-p`); a `.git` link or a `gitdir:`
  outside the project is refused. The runner's `harness::git` applies `NEUTRAL_CONFIG` to every
  call and moves the `.git` of a nested repository created by an agent to `nants-nested` in the
  private Git folder before `add`, so a nested `fsmonitor` or `clean` filter never runs on the host.
  Each folder moved is also noted in `nants-nested-notice`, next to it, and the next `add_all` that
  succeeds returns them (and clears the note), so the runner's notice is not lost when a later
  folder fails in the middle.
- **Provenance.** Taint outlives the turn. Every Nanty message stores `taint: {sources: [{kind,
  project_id}]}`, taken from `Session::tainted`, and every reply of a tainted turn is tainted too. A
  Nanty line with no such field is an old line and counts as tainted, kind `unknown`. A turn is clean
  when its context has none of the kinds of `provenance::TaintKind`: file content or name, test
  output, tool trail, agent dialog, Git history, knowledge base, web, forwarded text or third party
  quote, imported memory. Task ids reach the model only in the SDD form (`T` and up to four digits),
  and the spec id does not reach it: it is free text of up to 64 characters written in the project.
- **Quarantine.** The text of a tainted message stays whole only in the last
  `[nanty.chat] quarantine_messages` messages (provisional: 6), and the turn is tainted while it is
  there. Older, it becomes a line written by the server with the time, the projects and the kind of
  read, from structured fields only, and the turn is clean again; to resume, the model reads again.
  The words of the person stay whole, and only third party text leaves. An heir (a reply written in
  a tainted turn) stores in `taint.origin` the message where the taint began and leaves with it, so
  a copy never outlives the text and the chain ends instead of renewing itself at every reply. A
  fresh read in the turn itself (`project_details`, the deep reads and the recall of imported
  memory) restarts the origin there: the reply that just read stays whole for its own K messages
  and taints the turns that see it. Accepted risk: a copy of inherited text can last up to K
  messages after the last real read, always in a tainted turn, with actions only by click and
  memory only by card.
- **Modes and what each one may do.** Sending to the owner on Telegram is communication with a fixed
  recipient, an accepted exception to the last column.

  | Mode | Untrusted data | Private data | Changes state or leaves |
  |---|---|---|---|
  | Clean conversation | no | yes | memory and cards |
  | Tainted conversation | yes | yes | only with a click |
  | Proactive analysis | yes | only the project of the notice | only the message to the person |

- **One redactor.** `redact::scrub` adds patterns to the known secrets of `SecretSet`: key prefixes,
  JWT, `Bearer`, `KEY=value` in sensitive names, URL credentials, PEM and high entropy strings. It
  runs on every tool result, inside `format::clean` (the final reply and all that goes to Telegram)
  and on card text, and it is the API for the memory and the summary. Hexadecimal never counts as
  high entropy, so commit hashes, UUIDs and ids stay readable.
- **Isolation between projects.** A message keeps every project it touched in `project_ids`, and
  `provenance::Scoped` is the contract of whatever stores text tied to projects: removing a project
  and excluding it from observation go through it, whole message included, because the text cannot
  be split by project.
- **What it may offer.** `can_do.rs` calculates, per channel and turn, the buttons of the cards, what
  `propose_action` accepts and the conversation tools; it goes in the input of notices and
  conversation. The prompts let Nanty offer only `actions` and `conversation`, which a plain yes can
  fulfil; `buttons` work only by touch, so it may just point to them. `open_nants` is a tool effect (`link.rs`):
  the server adds the way in. On Telegram that is a URL button built from `[nanty] address` or from
  the last `Host` of an authorized device (`NantyState::device_host`, read only in network mode), and
  if Telegram refuses the button the message goes again with the address as text. With no address, it
  says to open Nants on the computer. In the window it opens the project screen, with no link.
- **Text of a message.** With model text, `render::message_text` puts Nanty's words first and the
  server sentence last (`render::closing`), so the cut of the caption and of the speech keeps it.
  Only the summary (`digest::LEAD`) opens with the title and the kept whys. The why of each notice always comes
  from the dictionary.
- **A card too long for Telegram.** `outbox::fit_card` drops the "yes" of a card whose redacted
  text passes one message and adds the dictionary line that points to the window; the card id goes
  to `NantyState::window_only_cards`, apart from `telegram_sent` (which has a ceiling and forgets the
  card when it ends), and `telegram::on_callback` refuses a "yes" for that card while it lives (the
  "no" carries the same token), journal code `telegram.yes_window_only`. A restore that swaps a note
  journals the one that left as `memory.swapped`.
- **Long replies.** Cleaning and cutting are apart: the conversation stores the whole reply, cleaned
  by `format::clean_whole` and never cut at 4096, and the window shows it whole. Telegram splits it
  with `format::split` into up to `MAX_PARTS` (3) messages, at a paragraph, line or sentence break or
  a space, never inside a word or a masked secret, keeping `LAST_PART_ROOM` in the last one for the
  lines of a refused keyboard. Above three, the last part ends with a dictionary line that points to
  the window. The keyboard (the way in and the delete buttons) and the server sentence go in the last
  part, the one recorded for later edits; the parts leave one per call and in order, before anything
  else, and never go into the queue summary. With voice on, a reply that opens with the model and
  does not fit a caption speaks only the start of its first paragraph, up to `[nanty.voice]
  lead_chars` (provisional: 450) at a sentence end, with no extra summary call (`voice::long_lead`):
  the caption carries the same text and says that the rest follows, and the parts carry only the
  rest. The spoken part always fits the caption (`lead_chars` is capped at 900 and the part steps
  back to a sentence end within the caption), so nothing is spoken without being written. With a
  keyboard, even a single message keeps the room for the refusal lines, so the server sentence
  survives a refused button. In the history, a reply above 1500 characters keeps its first 1000 and last 400 with a
  server mark in between (`history::trim_reply`).
- **Voice.** Gemini only, then text; the OpenAI steps of an old `order` are read, dropped with a line
  in the record and never written. `voice::deliver` stores the text first, then generates the speech
  outside the send queue, one at a time and with a deadline, and sends the message once.
  `GET /api/nanty/voices` serves the Settings: `voices` (filtered to Portuguese and English, keeping
  the ones with no language stated) and `speech_models` (the names with a `tts` part), with a short
  cache.
- **Tone of the voice (spec 140 T01, C-28).** `voice/tone.rs` has a closed `Tone` list (neutral,
  upbeat, serious, concerned, calm), and only `Tone::style`, a short English text fixed in the code,
  reaches the provider; neutral sends nothing. Notices take it from `SignalKind::tone`, an exhaustive
  `match` with no wildcard. In the conversation the model writes a structural marker on the first
  line, read by a strict parser (`take_marker`) and removed before `format::clean`; an invalid value
  is neutral, and a tainted turn is always neutral. A `bad_request` with the style is retried once
  without it, and that call is checked against the budget and charged.
- **Spoken notice with the model text (spec 140 T02, C-29).** With a model and budget, the outbox
  waits inside `start_speech` for the text of the notice turn up to `NOTICE_TEXT_WAIT` (15 s;
  `NOTICE_TEXT_WAIT_URGENT`, 8 s), within `NOTICE_TOTAL` (60 s) from the start of that speech, and
  then speaks it in the one voice note of the card, with the model text and the why of the
  dictionary in the caption (`format::fit_keeping`). `SentCard::text` keeps that text for edits,
  and `voice::keep_notice` stores what was spoken, so the proactive reply is not repeated. Past the
  wait, or with a refusal, no budget or a model failure, the card goes the usual way with the fixed
  sentence and the late text follows as a message. Only with a single new card; a burst keeps the
  old behavior.
- **Conversation on disk (spec 140 T05 and T06, C-20).** `store::Conversation` wraps a `JsonlLog`
  that rotates at `[nanty.storage] conversation_max_bytes` (provisional: 1 MiB, at most 8 MiB) and
  keeps `conversation_generations` (2) older files under `conversation_total_bytes`. The oldest one
  leaves only when the mark of `summary.json` covers all its lines; with the summary off for lack of
  budget it leaves and the server line enters the summary, and above `jsonl::HARD_MAX_GENERATIONS`
  (32) it leaves anyway, with a note in the journal. A generation that exists and cannot be read is
  held, never counted as empty. Every line has a `seq`: the lines of older versions get one at the
  first write, and with an unreadable generation they stay without one (never covered, so never
  dropped without a summary), and the numbering is tried again only after `SEQ_RETRY` (60 s) or
  after some line left the file, so a write never rereads every generation. Audio has its own
  ceiling by bytes and age (`audio_max_bytes`, `audio_max_age_days`), the speech leaving before the
  text, and an index of the cited audio ids that is rebuilt only after a removal. `summary.json` is
  its own file, never `state.json`. All files are 0600 in a 0700 folder.
- **Budget (spec 140 T17, C-19).** `turn::budget_mode` decides by the main Nanty model: priced and
  not on the plan is US$, otherwise tokens (input plus output, cache reads counted in full). The
  defaults come from the measurement of T16 (`DEFAULT_DAILY_BUDGET_USD` 1.00 and
  `DEFAULT_DAILY_BUDGET_TOKENS` 600,000, chosen for the Gemini prices of 2027, which double on
  2027-01-01) and are written only when they change. Everything goes through `Spend`: turns,
  summary, analysis (`Spend::analysis_*`, at most `[nanty.analysis] budget_pct` of the day),
  `deep_think` and voice. Another route of Nanty (speech and transcription) is charged by
  `turn::route_cost`: its own price, or, in US$ mode with no price, an estimate at the price of the
  main model, so the voice always counts.
- **Memory tab.** `GET /api/nanty/memory` lists the topics (with the state of each file), the notes,
  the deleted ones still in time to come back (`restore_until_ms`) and `stale` on the notes that left
  the index for lack of use. `PUT /api/nanty/memory/:id` edits over the `updated` the tab read (a
  newer one is a 409 `nanty_memory_conflict`, and the tab shows the saved version and lets the person
  choose), and `POST .../delete` and `.../restore` delete for `deleted_keep_days` and bring back.
  A restore over an active note of the same topic and title (the newer version of a `replace_id`)
  swaps them: the active one goes to the deleted, with the same time to come back.
  `POST /api/nanty/memory/export` is the copy the tab downloads: a POST, so no navigation from another
  site triggers it with the `Lax` cookie, with only the active notes of a general topic or of a
  project still registered, redacted again with the secrets of now. Each change and each export goes
  to the journal (`memory.edited`, `memory.deleted`, `memory.restored`, `memory.exported`) with ids
  and codes only. Import comes later.
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
  is the one exception, and becomes a card in a tainted turn), read only tools with checked ids,
  deep reads only through the safe reader, and a second project only in the same request as the
  first.
- Every project runs in Docker; the embedded app probe always uses `127.0.0.1`.
