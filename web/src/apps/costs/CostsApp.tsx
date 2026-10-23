/*
 * Janela Custos (`DEFAULT_APPS.costs.Content`, em `memo`), enxuta (095/T05, decisão 7):
 * - em destaque: o total do projeto em 32 px (com "inclui N imagens (estimado)", da 092), o
 *   orçamento da spec ativa ("US$ 0,10 de US$ 5,00" e a barra) e os três maiores itens (papéis e
 *   imagens);
 * - o efeito da gestão de contexto (spec 122), quando houver: compactações, resumos e os tokens
 *   evitados (estimativa), do projeto e da spec ativa, em uma linha cada;
 * - "Ver detalhes por tarefa e modelo" (`<details>` recolhido): tokens e chamadas, barras por papel
 *   e por tarefa, o ritmo do provedor, a tabela por categoria com TOTAL, as chamadas sem preço com
 *   "Configurar", as buscas na web, as imagens, por agente e por modelo;
 * - orçamento da spec (`<details>` recolhido): o formulário do limite, editável.
 *
 * Responsividade por `data-size`: estreita, as tabelas viram listas empilhadas. Falha de carga
 * não mostra números (informativo). A atividade ao vivo só chega aqui pelo aviso do limitador
 * (um texto): a rajada de eventos não re-renderiza a janela.
 */
import { memo, useId, useState, type FormEvent } from "react";
import { putBudget } from "../../api/client";
import type { NamedUsage, PlanUsageTotals, SpecBudget, UsageTotals } from "../../api/types";
import { imagesCostHint, imagesCostLine, webSearchCostLine } from "../../copy";
import { EmptyState } from "../../desktop/EmptyState";
import { Icon } from "../../desktop/Icon";
import { useShell, useSpace } from "../../desktop/shellContext";
import { useWindowVisibility } from "../../desktop/useWindowVisibility";
import type { MessageKey } from "../../i18n";
import { apiFailureText } from "../../i18n/serverCodes";
import { useT } from "../../i18n/react";
import { useAppStore } from "../../store";
import { formatTokens, formatUsd } from "../../usageFormat";
import type { AppContentProps } from "../registry";
import { agentLabel } from "../shared/agents";
import { useUsageView } from "../shared/useUsage";
import "../shared/apps.css";
import "./costs.css";
import { PLAN_USAGE_URL } from "../settings/chatgptPlan";
import {
  budgetProgress,
  contextEffectText,
  costBars,
  costText,
  hasApiConsumption,
  hasConsumption,
  pacingNotice,
  pacingText,
  tokensText,
  tokensValue,
  topCostItems,
  totalCostText,
  type BudgetProgress,
  type CostBar,
} from "./costsModel";

