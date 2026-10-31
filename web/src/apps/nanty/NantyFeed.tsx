/*
 * Feed do Nanty (spec 138 T10, `reference/Main.dc.html`): as seções Esperando você, Feito por conta
 * própria, Previsto e Hoje.
 *
 * - **Cartão:** o título do tipo de aviso, a hora, os fatos (C-18: só contagens e códigos, nunca
 *   texto de projeto), a linha do porquê (o texto do observador, C-06) e os mesmos botões do
 *   Telegram (`cardButtons`). Decidir aqui fecha o cartão no Telegram, e vice-versa: o servidor
 *   avisa pelo `nanty_changed` e o feed se relê sozinho.
 * - **Resultado:** o que a decisão disse (`nanty.outcome.*`) ou o erro da rota (cartão vencido,
 *   token trocado), pelo dicionário, numa linha de estado no topo; a janela nunca quebra.
 * - **Desfazer:** no "feito" que ainda tem o desfazer válido.
 */
import { useEffect, useId, useState, type ReactNode } from "react";
import { formatTime } from "../../i18n";
import { useT } from "../../i18n/react";
import { Icon } from "../../desktop/Icon";
import { useAppStore } from "../../store";
import { agentLabel } from "../shared/agents";
import { getNantyDigestTime, type NantyCard, type NantyDone, type NantyFeed, type NantyPlanned, type NantyStatus } from "./nantyApi";
import { canUndo, cardButtons, doneWho, factTexts, feedSections, type CardChoice } from "./nantyModel";
import { clockText, nantyCodeText, nantyTitle } from "./nantyText";
import type { NantyDecisions, NoticeOrigin } from "./useNantyDecisions";

/** Nome do projeto pelo id (a lista do Nants); sem cadastro, nada. */
function useProjectName(): (id: string | null) => string | null {
  const projects = useAppStore((s) => s.config?.projects);
  return (id) => (id === null ? null : (projects?.find((p) => p.id === id)?.name ?? null));
}

/** A linha dos fatos: o projeto e os fatos do cartão, separados por ponto. */
function FactsLine({ project, facts }: { project: string | null; facts: readonly string[] }) {
  const parts = project === null ? facts : [project, ...facts];
  if (parts.length === 0) return null;
  return (
    <p className="nd-nanty-facts" data-nanty-facts="">
      {parts.join(" · ")}
    </p>
  );
}

function Section({ id, label, count, children }: { id: string; label: string; count?: number; children: ReactNode }) {
  const headingId = useId();
  return (
    <section className="nd-nanty-section" aria-labelledby={headingId} data-nanty-section={id}>
      <h3 id={headingId} className="nd-nanty-section-label">
        {count === undefined ? label : `${label} · ${String(count)}`}
      </h3>
      {children}
    </section>
  );
}

/** Os botões de um cartão (no feed e na conversa); só o primeiro cartão do feed leva a primária. */
export function CardActions({ card, decisions, primary, origin = "feed" }: { card: NantyCard; decisions: NantyDecisions; primary: boolean; origin?: NoticeOrigin }) {
  const buttons = cardButtons(card);
  if (buttons.length === 0) return null;
  const busy = decisions.pending.has(card.id);
  const onClick = (choice: CardChoice): void => void decisions.decide(card, choice, origin);
  return (
    <div className="nd-nanty-actions" data-nanty-actions="">
      {buttons.map((b, i) => (
        <button
          key={b.choice}
          type="button"
          className="nd-btn"
          data-variant={primary && i === 0 && b.choice === "yes" ? "primary" : undefined}
          data-nanty-choice={b.choice}
          disabled={busy}
          onClick={() => onClick(b.choice)}
        >
          {b.label}
        </button>
      ))}
    </div>
  );
}

function WaitingCard({ card, decisions, primary, projectName }: { card: NantyCard; decisions: NantyDecisions; primary: boolean; projectName: string | null }) {
  const why = nantyCodeText(card.code, card.params ?? {});
  return (
    <article className="nd-nanty-card" data-nanty-card={card.id} data-variant="waiting">
      <div className="nd-nanty-card-head">
        <span className="nd-dot" data-tone="amber" aria-hidden="true" />
        <h4 className="nd-nanty-card-title">{nantyTitle(card.code)}</h4>
        {card.created_at_ms !== undefined ? <span className="nd-nanty-time nd-num">{formatTime(card.created_at_ms)}</span> : null}
      </div>
      <FactsLine project={projectName} facts={factTexts(card.params ?? {}, agentLabel)} />
      {why !== null ? (
        <p className="nd-nanty-why" data-nanty-why="">
          {why}
        </p>
      ) : null}
      <CardActions card={card} decisions={decisions} primary={primary} />
    </article>
  );
}

function DoneCard({ done, decisions, projectName, now }: { done: NantyDone; decisions: NantyDecisions; projectName: string | null; now: number }) {
  const { t } = useT();
  const error = done.result === "failed" && typeof done.params.error === "string" ? nantyCodeText("nanty.fact.error", done.params, done.error_params) : null;
  const undo = canUndo(done, now) ? done.undo : null;
  return (
    <article className="nd-nanty-card" data-nanty-done={done.action ?? done.code} data-result={done.result}>
      <div className="nd-nanty-card-head">
        <span className="nd-dot" data-tone={done.result === "failed" ? "rose" : "green"} aria-hidden="true" />
        <h4 className="nd-nanty-card-title">{nantyTitle(done.code)}</h4>
        <span className="nd-nanty-time nd-num">{formatTime(done.at_ms)}</span>
      </div>
      <FactsLine project={projectName} facts={factTexts(done.params, agentLabel)} />
      <div className="nd-nanty-card-foot">
        <p className="nd-nanty-why">{error === null ? doneWho(done) : `${doneWho(done)}. ${error}`}</p>
        {undo !== null ? (
          <button type="button" className="nd-btn nd-btn-sm" data-nanty-undo={undo.id} disabled={decisions.pending.has(undo.id)} onClick={() => void decisions.undo(undo)}>
            <Icon name="undo" size={14} />
            {t("nanty.feed.undo")}
          </button>
        ) : null}
      </div>
    </article>
  );
}

