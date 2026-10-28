---
id: 001-household-ledger
title: Household ledger reconciler
status: COMPLETED
base_branch: main
created_at: 2026-10-27T18:37:05Z
test_command: python -m pytest -q
formatters:
- name: black
  command: python -m black .
  workdir: .
  paths:
  - '**/*.py'
environment: docker
install_command: python -m pip install --no-compile -r requirements.lock
lockfiles:
- requirements.lock
---
## Contexto e Problema
A household needs a local, offline command-line workflow to combine monthly bank and card exports, categorize transactions, suppress overlapping imports, reconcile internal transfers, and export auditable monthly reports. There is no graphical interface.

## Abordagem Técnica
Use Python 3.12 with argparse, sqlite3, csv, Decimal and pathlib. Use PyYAML 6.0.2 for configuration, pytest 8.3.5 for tests, and Black 25.1.0 for formatting. Pin all direct and transitive dependencies exactly in requirements.lock, resolved inside the container. No build, lint or typecheck stage is introduced.

The Docker workspace uses python:3.12.12-slim-bookworm, mounts the worktree at /work, and installs dependencies into ignored /work/.deps with PIP_TARGET and PYTHONPATH. It needs no services, host language runtime, published ports, or HTTP URL. Installation may access the package index; tests must not access any network. Fast unit and subprocess CLI integration tests form a single suite.

### CLI and configuration contract
Expose `python -m ledger` and an executable repository launcher `./ledger-cli` with the same argparse entry point. The launcher can be placed on PATH as `ledger`, providing the requested command name without colliding with the package directory. Global options `--config` and `--db` precede subcommands and default to ledger.yaml and ledger.sqlite3. Implement `import <file> --account <name>`, `recategorize`, `set-category <id> <category>` and `report --month YYYY-MM --output-dir <directory>`; output directory defaults to reports. Expected failures exit nonzero with a concise stderr message and no traceback. Success messages include inserted/skipped counts or generated report paths. Invalid arguments, unknown accounts and unknown transaction IDs do not mutate existing ledger data.

YAML contains an `accounts` mapping and ordered `rules` list. Each account selects `layout: signed` or `layout: debit_credit`; optional `columns` maps canonical keys to source headers. Canonical headers are `date`, `description`, `amount`, or `date`, `description`, `debit`, `credit`. Rules contain `pattern`, `category` and optional inclusive signed integer `min_cents`/`max_cents`; patterns are case-insensitive Python regular expressions searched against the normalized description. Validate types, required fields, ranges, nonempty category names and regex compilation before mutation. Load with safe_load. Account names identify household-owned accounts in one currency.

### Parsing and atomic import
Try UTF-8 with optional BOM first, then Latin-1. Detect comma or semicolon delimiter from the header against required mapped fields and validate every record. Quoted decimal-comma cells work in comma-delimited files. Trim headers and surrounding field whitespace; normalize description internal whitespace for display and fingerprinting. Dates accept only DD/MM/YYYY or YYYY-MM-DD and must be valid calendar dates. Decimal point or comma is accepted without thousands separators; optional leading sign is allowed in signed layout. Reject exponent notation, nonfinite amounts, ambiguous separators and nonzero sub-cent fractions. Integer and one-decimal amounts are accepted and converted exactly to cents. In split layout, debit and credit cells are nonnegative, exactly one must be nonzero, and an empty other cell means zero; debit becomes negative. Signed zero rows are valid but never transfers or top expenses.

Ignore wholly blank lines. All other records require date and nonempty description. Missing columns refer to header line 1. Record errors identify the input path, physical starting line of the CSV record, and field/reason; malformed quoting also fails. Parse and validate the complete file before committing. Any failure leaves transaction rows, categories, import records and transfer matches unchanged.

### Persistence, categorization and deduplication
Store integer primary keys, account, ISO date, normalized description, signed cents, category, manual flag, and transfer linkage in SQLite. Add schema version initialization and foreign keys; creating the same database twice is safe. Preserve source provenance sufficient to diagnose imports. Do not log statement contents unnecessarily.

Duplicate identity is account + date + casefolded normalized description + signed cents. For each fingerprint, keep the maximum occurrence count observed in any one imported statement, rather than summing counts across statements. This preserves two identical purchases within one statement while suppressing overlap and exact reimports. Identical entries in different accounts remain distinct. This policy cannot distinguish additional identical purchases split across statements; document that limitation. Existing IDs, manual category flags and edits survive deduplication. Exact reimports do not add provenance records. File hashes may optimize exact reimports but must not define overlap detection.

The first matching YAML rule wins; unmatched entries use `Uncategorized`. New imports are categorized automatically. `set-category` marks an existing row manual; `recategorize` updates only nonmanual rows using the current YAML. Manual category provenance is independent of transfer matching.

### Internal transfers
After each successful import, recompute matches over the ledger atomically. Eligible pairs have opposite nonzero signed cents of equal magnitude, different configured accounts, and absolute calendar-day difference at most 3, including cross-month matches. Build candidates ordered by day distance, debit date, debit ID, credit date, credit ID; greedily pair unused rows one-to-one. Persist a common linkage for both rows. Never match a row twice, a same-account pair, a zero row, or a lone debit. Preserve categories and manual flags while recomputing. This is an amount/date heuristic and may match unrelated equal-value payments; document the limitation. Determinism is for a fixed ledger and stable IDs, not guaranteed identical pair selection under different insertion orders when candidates tie.

