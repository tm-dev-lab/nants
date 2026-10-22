/*
 * Janela Quadro (`DEFAULT_APPS.board.Content`, em `memo`): colunas configuráveis (estado em `selection.boardColumns`),
 * cartões com tentativa, veredito, dono, dependências, tempo, custo e selo de recuperação,
 * "Ações (N)" numa folha dentro da janela, a última revisão da tarefa em foco, seleção por
 * teclado e animação de troca de coluna.
 *
 * Responsividade pelo tamanho da janela (`data-size`): ampla e média, colunas lado a lado com
 * rolagem própria; estreita, as colunas viram uma lista com a rolagem dentro da janela.
 *
 * Desempenho: lê o espaço pelo contexto e só fatias estáveis do store (recuperações, a última
 * mensagem do Revisor da tarefa em foco, se a verificação final está ativa). A rajada de
 * atividade ao vivo não re-renderiza o Quadro.
 */
import {
  memo,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import type { SpecView, TaskStatus, TaskView } from "../../api/types";
import { TASK_VERDICT_LABEL } from "../../copy";
import { EmptyState } from "../../desktop/EmptyState";
import { Icon } from "../../desktop/Icon";
import { useShell, useSpace } from "../../desktop/shellContext";
import type { ContainerSize } from "../../desktop/useContainerSize";
import { useWindowVisibility } from "../../desktop/useWindowVisibility";
import { useWindowHost } from "../../desktop/windowHost";
import { useT } from "../../i18n/react";
import { recoveryKey, useAppStore, type RecoveryEntry } from "../../store";
import { taskUsageLine } from "../../usageFormat";
import type { AppContentProps } from "../registry";
import { AgentAvatar } from "../shared/AgentAvatar";
import { FinalVerificationNotice } from "../shared/FinalVerificationNotice";
import { ReviewerChecklist, ReviewFindingList, ReviewVerdictBadge } from "../shared/ReviewMessage";
import { MarkdownText } from "../shared/ReportMarkdown";
import { taskUsageOf, useUsageView, type TaskUsageMap } from "../shared/useUsage";
import "../shared/apps.css";
import "./board.css";
import {
  boardEmptyMessage,
  buildColumns,
  CARD_OWNER,
  CARD_VERDICT_LABEL,
  columnIndex,
  FINAL_TASK_ID,
  lastReviewLine,
  lastReviewMessage,
  movedCards,
  OPTIONAL_COLUMNS,
  parseBoardColumns,
  parsedReview,
  pickActiveTask,
  recoveryLabel,
  statusLabel,
  STATUS_TONE,
  toggleBoardColumn,
  toolActionsLabel,
  type BoardColumn,
} from "./boardModel";
import { ToolActionsSheet } from "./ToolActionsSheet";

type Recovery = Pick<RecoveryEntry, "phase" | "cycle">;

/** Colunas em que a tarefa está em andamento (o dono pulsa). */
const WORKING: ReadonlySet<string> = new Set(["IN_PROGRESS", "IN_REVIEW", "INTEGRATING"]);

/** Id do cartão sob `target` (ou `null` fora do quadro). */
function cardIdOf(target: EventTarget | null, board: HTMLElement | null): string | null {
  if (!(target instanceof Element) || board === null) return null;
  const card = target.closest("button[data-task-id]");
  if (card === null || !board.contains(card)) return null;
  return card.getAttribute("data-task-id");
}

function BoardContentView({ size }: AppContentProps) {
  const space = useSpace();
  const shell = useShell();
  // Assina o idioma: o conteúdo (em `memo`) re-renderiza na troca.
  const { t } = useT();
  const spec = space?.data.activeSpec ?? null;
  if (space === null) return null;
  if (spec === null || spec.tasks.length === 0) {
    return <EmptyState icon="board" message={boardEmptyMessage()} action={{ label: t("windowEmpty.openPlan"), onClick: () => shell.openWindow("plan") }} />;
  }
  return (
    <Board
      projectId={space.data.projectId}
      spec={spec}
      size={size}
      selectedTask={space.selection.taskId}
      boardColumns={space.selection.boardColumns}
      setSelection={space.setSelection}
    />
  );
}

export const BoardContent = memo(BoardContentView);

interface BoardProps {
  projectId: string;
  spec: SpecView;
  size: ContainerSize;
  selectedTask: string | null;
  boardColumns: readonly string[] | undefined;
  setSelection: NonNullable<ReturnType<typeof useSpace>>["setSelection"];
}

function Board({ projectId, spec, size, selectedTask, boardColumns, setSelection }: BoardProps) {
  const host = useWindowHost();
  const { t, lang } = useT();
  const reduced = host.reducedMotion();
  const visible = useWindowVisibility("board");
  const usage = useUsageView(projectId, spec.id, visible);
  const taskUsage = useMemo(() => taskUsageOf(usage, spec.id), [usage, spec.id]);
  const recoveries = useAppStore((s) => s.recoveries);
  const shown = useMemo(() => parseBoardColumns(boardColumns), [boardColumns]);
  // Os rótulos das colunas seguem o idioma: ele entra nas dependências.
  const columns = useMemo(() => buildColumns(spec.tasks, shown), [spec.tasks, shown, lang]);
  const placement = useMemo(() => columnIndex(columns), [columns]);
  const activeId = pickActiveTask(spec.tasks)?.id ?? null;
  const focusedId = selectedTask !== null && spec.tasks.some((t) => t.id === selectedTask) ? selectedTask : activeId;
  const [actionsTask, setActionsTask] = useState<string | null>(null);
  const opener = useRef<HTMLElement | null>(null);
  const boardRef = useRef<HTMLDivElement>(null);
  const cardRefs = useRef(new Map<string, HTMLButtonElement>());
  const lastFocused = useRef<string | null>(null);
  const prevPlacement = useRef<Map<string, string> | null>(null);
  const toolCounts = spec.tool_counts;
  const finalActions = toolCounts?.[FINAL_TASK_ID] ?? 0;
  const narrow = size === "narrow";

  // Animação só para os cartões que trocaram de coluna desde o render anterior.
  const entered = useMemo(() => movedCards(prevPlacement.current, placement), [placement]);
  useEffect(() => {
    prevPlacement.current = placement;
  }, [placement]);

  const recoveryOf = useCallback(
    (taskId: string): Recovery | undefined => {
      const entry = recoveries[recoveryKey(projectId, spec.id, taskId)];
      return entry !== undefined && entry.phase !== "escalate" ? entry : undefined;
    },
    [recoveries, projectId, spec.id],
  );

  // O último cartão focado pelo usuário dentro do quadro (o foco volta a ele ao trocar de coluna).
  useEffect(() => {
    const track = (event: Event): void => {
      lastFocused.current = cardIdOf(event.target, boardRef.current);
    };
    document.addEventListener("focusin", track);
    document.addEventListener("pointerdown", track);
    return () => {
      document.removeEventListener("focusin", track);
      document.removeEventListener("pointerdown", track);
    };
  }, []);
  // O cartão é recriado ao mudar de coluna e o foco cai no body: devolve ao mesmo id.
  useLayoutEffect(() => {
    const id = lastFocused.current;
    if (id === null) return;
    const active = document.activeElement;
    if (active !== null && active !== document.body) return;
    const card = cardRefs.current.get(id);
    if (card !== undefined && card.isConnected) card.focus({ preventScroll: true });
  });

  const select = useCallback((id: string) => setSelection({ taskId: id }), [setSelection]);
  const openActions = (task: string, from: HTMLElement): void => {
    opener.current = from;
    setActionsTask(task);
  };
  const closeActions = (): void => {
    setActionsTask(null);
    const back = opener.current;
    opener.current = null;
    // O quadro sai do `inert` no próximo render; o foco volta ao link que abriu a folha.
    requestFocus(back);
  };

  function focusCard(task: TaskView | undefined): void {
    if (task !== undefined) cardRefs.current.get(task.id)?.focus();
  }

  function onCardKeyDown(event: ReactKeyboardEvent<HTMLButtonElement>, col: number, row: number): void {
    const current = columns[col]?.tasks ?? [];
    switch (event.key) {
      case "ArrowDown":
      case "ArrowUp": {
        event.preventDefault();
        const next = event.key === "ArrowDown" ? row + 1 : row - 1;
        if (next >= 0 && next < current.length) focusCard(current[next]);
        break;
      }
      case "ArrowRight":
      case "ArrowLeft": {
        event.preventDefault();
        const step = event.key === "ArrowRight" ? 1 : -1;
        for (let c = col + step; c >= 0 && c < columns.length; c += step) {
          const target = columns[c]?.tasks ?? [];
          if (target.length > 0) {
            focusCard(target[Math.min(row, target.length - 1)]);
            break;
          }
        }
        break;
      }
      default:
    }
  }

  const hasBlockedTask = spec.tasks.some((t) => t.status === "BLOCKED");
  const register = (id: string) => (el: HTMLButtonElement | null) => {
    if (el === null) cardRefs.current.delete(id);
    else cardRefs.current.set(id, el);
  };

  return (
    <div className="nd-board-root" data-board="">
      <div className="nd-board-main nd-app-scroll" inert={actionsTask !== null}>
        <FinalVerificationNotice projectId={projectId} />
        <div className="nd-board-toolbar">
          {finalActions > 0 ? (
            <p className="nd-row nd-muted" data-final-actions="">
              <span>{t("board.finalVerification")}</span>
              <button
                type="button"
                className="nd-link-btn"
                data-tool-actions={FINAL_TASK_ID}
                aria-haspopup="dialog"
                aria-label={t("board.finalActionsLabel", { actions: toolActionsLabel(finalActions) })}
                onClick={(event) => openActions(FINAL_TASK_ID, event.currentTarget)}
              >
                {toolActionsLabel(finalActions)}
              </button>
            </p>
          ) : (
            <span />
          )}
          <ColumnsMenu shown={shown} forcedBlocked={hasBlockedTask} onToggle={(status) => setSelection({ boardColumns: toggleBoardColumn(shown, status) })} />
        </div>
        <div
          ref={boardRef}
          role="region"
          aria-label={t("board.region")}
          className="nd-board"
          data-board-layout={narrow ? "list" : "columns"}
          style={narrow ? undefined : { gridTemplateColumns: `repeat(${columns.length}, minmax(150px, 1fr))` }}
        >
          {columns.map((column, col) => (
            <Column
              key={column.key}
              column={column}
              narrow={narrow}
              renderCard={(task, row) => (
                <TaskCard
                  key={task.id}
                  task={task}
                  active={task.id === activeId}
                  focused={task.id === focusedId}
                  animate={!reduced && entered.has(task.id)}
                  register={register(task.id)}
                  onSelect={() => select(task.id)}
                  onKeyDown={(event) => onCardKeyDown(event, col, row)}
                  recovery={recoveryOf(task.id)}
                  realStatus={column.kind === "flow" && task.status !== null && task.status !== column.key ? task.status : null}
                  actions={toolCounts?.[task.id] ?? 0}
                  onOpenActions={(from) => openActions(task.id, from)}
                  usage={taskUsage}
                />
              )}
            />
          ))}
        </div>
        {focusedId !== null ? <LastReview key={focusedId} projectId={projectId} specId={spec.id} taskId={focusedId} /> : null}
      </div>
      {actionsTask !== null ? (
        <ToolActionsSheet
          projectId={projectId}
          specId={spec.id}
          task={actionsTask}
          title={actionsTask === FINAL_TASK_ID ? t("board.sheetTitle.final") : t("board.sheetTitle.task", { task: actionsTask })}
          onClose={closeActions}
        />
      ) : null}
    </div>
  );
}

/** Foco no próximo quadro (depois de o conteúdo sair do `inert`). */
function requestFocus(target: HTMLElement | null): void {
  if (target === null) return;
  const run = (): void => {
    if (target.isConnected) target.focus({ preventScroll: true });
  };
  if (typeof requestAnimationFrame === "function") requestAnimationFrame(run);
  else setTimeout(run, 0);
}

function Column({ column, narrow, renderCard }: { column: BoardColumn; narrow: boolean; renderCard: (task: TaskView, row: number) => ReactNode }) {
  const { t, tp } = useT();
  const count = column.tasks.length;
  return (
    <div role="group" aria-label={`${column.label}, ${tp("board.taskCount", count)}`} className="nd-board-column" data-column={column.key} data-kind={column.kind}>
      <h3 className="nd-board-column-head">
        <span className="nd-board-column-label" data-tone={column.tone}>
          {column.label}
        </span>
        <span aria-hidden="true" className="nd-board-column-count">
          ·
        </span>
        <span aria-hidden="true" className="nd-board-column-count nd-num" data-column-count="">
          {count}
        </span>
      </h3>
      {narrow && count === 0 ? null : (
        <div className="nd-board-scroll" data-board-scroll="" role="region" tabIndex={0} aria-label={t("board.columnTasks", { column: column.label })}>
          {count === 0 ? (
            <p className="nd-board-empty" data-column-empty="">
              {t("board.columnEmpty")}
            </p>
          ) : null}
          <ul className="nd-board-cards">{column.tasks.map((task, row) => renderCard(task, row))}</ul>
        </div>
      )}
    </div>
  );
}

interface CardProps {
  task: TaskView;
  active: boolean;
  focused: boolean;
  animate: boolean;
  register: (el: HTMLButtonElement | null) => void;
  onSelect: () => void;
  onKeyDown: (event: ReactKeyboardEvent<HTMLButtonElement>) => void;
  recovery: Recovery | undefined;
  /** Estado real quando o cartão aparece na coluna de outro estado (coluna oculta). */
  realStatus: TaskStatus | null;
  actions: number;
  onOpenActions: (from: HTMLElement) => void;
  usage: TaskUsageMap;
}

function TaskCard({ task, active, focused, animate, register, onSelect, onKeyDown, recovery, realStatus, actions, onOpenActions, usage }: CardProps) {
  const { t } = useT();
  const verdict =
    task.last_review_verdict === "APPROVED" || task.last_review_verdict === "CHANGES_REQUESTED" ? task.last_review_verdict : null;
  const owner = task.status === null ? undefined : CARD_OWNER[task.status];
  const usageLine = taskUsageLine(usage[task.id]);
  const tone = task.status !== null && task.status in STATUS_TONE ? STATUS_TONE[task.status] : "muted";
  // Metadados numa linha só, separados por "·" (094/T08, A-02): tentativa, tempo/custo e dependências.
  const meta: { key: string; node: ReactNode }[] = [];
  if (task.attempts > 0) meta.push({ key: "attempts", node: <span>{t("board.attempt", { attempt: task.attempts, max: task.max_attempts })}</span> });
  if (usageLine !== null) meta.push({ key: "usage", node: <span data-task-usage={task.id}>{usageLine}</span> });
  if (task.depends_on.length > 0) meta.push({ key: "deps", node: <span className="nd-break">{t("plan.dependsOn", { ids: task.depends_on.join(", ") })}</span> });
  return (
    <li
      className="nd-board-item"
      data-enter={animate ? "true" : undefined}
      data-active={active ? "true" : undefined}
      data-selected={focused ? "true" : undefined}
    >
      <button
        ref={register}
        type="button"
        className="nd-board-card"
        data-task-id={task.id}
        data-active={active ? "true" : undefined}
        data-tone={tone}
        aria-pressed={focused}
        onClick={onSelect}
        onKeyDown={onKeyDown}
      >
        <span className="nd-board-card-top">
          <span className="nd-mono nd-board-card-id nd-break">{task.id}</span>
          {owner !== undefined ? <AgentAvatar kind={owner} size="sm" decorative state={task.status !== null && WORKING.has(task.status) ? "thinking" : undefined} /> : null}
        </span>
        <span className="nd-board-card-title nd-break">{task.title}</span>
        {meta.length > 0 ? (
          <span className="nd-meta-line" data-card-meta="">
            {meta.map((part, i) => (
              <span key={part.key} className="nd-meta-part">
                {i > 0 ? (
                  <span aria-hidden="true" className="nd-meta-sep">
                    ·
                  </span>
                ) : null}
                {part.node}
              </span>
            ))}
          </span>
        ) : null}
        {realStatus !== null ? (
          <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-tone={STATUS_TONE[realStatus]} data-real-status={realStatus}>
            {statusLabel(realStatus)}
          </span>
        ) : null}
        {recovery !== undefined ? (
          <span className="nd-pill nd-pill-sm" data-tone="violet" data-recovery={recovery.phase}>
            <Icon name={recovery.phase === "analyzing" ? "sparkles" : "refresh"} size={14} />
            {recoveryLabel(recovery)}
          </span>
        ) : null}
        {verdict !== null ? (
          <span className="nd-pill nd-pill-sm nd-pill-nowrap" data-tone={verdict === "APPROVED" ? "green" : "amber"} data-verdict={verdict} title={TASK_VERDICT_LABEL[verdict]}>
            <Icon name={verdict === "APPROVED" ? "check" : "alert"} size={14} />
            <span className="nd-sr-only">{t("board.reviewPrefix")}</span>
            {CARD_VERDICT_LABEL[verdict]}
          </span>
        ) : null}
        {task.status === "BLOCKED" && task.next_action ? (
          <span className="nd-board-reason">
            <span className="nd-section-label" data-tone="rose">
              {t("board.reason")}
            </span>
            <span className="nd-prewrap nd-break">{task.next_action}</span>
          </span>
        ) : null}
        {task.error !== undefined ? <span className="nd-prewrap nd-muted nd-break">{task.error}</span> : null}
      </button>
      {actions > 0 ? (
        <span className="nd-board-card-foot" data-card-foot="">
          <button
            type="button"
            className="nd-link-btn"
            data-tool-actions={task.id}
            aria-haspopup="dialog"
            aria-label={t("board.taskActionsLabel", { actions: toolActionsLabel(actions), task: task.id })}
            onClick={(event) => onOpenActions(event.currentTarget)}
          >
            {toolActionsLabel(actions)}
          </button>
        </span>
      ) : null}
    </li>
  );
}

/** "Colunas": as opcionais em caixas de seleção; o estado vai para o ui-state. */
function ColumnsMenu({ shown, forcedBlocked, onToggle }: { shown: ReadonlySet<TaskStatus>; forcedBlocked: boolean; onToggle: (status: TaskStatus) => void }) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const popoverId = useId();

  useEffect(() => {
    if (!open) return undefined;
    rootRef.current?.querySelector<HTMLInputElement>('input[type="checkbox"]:not(:disabled)')?.focus();
    const onDown = (event: PointerEvent): void => {
      if (event.target instanceof Node && rootRef.current?.contains(event.target) !== true) setOpen(false);
    };
    document.addEventListener("pointerdown", onDown);
    return () => document.removeEventListener("pointerdown", onDown);
  }, [open]);

  return (
    <div
      ref={rootRef}
      className="nd-board-columns-menu"
      onKeyDown={(event) => {
        if (open && event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          setOpen(false);
          buttonRef.current?.focus();
        }
      }}
      onBlur={(event) => {
        const next = event.relatedTarget;
        if (open && next instanceof Node && rootRef.current?.contains(next) !== true) setOpen(false);
      }}
    >
      <button
        ref={buttonRef}
        type="button"
        className="nd-btn nd-btn-sm"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls={open ? popoverId : undefined}
        onClick={() => setOpen((v) => !v)}
      >
        <Icon name="layout" size={14} />
        {t("board.columns")}
      </button>
      {open ? (
        <div id={popoverId} role="dialog" aria-label={t("board.columnsDialog")} className="nd-board-popover nd-overlay">
          {OPTIONAL_COLUMNS.map((status) => {
            const forced = status === "BLOCKED" && forcedBlocked;
            return (
              <label key={status} className="nd-board-check">
                <input type="checkbox" checked={forced || shown.has(status)} disabled={forced} onChange={() => onToggle(status)} />
                <span>{statusLabel(status)}</span>
                {forced ? <span className="nd-muted">{t("board.hasBlocked")}</span> : null}
              </label>
            );
          })}
        </div>
      ) : null}
    </div>
  );
}

