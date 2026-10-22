/*
 * Ações de uma tarefa como FOLHA dentro da janela Quadro (spec 086/T05): a trilha `tools.jsonl` em ordem cronológica
 * (hora local HH:MM:SS, papel e rótulo), paginada para trás por "Carregar anteriores".
 * `task = "FINAL"` mostra a verificação final. A folha cobre só o conteúdo da janela (o resto
 * do quadro fica `inert` enquanto ela está aberta); Esc e "Fechar" devolvem o foco a quem abriu.
 * Só texto React.
 */
import { useCallback, useEffect, useId, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { getToolActions } from "../../api/client";
import type { ToolActionItem } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import { formatTime } from "../../i18n";
import { useT } from "../../i18n/react";
import { toolActionText } from "../shared/activityText";
import { agentLabel, agentTone } from "../shared/agents";

/** Itens por página pedidos ao servidor. */
const TOOL_ACTIONS_PAGE = 100;

/** Hora local HH:MM:SS (24 h) de `ts` (segundos Unix), pelo formatador do idioma ativo. */
export function formatClock(ts: number): string {
  return formatTime(ts * 1000, { hour: "2-digit", minute: "2-digit", second: "2-digit", hourCycle: "h23" });
}

type Load = { phase: "loading" } | { phase: "error"; message: string } | { phase: "ready" };

interface ToolActionsSheetProps {
  projectId: string;
  specId: string;
  /** Id da tarefa (ou `FINAL`). */
  task: string;
  title: string;
  onClose: () => void;
}

export function ToolActionsSheet({ projectId, specId, task, title, onClose }: ToolActionsSheetProps) {
  const titleId = useId();
  const { t, tp } = useT();
  const [items, setItems] = useState<ToolActionItem[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [total, setTotal] = useState(0);
  const [load, setLoad] = useState<Load>({ phase: "loading" });
  const [older, setOlder] = useState(false);
  const alive = useRef(true);
  const listRef = useRef<HTMLOListElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    alive.current = true;
    closeRef.current?.focus({ preventScroll: true });
    return () => {
      alive.current = false;
    };
  }, []);

  const loadLatest = useCallback(() => {
    setLoad({ phase: "loading" });
    void getToolActions(projectId, specId, { task, limit: TOOL_ACTIONS_PAGE }).then((r) => {
      if (!alive.current) return;
      if (!r.ok) {
        setLoad({ phase: "error", message: r.error.message });
        return;
      }
      setItems(r.data.items);
      setHasMore(r.data.has_more);
      setTotal(r.data.total);
      setLoad({ phase: "ready" });
    });
  }, [projectId, specId, task]);

  useEffect(() => {
    loadLatest();
  }, [loadLatest]);

  function loadOlder(): void {
    const first = items[0];
    if (first === undefined || older) return;
    setOlder(true);
    void getToolActions(projectId, specId, { task, before: first.seq, limit: TOOL_ACTIONS_PAGE }).then((r) => {
      if (!alive.current) return;
      setOlder(false);
      if (!r.ok) {
        setLoad({ phase: "error", message: r.error.message });
        return;
      }
      setItems((current) => {
        const seen = new Set(current.map((i) => i.seq));
        return [...r.data.items.filter((i) => !seen.has(i.seq)), ...current];
      });
      setHasMore(r.data.has_more);
      setTotal(r.data.total);
      setLoad({ phase: "ready" });
      // O botão some quando não há mais anteriores: o foco vai para a lista, não para o body.
      if (!r.data.has_more) listRef.current?.focus();
    });
  }

  const onKeyDown = (event: ReactKeyboardEvent<HTMLElement>): void => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    onClose();
  };

  return (
    <section role="dialog" aria-modal="false" aria-labelledby={titleId} className="nd-actions-sheet" data-tool-actions-sheet={task} onKeyDown={onKeyDown}>
      <header className="nd-actions-sheet-head">
        <h3 id={titleId} className="nd-actions-sheet-title nd-break">
          {title}
        </h3>
        <button ref={closeRef} type="button" className="nd-wbtn" aria-label={t("common.close")} onClick={onClose}>
          <Icon name="close" size={14} />
        </button>
      </header>
      {load.phase === "ready" ? (
        <p className="nd-muted">{tp("board.actions.count", total, { shown: items.length })}</p>
      ) : null}
      <div className="nd-actions-sheet-scroll" data-testid="tool-actions-scroll">
        {hasMore ? (
          <button type="button" className="nd-btn nd-btn-sm" onClick={loadOlder} disabled={older}>
            <Icon name="arrow-down" size={14} />
            {older ? t("board.actions.loadingOlder") : t("board.actions.loadOlder")}
          </button>
        ) : null}
        {load.phase === "loading" ? (
          <p role="status" className="nd-muted">
            {t("board.actions.loading")}
          </p>
        ) : null}
        {load.phase === "error" ? (
          <div role="alert" className="nd-row">
            <span className="nd-error-inline">{load.message}</span>
            <button type="button" className="nd-btn nd-btn-sm" onClick={loadLatest}>
              <Icon name="refresh" size={14} />
              {t("common.tryAgain")}
            </button>
          </div>
        ) : null}
        {load.phase === "ready" && items.length === 0 ? <p className="nd-muted">{t("board.actions.none")}</p> : null}
        {items.length > 0 ? (
          <ol ref={listRef} tabIndex={-1} aria-label={t("board.actions.listLabel")} className="nd-trail">
            {items.map((item) => (
              <li key={item.seq} data-seq={item.seq} className="nd-trail-item">
                <time dateTime={new Date(item.ts * 1000).toISOString()} className="nd-muted">
                  {formatClock(item.ts)}
                </time>
                <span className="nd-agent-name" data-tone={agentTone(item.agent)}>
                  {agentLabel(item.agent)}
                </span>
                <span data-kind={item.kind} className={`nd-trail-text nd-break${item.kind === "command" ? " nd-mono" : ""}`}>
                  {toolActionText(item)}
                </span>
              </li>
            ))}
          </ol>
        ) : null}
      </div>
    </section>
  );
}
