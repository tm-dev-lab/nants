/*
 * Aprovação automática (chave interna `yolo`) e Internet na barra superior (spec 093, decisão 8):
 * interruptores (`role="switch"`) do projeto ativo. Botões neutros do conjunto único (095/T02,
 * decisão 9): ligado, o tom (âmbar na aprovação automática, violeta na Internet) aparece só no pino do trilho. Ligar pede confirmação (com a explicação do
 * que muda); desligar é direto. Erros aparecem abaixo da barra (`role="alert"`).
 */
import { useId, useState } from "react";
import { putProjectInternet, putProjectYolo } from "../../api/client";
import type { ProjectState } from "../../api/types";
import { t as translateNow } from "../../i18n";
import { useT } from "../../i18n/react";
import { appStore } from "../../store";
import { Dialog } from "../Dialog";
import { Icon } from "../Icon";
import { BarAlert } from "./BarAlert";

interface ToggleProps {
  projectId: string;
  state: ProjectState;
  /** Recarrega o estado do espaço depois da troca. */
  onChanged: () => Promise<void>;
}

/** Explicação da aprovação automática, para leigos: o que passa a ser automático e o que nunca é. */
function AutoApprovalExplanation({ id }: { id: string }) {
  const { t } = useT();
  return (
    <div id={id} className="nd-dialog-text">
      <p>{t("autoApprove.intro")}</p>
      <ul>
        <li>{t("autoApprove.auto1")}</li>
        <li>{t("autoApprove.auto2")}</li>
      </ul>
      <p>{t("autoApprove.alwaysAsks")}</p>
      <ul>
        <li>{t("autoApprove.ask1")}</li>
        <li>{t("autoApprove.ask2")}</li>
        <li>{t("autoApprove.ask3")}</li>
        <li>{t("autoApprove.ask4")}</li>
        <li>{t("autoApprove.ask5")}</li>
      </ul>
      <p>{t("autoApprove.complex")}</p>
      <p className="nd-dialog-warning">{t("autoApprove.warning")}</p>
    </div>
  );
}

export function YoloToggle({ projectId, state, onChanged }: ToggleProps) {
  const { t } = useT();
  const titleId = useId();
  const textId = useId();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const on = state.yolo === true;

  async function apply(enabled: boolean): Promise<boolean> {
    setBusy(true);
    setError(null);
    const result = await putProjectYolo(projectId, enabled);
    setBusy(false);
    if (!result.ok) {
      setError(translateNow("autoApprove.error", { message: result.error.message }));
      return false;
    }
    await onChanged();
    return true;
  }

  return (
    <>
      <button
        type="button"
        role="switch"
        aria-checked={on}
        aria-label={t("autoApprove.label")}
        title={t("autoApprove.hint")}
        className="nd-btn nd-bar-btn nd-toggle"
        data-variant="ghost"
        data-tone="amber"
        data-yolo=""
        disabled={busy}
        onClick={() => {
          if (on) void apply(false);
          else {
            setError(null);
            setOpen(true);
          }
        }}
      >
        <span className="nd-switch" data-on={on ? "true" : undefined} aria-hidden="true" />
        <Icon name="zap" size={14} />
        <span>{t("autoApprove.short")}</span>
      </button>
      {error !== null && !open ? <BarAlert source="yolo" message={error} onDismiss={() => setError(null)} /> : null}
      {open ? (
        <Dialog titleId={titleId} title={t("autoApprove.confirmTitle")} describedById={textId} onClose={() => setOpen(false)}>
          <AutoApprovalExplanation id={textId} />
          {error !== null ? (
            <p role="alert" className="nd-dialog-error">
              {error}
            </p>
          ) : null}
          <div className="nd-dialog-actions">
            <button type="button" className="nd-btn" onClick={() => setOpen(false)}>
              {t("common.cancel")}
            </button>
            <button
              type="button"
              className="nd-btn"
              data-variant="primary"
              disabled={busy}
              onClick={() => {
                void apply(true).then((ok) => {
                  if (ok) setOpen(false);
                });
              }}
            >
              <Icon name="zap" size={14} />
              {t("autoApprove.allow")}
            </button>
          </div>
        </Dialog>
      ) : null}
    </>
  );
}

export function InternetToggle({ projectId, state }: Omit<ToggleProps, "onChanged">) {
  const { t } = useT();
  const titleId = useId();
  const textId = useId();
  const [optimistic, setOptimistic] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const checked = optimistic ?? state.architect_internet === true;

  async function apply(next: boolean): Promise<void> {
    if (optimistic !== null) return;
    setOptimistic(next);
    setError(null);
    const result = await putProjectInternet(projectId, next);
    if (!result.ok) {
      setOptimistic(null);
      setError(translateNow("internet.error", { message: result.error.message }));
      return;
    }
    const data = result.data;
    appStore.setState((s) => ({ projectStates: { ...s.projectStates, [projectId]: data } }));
    setOptimistic(null);
    setOpen(false);
  }

  return (
    <>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        aria-label={t("internet.label")}
        aria-busy={optimistic !== null}
        className="nd-btn nd-bar-btn nd-toggle"
        data-variant="ghost"
        data-tone="violet"
        data-internet=""
        disabled={optimistic !== null && !open}
        onClick={() => {
          if (checked) void apply(false);
          else {
            setError(null);
            setOpen(true);
          }
        }}
      >
        <span className="nd-switch" data-on={checked ? "true" : undefined} aria-hidden="true" />
        <Icon name="globe" size={14} />
        <span>{t("internet.short")}</span>
      </button>
      {error !== null && !open ? <BarAlert source="internet" message={error} onDismiss={() => setError(null)} /> : null}
      {open ? (
        <Dialog titleId={titleId} title={t("internet.confirmTitle")} describedById={textId} onClose={() => setOpen(false)}>
          <div id={textId} className="nd-dialog-text">
            <p>{t("internet.explanation")}</p>
            {state.web_search_available === false ? (
              <p role="note" data-internet-no-search="" className="nd-dialog-warning">
                {t("internet.noSearch")}
              </p>
            ) : null}
          </div>
          {error !== null ? (
            <p role="alert" className="nd-dialog-error">
              {error}
            </p>
          ) : null}
          <div className="nd-dialog-actions">
            <button type="button" className="nd-btn" onClick={() => setOpen(false)}>
              {t("common.cancel")}
            </button>
            <button
              type="button"
              className="nd-btn"
              data-variant="primary"
              disabled={optimistic !== null}
              onClick={() => void apply(true)}
            >
              <Icon name="globe" size={14} />
              {t("internet.allow")}
            </button>
          </div>
        </Dialog>
      ) : null}
    </>
  );
}
