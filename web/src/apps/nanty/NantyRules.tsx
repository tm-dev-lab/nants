/*
 * Aba Regras do Nanty (spec 138 T11, `reference/Rules.dc.html`): o que ele pode fazer, ação por
 * ação.
 *
 * - **Um nível por id do catálogo (C-02):** desligada, pergunta antes ou automática, num seletor de
 *   três posições. Mudar grava na hora (`PUT /api/nanty/actions/:id`); o erro sai com o código
 *   traduzido e o seletor volta ao nível gravado.
 * - **Faixa "só você":** travada, com o cadeado e o motivo; sem seletor.
 * - O texto do topo diz que o pedido em texto livre sempre volta como cartão de confirmação. Não há
 *   regras concorrentes, então nada de "vale a mais restritiva".
 */
import { useEffect, useId, useState } from "react";
import { Icon } from "../../desktop/Icon";
import type { MessageKey } from "../../i18n";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { getNantySettings, putNantyLevel, type NantyCatalogEntry, type NantyLevel, type NantySettings } from "./nantyApi";
import { nantyTitle } from "./nantyText";

const LEVELS: readonly NantyLevel[] = ["off", "ask", "auto"];

const LEVEL_KEYS: Readonly<Record<NantyLevel, MessageKey>> = {
  off: "nanty.level.off",
  ask: "nanty.level.ask",
  auto: "nanty.level.auto",
};

type GroupId = "run" | "connections" | "machine";

/** O grupo de cada ação (a referência): execução, conexões e máquina. Ação nova cai em execução. */
const GROUP_OF: Readonly<Record<string, GroupId>> = {
  approve_plan: "run",
  resume_run: "run",
  retry_provider: "run",
  pause_run: "run",
  swap_reserve: "connections",
  clean_orphans: "machine",
};

const GROUPS: readonly { id: GroupId; key: MessageKey }[] = [
  { id: "run", key: "nanty.rules.group.run" },
  { id: "connections", key: "nanty.rules.group.connections" },
  { id: "machine", key: "nanty.rules.group.machine" },
];

const NOTE_KEYS: Readonly<Record<string, MessageKey>> = {
  approve_plan: "nanty.rules.note.approve_plan",
  resume_run: "nanty.rules.note.resume_run",
  retry_provider: "nanty.rules.note.retry_provider",
  pause_run: "nanty.rules.note.pause_run",
  swap_reserve: "nanty.rules.note.swap_reserve",
  clean_orphans: "nanty.rules.note.clean_orphans",
  allow_command_always: "nanty.rules.note.allow_command_always",
  change_channel: "nanty.rules.note.change_channel",
};

type Notice = { tone: "ok" | "warn"; text: string };

function LevelPicker({ entry, level, busy, onPick }: { entry: NantyCatalogEntry; level: NantyLevel; busy: boolean; onPick: (level: NantyLevel) => void }) {
  const { t } = useT();
  const name = useId();
  return (
    <div role="radiogroup" aria-label={nantyTitle(`nanty.action.${entry.id}`)} aria-busy={busy} className="nd-segment nd-chrome nd-nanty-levels" data-nanty-levels={entry.id}>
      {LEVELS.map((value) => (
        <label key={value} className="nd-segment-item" data-checked={level === value ? "true" : undefined} data-level={value}>
          <input type="radio" name={name} value={value} checked={level === value} disabled={busy} onChange={() => onPick(value)} className="nd-sr-only" />
          <span>{t(LEVEL_KEYS[value])}</span>
        </label>
      ))}
    </div>
  );
}

function ActionRow({ entry, note, busy, onPick }: { entry: NantyCatalogEntry; note: string | null; busy: boolean; onPick: (level: NantyLevel) => void }) {
  return (
    <div className="nd-nanty-rule" data-nanty-rule={entry.id}>
      <div className="nd-nanty-rule-text">
        <span className="nd-nanty-rule-name">{nantyTitle(`nanty.action.${entry.id}`)}</span>
        {note !== null ? <span className="nd-nanty-rule-note">{note}</span> : null}
      </div>
      <LevelPicker entry={entry} level={entry.level} busy={busy} onPick={onPick} />
    </div>
  );
}

