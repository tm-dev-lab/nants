/*
 * Aviso de bloqueio por prontidão (spec 093, decisão 15): o que falta para a ação e, em cada
 * item, "Configurar", que abre a aba certa das Configurações. Usado no campo da Conversa, no
 * "Aprovar e iniciar" do Plano, no "Retomar" e no "Precisa de você".
 */
import { useId } from "react";
import type { ReadinessItem } from "../../desktop/api";
import { Icon } from "../../desktop/Icon";
import { useShell } from "../../desktop/shellContext";
import { useT } from "../../i18n/react";
import { prereqMessage, readinessItemLabel, relabelPrereqs } from "./readiness";

interface ReadinessNoticeProps {
  /** Frase que explica o bloqueio ("Para conversar com o Arquiteto, falta:"). */
  title: string;
  missing: readonly ReadinessItem[];
}

/**
 * Falha de uma ação (texto fixo), com "Configurar" quando há pendências (412 `prereq_missing`).
 * Com `prereq`, os rótulos e a mensagem saem da prontidão ATUAL (relida depois do 412): os ids
 * com parâmetro (`connection_key:<id>`, `pricing:<modelo>`) aparecem com o rótulo do servidor.
 */
export function ProblemNotice({ message: given, missing: raw, prereq = false }: { message: string; missing: readonly ReadinessItem[] | null; prereq?: boolean | undefined }) {
  const shell = useShell();
  const { t } = useT();
  const missing = prereq && raw !== null ? relabelPrereqs(raw, shell.readiness) : raw;
  const message = prereq && missing !== null ? prereqMessage(missing) : given;
  return (
    <div role="alert" className="nd-problem" data-problem="">
      <p className="nd-dialog-error">{message}</p>
      {missing !== null && missing.length > 0 ? (
        <ul className="nd-readiness-list">
          {missing.map((item) => (
            <li key={item.id} className="nd-readiness-item" data-missing={item.id}>
              <span className="nd-readiness-label">{readinessItemLabel(item)}</span>
              <button
                type="button"
                className="nd-btn nd-btn-sm"
                aria-label={t("readiness.configureItem", { label: readinessItemLabel(item) })}
                data-settings-tab={item.settings_tab}
                onClick={() => shell.openSettings(item.settings_tab)}
              >
                <Icon name="settings" size={14} />
                {t("readiness.configure")}
              </button>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

export function ReadinessNotice({ title, missing }: ReadinessNoticeProps) {
  const shell = useShell();
  const { t } = useT();
  const titleId = useId();
  return (
    <section className="nd-readiness" aria-labelledby={titleId} data-readiness-notice="">
      <p id={titleId} className="nd-readiness-title">
        <Icon name="alert" size={14} />
        <span>{title}</span>
      </p>
      <ul className="nd-readiness-list" aria-labelledby={titleId}>
        {missing.map((item) => (
          <li key={item.id} className="nd-readiness-item" data-missing={item.id}>
            <span className="nd-readiness-label">{readinessItemLabel(item)}</span>
            <button
              type="button"
              className="nd-btn nd-btn-sm"
              aria-label={t("readiness.configureItem", { label: readinessItemLabel(item) })}
              data-settings-tab={item.settings_tab}
              onClick={() => shell.openSettings(item.settings_tab)}
            >
              <Icon name="settings" size={14} />
              {t("readiness.configure")}
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
