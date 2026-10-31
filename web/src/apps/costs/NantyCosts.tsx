/*
 * O gasto do Nanty na janela Custos (spec 138 T10, C-17): uma seção à parte, fora do total do
 * projeto. Hoje, em US$ quando o modelo tem preço e sempre em tokens; as chamadas sem preço, as de
 * voz (a fala e a transcrição, já somadas ao total), o orçamento do dia e os dias anteriores.
 * Lido de `GET /api/nanty/usage`, relido pelo `nanty_changed` (o gasto mora no estado do feed).
 */
import { useId } from "react";
import { formatDate, formatNumber, formatUsd } from "../../i18n";
import { useT } from "../../i18n/react";
import { Icon } from "../../desktop/Icon";
import type { NantySpend, NantyUsage } from "../nanty/nantyApi";

/** Dias anteriores mostrados. */
const HISTORY_DAYS = 7;

/** O Nanty gastou algo (hoje ou antes)? */
export function hasNantySpend(usage: NantyUsage | null): usage is NantyUsage {
  if (usage === null) return false;
  const spent = (s: NantySpend): boolean => s.usd > 0 || s.input_tokens + s.output_tokens > 0 || s.voice_calls > 0;
  return spent(usage.today) || usage.history.some(spent);
}

function spendText(s: NantySpend, tokensLabel: (tokens: string) => string): string {
  const tokens = tokensLabel(formatNumber(s.input_tokens + s.output_tokens));
  return s.usd > 0 ? `${formatUsd(s.usd)} · ${tokens}` : tokens;
}

/** `AAAA-MM-DD` como data local (sem o deslocamento do fuso que o `Date` daria ao texto). */
function dayLabel(day: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day);
  if (m === null) return day;
  return formatDate(new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])));
}

export function NantyCosts({ usage }: { usage: NantyUsage }) {
  const { t, tp } = useT();
  const headingId = useId();
  const tokensLabel = (tokens: string): string => t("costs.nanty.tokens", { tokens });
  const { today, budget } = usage;
  const history = usage.history.slice(0, HISTORY_DAYS);
  return (
    <section aria-labelledby={headingId} className="nd-costs-nanty nd-stack-xs" data-costs-nanty="">
      <h3 id={headingId} className="nd-section-label">
        {t("costs.nanty.title")}
      </h3>
      <p className="nd-muted">{t("costs.nanty.note")}</p>
      <p className="nd-costs-nanty-today nd-num" data-nanty-today="">
        <span className="nd-muted">{t("costs.nanty.today")}</span> <span className="nd-strong">{spendText(today, tokensLabel)}</span>
      </p>
      {budget !== null ? (
        <p className="nd-muted nd-num" data-nanty-budget="">
          {budget.mode === "usd" ? t("costs.nanty.budgetUsd", { budget: formatUsd(budget.budget_usd) }) : t("costs.nanty.budgetTokens", { budget: formatNumber(budget.budget_tokens) })}
        </p>
      ) : null}
      {budget?.exhausted === true ? <p className="nd-muted">{t("costs.nanty.exhausted")}</p> : null}
      {today.unpriced_turns > 0 ? (
        <p className="nd-muted nd-num" data-nanty-unpriced="">
          {tp("costs.nanty.unpriced", today.unpriced_turns)}
        </p>
      ) : null}
      {today.voice_calls > 0 ? (
        <p className="nd-muted nd-num" data-nanty-voice="">
          {tp("costs.nanty.voice", today.voice_calls)}
        </p>
      ) : null}
      {history.length > 0 ? (
        <details className="nd-costs-more" data-nanty-history="">
          <summary className="nd-costs-more-summary">
            <Icon name="chevron-right" size={14} className="nd-details-chevron" />
            {t("costs.nanty.history")}
          </summary>
          <ul className="nd-usage-stack nd-costs-more-body">
            {history.map((day) => (
              <li key={day.day} className="nd-usage-stack-item">
                <span>{dayLabel(day.day)}</span>
                <span className="nd-num">{spendText(day, tokensLabel)}</span>
              </li>
            ))}
          </ul>
        </details>
      ) : null}
    </section>
  );
}