/**
 * Última revisão da tarefa em foco: UMA linha recolhível ("Última revisão · T02 · Pediu ajustes",
 * spec 094 T05) que, aberta, mostra o veredito, apontamentos e observações (com o rótulo da
 * observação rebaixada da 092) e a matriz de critérios (`ReviewerChecklist`), em no máximo 40% da
 * altura do Quadro, com rolagem própria. Recolhida por padrão: as colunas mantêm a altura útil.
 * Assina só as mensagens do projeto (nunca as atividades): a rajada de atividade não chega aqui.
 */
function LastReview({ projectId, specId, taskId }: { projectId: string; specId: string; taskId: string }) {
  const { t } = useT();
  const messages = useAppStore((s) => s.messages[projectId]);
  const message = useMemo(() => lastReviewMessage(messages, specId, taskId), [messages, specId, taskId]);
  const review = useMemo(() => parsedReview(message), [message]);
  const [open, setOpen] = useState(false);
  const bodyId = useId();
  if (review === null) return null;
  const line = lastReviewLine(taskId, review.verdict);
  return (
    <section className="nd-board-review" aria-label={t("board.lastReview", { task: taskId })} data-last-review={taskId} data-open={open ? "true" : "false"}>
      <button
        type="button"
        className="nd-disclosure-btn nd-board-review-toggle"
        aria-expanded={open}
        aria-controls={open ? bodyId : undefined}
        onClick={() => setOpen((v) => !v)}
      >
        <Icon name={open ? "chevron-down" : "chevron-right"} size={14} />
        <span className="nd-board-review-line" data-tone={review.verdict === null ? undefined : review.verdict === "approved" ? "green" : "amber"}>
          {line}
        </span>
      </button>
      {open ? (
        <div id={bodyId} className="nd-board-review-body" data-review-body="">
          {review.verdict !== null ? <ReviewVerdictBadge verdict={review.verdict} /> : null}
          {review.summary !== "" ? <MarkdownText text={review.summary} keepLineBreaks /> : null}
          <ReviewFindingList findings={review.findings} kind="blocking" />
          <ReviewFindingList findings={review.observations} kind="observation" />
          {review.items.length > 0 ? <ReviewerChecklist items={review.items} /> : null}
        </div>
      ) : null}
    </section>
  );
}