### Monthly report contract
Read ledger state without recategorizing. Validate YYYY-MM strictly. Write UTF-8 files `<month>.md` and `<month>.csv`, replacing those files on rerun, without changing ledger data. Include all transactions dated within the month, excluding both sides of matched internal transfers from every financial section. Category totals are net outflow cents: negative of the sum of signed amounts, so income or net refunds produce negative totals. The category table uses the union of current and previous calendar month's categories, sorted by category, with missing values zero; change equals current minus previous, not a percentage. Handle January rollover.

Top expenses are the 10 most negative nontransfer transactions, ordered by amount ascending, then date and ID ascending. Display their expense magnitude as positive cents. Uncategorized entries include all current nontransfer rows categorized `Uncategorized`, ordered by date and ID, with signed amounts. Empty sections include an explicit no-data message in Markdown and no fabricated transaction rows in CSV.

CSV is one rectangular file with header `section,category,total_cents,previous_cents,change_cents,transaction_id,date,description,amount_cents`. Rows use `category`, `top_expense` or `uncategorized` in section, leaving inapplicable fields empty. Markdown represents precisely these values with two decimal places and proper escaping of pipes/newlines; CSV uses correct quoting. No currency symbol is assumed. Markdown and CSV totals, rankings and uncategorized IDs must agree exactly.

## Premissas
- The request is sufficiently complete; no user decisions are pending.
- English CLI, errors, examples, reports and documentation follow the language of the request.
- One trusted household uses local files with serialized writes; authentication, concurrent collaboration and multiple currencies are excluded.
- Supported layouts use configurable explicit header mappings, not bank-specific inference.
- Signed amounts are credits positive and debits negative, including credit-card exports after normalization.
- Rule amount ranges use signed cents; reports use net outflow with explicit negative income totals.
- No network is needed after dependencies are installed; no bank APIs, charts, budgets, alerts, desktop or web UI are included.

## Critérios de Aceitação Globais
- [ ] G1: The Docker workspace runs the offline pytest suite with one command using exactly pinned dependencies and isolated temporary data.
- [ ] G2: Both CSV layouts and every required encoding, delimiter, date and decimal style import correctly and reimport idempotently.
- [ ] G3: Bad dates, amounts, missing columns and malformed records report path, line and reason with no partial ledger changes.
- [ ] G4: First-match rules, inclusive ranges, Uncategorized fallback and manual fixes behave correctly through recategorization.
- [ ] G5: Overlapping imports preserve fingerprint multiplicities and deterministic one-to-one transfers exclude only matched rows from financial reports.
- [ ] G6: Seeded monthly Markdown and CSV agree exactly on integer-cent category totals, top 10, previous-month changes and uncategorized rows.
- [ ] G7: README and fictional samples provide a complete Docker-based import-to-report session with executable, tested commands.

## Plano de Tarefas
1. T01: CLI skeleton, dependency lock, database foundation and offline pytest infrastructure.
2. T02: YAML validation, rule engine and atomic CSV import, depends on T01.
3. T03: Persistent duplicate handling and manual recategorization commands, depends on T02.
4. T04: Atomic transfer reconciliation, depends on T03.
5. T05: Deterministic Markdown and CSV monthly reporting, depends on T04.
6. T06: Executable launcher and subprocess CLI parity tests, depends on T05. Scope: ledger-cli and tests/test_launcher.py.
7. T07: Fictional example workflow, integration tests and Docker README, depends on T06. Scope: examples/**, tests/test_example_session.py and README.md.
All implementation tasks keep the single test command green, introducing tests with their corresponding functionality. Environment files are approved with this draft and are outside task scopes. T07 retains its ID after splitting launcher work into T06; execution order is T01 through T05, then T06, then T07.

## Orientações por papel
### Desenvolvedor
Use Decimal only at parsing/formatting boundaries and integer cents throughout reconciliation and aggregation. Keep configuration parsing, CSV normalization, database operations and report rendering separable for pytest fixtures. Use sqlite3 explicit transactions for import plus reconciliation, avoiding implicit commits from executescript. Validate late-row failures against preexisting manual and transfer state. Run subprocess CLI tests with sys.executable and temporary paths so they do not depend on shell aliases or the example database.

### Revisor
Check overlap multiplicity rather than only file hashes, two identical purchases, day-3 versus day-4 transfers, many-to-one candidates, and preservation of manual flags. Verify debit/credit sign conventions match report net-outflow arithmetic, including income, refunds and January rollover. Confirm report tests compare independently calculated expected cents, not two renderers sharing the same incorrect output. Confirm README commands use the Docker workspace and the same fictional sample amounts as integration tests.

## Renumbering

On approval, the tasks were renumbered in execution order: T07→T06, T06→T07. The planning records (usage.jsonl, tools.jsonl, .context.jsonl and .chat.jsonl) keep the old ids, and this map explains the correspondence.

## Persistência da instalação
- Install check on the base (`python -m pip install --no-compile -r requirements.lock`, code 1): 0 entries written outside /work, in 9 s (warning limit: 10).