function PlannedCard({ item, projectName }: { item: NantyPlanned; projectName: string | null }) {
  const { t } = useT();
  return (
    <article className="nd-nanty-card nd-nanty-row" data-nanty-planned={item.action}>
      <span className="nd-dot" aria-hidden="true" />
      <span className="nd-nanty-card-title">{nantyTitle(item.code)}</span>
      <span className="nd-nanty-row-text">{[t("nanty.feed.plannedAt", { time: formatTime(item.at_ms) }), projectName, ...factTexts(item.params, agentLabel)].filter((p) => p !== null).join(" · ")}</span>
    </article>
  );
}

function DigestPlanned({ time, telegram, onChangeTime }: { time: string; telegram: boolean; onChangeTime: () => void }) {
  const { t } = useT();
  return (
    <article className="nd-nanty-card nd-nanty-row" data-nanty-planned="digest">
      <span className="nd-dot" aria-hidden="true" />
      {/* A hora gravada (`HH:MM`) no formato de hora da tela (12 h em en), como a do cartão. */}
      <span className="nd-nanty-card-title">{t("nanty.feed.digest", { time: clockText(time) })}</span>
      <span className="nd-nanty-row-text">{t(telegram ? "nanty.feed.digestTelegram" : "nanty.feed.digestWeb")}</span>
      <button type="button" className="nd-btn nd-btn-sm nd-nanty-row-action" onClick={onChangeTime}>
        {t("nanty.feed.changeTime")}
      </button>
    </article>
  );
}

function TodayLine({ done, projectName }: { done: NantyDone; projectName: string | null }) {
  return (
    <li className="nd-nanty-today-item" data-nanty-today={done.result}>
      <span className="nd-nanty-time nd-num">{formatTime(done.at_ms)}</span>
      <span className="nd-nanty-today-text">{[nantyTitle(done.code), projectName, doneWho(done)].filter((p) => p !== null).join(" · ")}</span>
    </li>
  );
}

interface FeedProps {
  status: NantyStatus | null;
  feed: NantyFeed | null;
  decisions: NantyDecisions;
  /** "Mudar horário" do resumo: abre os Ajustes. */
  onChangeDigestTime: () => void;
  /** O estado vazio (o avatar e o texto), quando nada há no feed. */
  empty: ReactNode;
}

export function NantyFeedView({ status, feed, decisions, onChangeDigestTime, empty }: FeedProps) {
  const { t } = useT();
  const projectName = useProjectName();
  const [digestTime, setDigestTime] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    void getNantyDigestTime().then((r) => {
      if (alive && r.ok) setDigestTime(r.data);
    });
    return () => {
      alive = false;
    };
  }, []);
  const now = Date.now();
  const sections = feedSections(feed, now);
  // Sem aviso, sem "feito" e sem o dia: o estado calmo; o previsto (o resumo da manhã) segue abaixo.
  const calm = sections.waiting.length + sections.auto.length + sections.today.length === 0;
  const notice = decisions.notice;
  return (
    <div className="nd-nanty-feed" data-nanty-feed="">
      {notice !== null && notice.origin === "feed" ? (
        <p role="status" className="nd-nanty-notice" data-tone={notice.tone} data-nanty-notice="">
          {notice.text}
        </p>
      ) : null}
      {calm ? empty : null}
      {sections.waiting.length > 0 ? (
        <Section id="waiting" label={t("nanty.feed.waiting")} count={sections.waiting.length}>
          {sections.waiting.map((card, i) => (
            <WaitingCard key={card.id} card={card} decisions={decisions} primary={i === 0} projectName={projectName(card.project_id)} />
          ))}
        </Section>
      ) : null}
      {sections.auto.length > 0 ? (
        <Section id="auto" label={t("nanty.feed.doneAuto")} count={sections.auto.length}>
          {sections.auto.map((done) => (
            <DoneCard key={`${String(done.at_ms)}-${done.code}`} done={done} decisions={decisions} projectName={projectName(done.project_id)} now={now} />
          ))}
        </Section>
      ) : null}
      {sections.planned.length > 0 || digestTime !== null ? (
        <Section id="planned" label={t("nanty.feed.planned")} count={sections.planned.length + (digestTime === null ? 0 : 1)}>
          {sections.planned.map((item) => (
            <PlannedCard key={`${String(item.at_ms)}-${item.action}`} item={item} projectName={projectName(item.project_id)} />
          ))}
          {digestTime !== null ? <DigestPlanned time={digestTime} telegram={status?.telegram_paired === true} onChangeTime={onChangeDigestTime} /> : null}
        </Section>
      ) : null}
      {sections.today.length > 0 ? (
        <Section id="today" label={t("nanty.feed.today")}>
          <ul className="nd-nanty-today">
            {sections.today.map((done) => (
              <TodayLine key={`${String(done.at_ms)}-${done.code}`} done={done} projectName={projectName(done.project_id)} />
            ))}
          </ul>
        </Section>
      ) : null}
    </div>
  );
}