function CostsContentView({ size }: AppContentProps) {
  const space = useSpace();
  const shell = useShell();
  // Assina o idioma: o conteúdo (em `memo`) re-renderiza na troca.
  const { t, tp } = useT();
  const projectId = space?.data.projectId ?? "";
  const spec = space?.data.activeSpec ?? null;
  const visible = useWindowVisibility("costs");
  const project = useUsageView(projectId, null, visible && projectId !== "");
  const specView = useUsageView(projectId, spec?.id ?? null, visible && projectId !== "" && spec !== null);
  const pacing = useAppStore((s) => pacingNotice(s.activities[projectId]));
  // Limite salvo nesta sessão (por spec): a barra acompanha sem esperar a releitura do uso.
  const [saved, setSaved] = useState<{ spec: string; budget: SpecBudget } | null>(null);
  if (space === null) return null;
  if (project === null) {
    return <EmptyState icon="costs" message={t("windowEmpty.costs")} />;
  }
  const totals = project.total;
  // Sem consumo: estado vazio próprio, nenhum número zerado em destaque.
  if (!hasConsumption(totals)) {
    return <EmptyState icon="costs" message={t("costs.noConsumption")} />;
  }
  // Só pelo plano ChatGPT: sem total em dólar (nada foi cobrado por chave), só a seção do plano.
  const apiUsage = hasApiConsumption(totals);
  const unpriced = totals.cost_usd === null;
  const imageCount = totals.images ?? 0;
  const imageEstimated = totals.images_estimated === true;
  const specs = space.data.state?.specs ?? [];
  const categoryLabel = (id: string): string => (id === "chat" ? t("costs.chatCategory") : (specs.find((s) => s.id === id)?.title ?? id));
  const specUsage = spec === null ? undefined : specView?.specs.find((s) => s.spec === spec.id);
  const taskTitle = (id: string): string => {
    const task = spec?.tasks.find((t) => t.id === id);
    return task === undefined ? id : `${id} ${task.title}`;
  };
  const budget = spec === null || specView === null ? null : saved !== null && saved.spec === spec.id ? saved.budget : specView.budget;
  const progress = spec === null || specView === null ? null : budgetProgress(specView.total, budget);
  const hasLimit = budget !== null && (budget.max_tokens !== undefined || budget.max_usd !== undefined);
  const top = topCostItems(totals, project.by_agent ?? [], agentLabel);
  const narrow = size === "narrow";
  // Efeito da gestão de contexto (spec 122): a linha do projeto e, com spec ativa, a da spec.
  const contextLines = [contextEffectText(project.context), spec === null ? null : contextEffectText(specView?.context, spec.id)].filter(
    (line): line is string => line !== null,
  );
  const openPricing = (): void => shell.openSettings("pricing");
  return (
    <div className="nd-app-scroll nd-app-page nd-costs" data-costs="" data-costs-layout={narrow ? "stacked" : "table"}>
      {apiUsage ? (
        <section className="nd-costs-total" aria-label={t("costs.projectCost")}>
          {unpriced ? (
            <>
              {/* Sem preço, o valor não existe: o total diz isso em texto, sem traço no lugar do número (095/T08). */}
              <p className="nd-costs-amount" data-testid="usage-cost" data-unpriced="">
                {t("costs.unpricedTotal")}
              </p>
              <p className="nd-costs-unpriced" role="note" data-unpriced-note="">
                <span>{t("costs.unpricedNote")}</span>
                <button type="button" className="nd-btn nd-btn-sm" onClick={openPricing}>
                  <Icon name="settings" size={14} />
                  {t("readiness.configure")}
                </button>
              </p>
            </>
          ) : (
            <p className="nd-costs-amount" data-testid="usage-cost">
              {totalCostText(totals)}
              {totals.cost_estimated === true ? <span className="nd-costs-amount-note">{` ${t("costs.estimated")}`}</span> : null}
            </p>
          )}
          {imageCount > 0 ? (
            <p className="nd-muted" data-testid="images-hint">
              {imagesCostHint(imageCount, imageEstimated)}
            </p>
          ) : null}
          {progress !== null && spec !== null ? <BudgetBar specId={spec.id} progress={progress} /> : null}
        </section>
      ) : null}

      {totals.chatgpt_plan !== undefined && totals.chatgpt_plan.calls > 0 ? <PlanUsage plan={totals.chatgpt_plan} /> : null}

      {apiUsage && top.length > 0 ? (
        <section className="nd-costs-top" aria-label={t("costs.top")} data-costs-top="">
          <ul className="nd-costs-top-list">
            {top.map((item) => (
              <li key={item.id} className="nd-costs-top-item" data-top-item={item.id}>
                <span className="nd-costs-top-label nd-break">{item.label}</span>
                <span className="nd-costs-top-value nd-num">{item.value}</span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {contextLines.length > 0 ? (
        <section className="nd-costs-context" aria-label={t("costs.context.label")} data-costs-context="">
          {contextLines.map((line) => (
            <p key={line} className="nd-muted nd-num">
              {line}
            </p>
          ))}
        </section>
      ) : null}

      {apiUsage ? (
        <details className="nd-costs-more" data-costs-details="">
          <summary className="nd-costs-more-summary">
            <Icon name="chevron-right" size={14} className="nd-details-chevron" />
            {t("costs.details")}
          </summary>
          <div className="nd-costs-more-body">
            <p className="nd-muted nd-num" data-costs-tokens="">{`${tokensText(totals)} · ${tp("costs.calls", totals.calls)}`}</p>
            <Bars title={t("costs.byRole")} bars={costBars(project.by_agent ?? [], agentLabel)} />
            {specUsage !== undefined && specUsage.tasks.length > 0 ? (
              <Bars title={t("costs.byTask", { spec: spec?.id ?? "" })} bars={costBars(specUsage.tasks, taskTitle)} />
            ) : null}
            <p className="nd-costs-pacing" data-pacing={pacing === null ? "idle" : "waiting"}>
              <Icon name={pacing === null ? "zap" : "refresh"} size={14} />
              <span>{pacingText(pacing)}</span>
            </p>
            <UsageTable title={t("costs.byCategory")} firstCol={t("costs.category")} rows={project.by_spec ?? []} total={totals} label={categoryLabel} narrow={narrow} />
            {totals.unpriced_calls > 0 ? (
              <p role="note" className="nd-costs-note" data-unpriced-calls="">
                <span>{tp("costs.unpricedCalls", totals.unpriced_calls)}</span>
                <button type="button" className="nd-btn nd-btn-sm" onClick={openPricing}>
                  <Icon name="settings" size={14} />
                  {t("readiness.configure")}
                </button>
              </p>
            ) : null}
            {(totals.web_search_calls ?? 0) > 0 ? (
              <p data-testid="web-search-cost" className="nd-muted nd-num">
                {webSearchCostLine(totals.web_search_calls ?? 0, formatUsd(totals.web_search_usd ?? 0))}
              </p>
            ) : null}
            {imageCount > 0 ? (
              <p data-testid="images-cost" className="nd-muted nd-num">
                {imagesCostLine(imageCount, formatUsd(totals.images_usd ?? 0), imageEstimated)}
              </p>
            ) : null}
            <UsageTable title={t("costs.byAgent")} firstCol={t("costs.agent")} rows={project.by_agent ?? []} label={agentLabel} narrow={narrow} />
            <UsageTable title={t("costs.byModel")} firstCol={t("costs.model")} rows={project.by_model ?? []} narrow={narrow} note={t("costs.modelNote")} />
          </div>
        </details>
      ) : null}

      {spec !== null && specView !== null ? (
        <details className="nd-costs-more" data-costs-budget="">
          <summary className="nd-costs-more-summary">
            <Icon name="chevron-right" size={14} className="nd-details-chevron" />
            {hasLimit ? t("costs.budget.edit") : t("costs.budget.define")}
          </summary>
          <div className="nd-costs-more-body">
            <BudgetForm key={spec.id} projectId={projectId} specId={spec.id} budget={specView.budget} onSaved={(b) => setSaved({ spec: spec.id, budget: b })} />
          </div>
        </details>
      ) : null}
    </div>
  );
}

/**
 * Orçamento da spec ativa em destaque: "Spec s1: US$ 0,10 de US$ 5,00" e a barra de progresso
 * (rosa ao passar do limite).
 */
function BudgetBar({ specId, progress }: { specId: string; progress: BudgetProgress }) {
  const { t } = useT();
  return (
    <div className="nd-costs-budget-bar" data-budget-bar="" data-over={progress.over ? "true" : "false"}>
      <p className="nd-costs-budget-line nd-num">
        <span className="nd-muted">{t("costs.budget.spec", { spec: specId })}</span>
        <span className="nd-costs-budget-used">{progress.used}</span> <span className="nd-muted">{progress.limit}</span>
      </p>
      <span
        role="progressbar"
        aria-label={t("costs.budget.barLabel", { spec: specId })}
        aria-valuetext={`${progress.used} ${progress.limit}`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={progress.pct}
        className="nd-costs-budget-track"
      >
        <span className="nd-costs-budget-fill" style={{ width: `${progress.pct}%` }} />
      </span>
    </div>
  );
}

export const CostsContent = memo(CostsContentView);

/**
 * Uso pelo plano ChatGPT (spec 116 T03): os tokens e as chamadas, sem US$ (o uso sai do limite do
 * plano), com o link "Gerenciar uso".
 */
function PlanUsage({ plan }: { plan: PlanUsageTotals }) {
  const { t, tp } = useT();
  const headingId = useId();
  return (
    <section aria-labelledby={headingId} className="nd-costs-plan nd-stack-xs" data-costs-plan="">
      <h3 id={headingId} className="nd-section-label">
        {t("costs.plan.title")}
      </h3>
      <p className="nd-costs-plan-tokens nd-num" data-plan-tokens="">{`${tokensText(plan)} · ${tp("costs.calls", plan.calls)}`}</p>
      <p className="nd-muted">{t("costs.plan.note")}</p>
      <a className="nd-btn nd-btn-sm nd-self-start" data-variant="ghost" href={PLAN_USAGE_URL} target="_blank" rel="noopener noreferrer" data-plan-usage="">
        <Icon name="external" size={14} />
        {t("chatgpt.manageUsage")}
      </a>
    </section>
  );
}

function Bars({ title, bars }: { title: string; bars: readonly CostBar[] }) {
  const headingId = useId();
  if (bars.length === 0) return null;
  return (
    <section aria-labelledby={headingId} className="nd-cost-bars">
      <h3 id={headingId} className="nd-section-label">
        {title}
      </h3>
      <ul className="nd-cost-bar-list">
        {bars.map((bar) => (
          <li key={bar.id} className="nd-cost-bar" data-bar={bar.id}>
            <span className="nd-cost-bar-label nd-break">{bar.label}</span>
            <span className="nd-cost-bar-track" aria-hidden="true">
              <span className="nd-cost-bar-fill" style={{ width: `${bar.pct}%` }} />
            </span>
            <span className="nd-cost-bar-value nd-num">{bar.value}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

interface TableProps {
  title: string;
  firstCol: string;
  rows: readonly NamedUsage[];
  total?: UsageTotals;
  label?: (id: string) => string;
  note?: string;
  narrow: boolean;
}

function UsageTable({ title, firstCol, rows, total, label, note, narrow }: TableProps) {
  const { t } = useT();
  if (rows.length === 0) return null;
  const name = (row: NamedUsage): string => label?.(row.id) ?? row.id;
  const body = narrow ? (
    <ul data-stacked="" className="nd-usage-stack">
      {rows.map((row) => (
        <li key={row.id} className="nd-usage-stack-item">
          <span className="nd-break">{name(row)}</span>
          <span className="nd-row nd-num">
            <span className="nd-muted">{tokensText(row)}</span>
            <span className="nd-strong">{costText(row)}</span>
          </span>
          {row.unpriced_calls > 0 ? <span className="nd-muted">{t("costs.unpricedCount", { count: row.unpriced_calls })}</span> : null}
        </li>
      ))}
      {total !== undefined ? (
        <li data-total="" className="nd-usage-stack-item" data-variant="total">
          <span className="nd-strong">{t("costs.total")}</span>
          <span className="nd-row nd-num">
            <span className="nd-muted">{tokensText(total)}</span>
            <span className="nd-strong">{costText(total)}</span>
          </span>
        </li>
      ) : null}
    </ul>
  ) : (
    <div className="nd-usage-table-wrap">
      <table className="nd-usage-table">
        <thead>
          <tr>
            <th scope="col">{firstCol}</th>
            <th scope="col">{t("costs.input")}</th>
            <th scope="col">{t("costs.output")}</th>
            <th scope="col">{t("costs.cost")}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row.id}>
              <th scope="row">
                <span className="nd-break">{name(row)}</span>
                {row.unpriced_calls > 0 ? <span className="nd-muted nd-usage-unpriced">{t("costs.unpricedCount", { count: row.unpriced_calls })}</span> : null}
              </th>
              <td className="nd-num">{formatTokens(row.input)}</td>
              <td className="nd-num">{formatTokens(row.output)}</td>
              <td className="nd-num">{costText(row)}</td>
            </tr>
          ))}
        </tbody>
        {total !== undefined ? (
          <tfoot>
            <tr data-total="">
              <th scope="row">{t("costs.total")}</th>
              <td className="nd-num">{formatTokens(total.input)}</td>
              <td className="nd-num">{formatTokens(total.output)}</td>
              <td className="nd-num">{costText(total)}</td>
            </tr>
          </tfoot>
        ) : null}
      </table>
    </div>
  );
  const content = (
    <>
      {body}
      {note !== undefined ? <p className="nd-muted">{note}</p> : null}
    </>
  );
  return (
    <section aria-label={title} className="nd-stack-sm">
      <h3 className="nd-section-label">{title}</h3>
      {content}
    </section>
  );
}

function BudgetForm({ projectId, specId, budget, onSaved }: { projectId: string; specId: string; budget: SpecBudget | null; onSaved: (budget: SpecBudget) => void }) {
  const tokensId = useId();
  const usdId = useId();
  const { t } = useT();
  const [tokens, setTokens] = useState(() => (budget?.max_tokens !== undefined ? String(budget.max_tokens) : ""));
  const [usd, setUsd] = useState(() => (budget?.max_usd !== undefined ? String(budget.max_usd) : ""));
  // Aviso do formulário: chave do dicionário (segue a troca de idioma) ou o texto do servidor.
  const [message, setMessage] = useState<{ key: MessageKey } | { text: string } | null>(null);
  const [saved, setSaved] = useState<SpecBudget | null>(null);
  const current = saved ?? budget;

  async function save(event: FormEvent): Promise<void> {
    event.preventDefault();
    const body: SpecBudget = {};
    if (tokens.trim() !== "") {
      const n = Number(tokens);
      if (!Number.isInteger(n) || n < 0) return setMessage({ key: "costs.budget.invalidTokens" });
      body.max_tokens = n;
    }
    if (usd.trim() !== "") {
      const v = Number(usd.replace(",", "."));
      if (!Number.isFinite(v) || v < 0) return setMessage({ key: "costs.budget.invalidUsd" });
      body.max_usd = v;
    }
    const r = await putBudget(projectId, specId, body);
    if (!r.ok) return setMessage({ text: apiFailureText(r.error) });
    setSaved(body);
    onSaved(body);
    setMessage({ key: Object.keys(body).length === 0 ? "costs.budget.removed" : "costs.budget.saved" });
  }

  return (
    <form className="nd-costs-budget nd-stack-sm" onSubmit={(event) => void save(event)} aria-label={t("costs.budget.form")}>
      <p className="nd-muted">{t("costs.budget.emptyHint")}</p>
      <div className="nd-row nd-costs-budget-fields">
        <div className="nd-field">
          <label className="nd-field-label" htmlFor={tokensId}>
            {t("costs.budget.tokensLabel")}
          </label>
          <input id={tokensId} className="nd-input nd-num" inputMode="numeric" value={tokens} onChange={(e) => setTokens(e.target.value)} />
        </div>
        <div className="nd-field">
          <label className="nd-field-label" htmlFor={usdId}>
            {t("costs.budget.usdLabel")}
          </label>
          <input id={usdId} className="nd-input nd-num" inputMode="decimal" value={usd} onChange={(e) => setUsd(e.target.value)} />
        </div>
        <button type="submit" className="nd-btn">
          <Icon name="save" size={14} />
          {t("costs.budget.save")}
        </button>
      </div>
      {current !== null ? (
        <p className="nd-muted">
          {t("costs.budget.current", {
            tokens: current.max_tokens !== undefined ? tokensValue(current.max_tokens) : t("costs.budget.noTokens"),
            usd: current.max_usd !== undefined ? formatUsd(current.max_usd) : t("costs.budget.noUsd"),
          })}
        </p>
      ) : null}
      {message !== null ? (
        <p role="status" className="nd-muted">
          {"key" in message ? t(message.key) : message.text}
        </p>
      ) : null}
    </form>
  );
}