function LockedRow({ entry }: { entry: NantyCatalogEntry }) {
  const { t } = useT();
  const key = NOTE_KEYS[entry.id];
  return (
    <div className="nd-nanty-rule" data-nanty-rule={entry.id} data-locked="true">
      <Icon name="lock" size={16} />
      <div className="nd-nanty-rule-text">
        <span className="nd-nanty-rule-name">{nantyTitle(`nanty.action.${entry.id}`)}</span>
        {key !== undefined ? <span className="nd-nanty-rule-note">{t(key)}</span> : null}
        <span className="nd-nanty-rule-lock">{t("nanty.rules.locked")}</span>
      </div>
    </div>
  );
}

export function NantyRules() {
  const { t } = useT();
  const headingId = useId();
  const [settings, setSettings] = useState<NantySettings | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);

  useEffect(() => {
    let alive = true;
    void getNantySettings().then((r) => {
      if (!alive) return;
      if (r.ok) setSettings(r.data);
      else setLoadError(t("nanty.rules.loadFailed", { error: apiFailureText(r.error) }));
    });
    return () => {
      alive = false;
    };
  }, []);

  const pick = async (entry: NantyCatalogEntry, level: NantyLevel): Promise<void> => {
    if (busy !== null || level === entry.level) return;
    setBusy(entry.id);
    setNotice(null);
    const result = await putNantyLevel(entry.id, level);
    setBusy(null);
    if (result.ok) {
      setSettings(result.data);
      setNotice({ tone: "ok", text: t("nanty.rules.saved", { action: nantyTitle(`nanty.action.${entry.id}`), level: t(LEVEL_KEYS[level]) }) });
    } else {
      setNotice({ tone: "warn", text: t("nanty.rules.failed", { error: apiFailureText(result.error) }) });
    }
  };

  const executes = settings?.catalog.filter((e) => e.kind === "executes") ?? [];
  const personOnly = settings?.catalog.filter((e) => e.kind === "person_only") ?? [];
  const noReserve = settings !== null && Object.keys(settings.reserve).length === 0;
  const noteOf = (entry: NantyCatalogEntry): string | null => {
    if (entry.id === "swap_reserve" && noReserve) return t("nanty.rules.note.swap_reserve_none");
    const key = NOTE_KEYS[entry.id];
    return key === undefined ? null : t(key);
  };

  return (
    <div className="nd-nanty-rules" data-nanty-rules="" aria-labelledby={headingId} role="region">
      <header className="nd-nanty-rules-head">
        <h3 id={headingId} className="nd-nanty-heading">
          {t("nanty.rules.heading")}
        </h3>
        <p className="nd-nanty-lede" data-nanty-rules-intro="">
          {t("nanty.rules.intro")}
        </p>
      </header>
      {notice !== null ? (
        <p role="status" className="nd-nanty-notice" data-tone={notice.tone} data-nanty-notice="">
          {notice.text}
        </p>
      ) : null}
      {loadError !== null ? (
        <p role="alert" className="nd-field-error">
          {loadError}
        </p>
      ) : null}
      {GROUPS.map((group) => {
        const items = executes.filter((e) => (GROUP_OF[e.id] ?? "run") === group.id);
        if (items.length === 0) return null;
        return (
          <section key={group.id} className="nd-nanty-section" data-nanty-group={group.id}>
            <h4 className="nd-nanty-section-label">{t(group.key)}</h4>
            {items.map((entry) => (
              <ActionRow key={entry.id} entry={entry} note={noteOf(entry)} busy={busy !== null} onPick={(level) => void pick(entry, level)} />
            ))}
          </section>
        );
      })}
      {personOnly.length > 0 ? (
        <section className="nd-nanty-section" data-nanty-group="person">
          <h4 className="nd-nanty-section-label">{t("nanty.rules.group.person")}</h4>
          {personOnly.map((entry) => (
            <LockedRow key={entry.id} entry={entry} />
          ))}
        </section>
      ) : null}
    </div>
  );
}
