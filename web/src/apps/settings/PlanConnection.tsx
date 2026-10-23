/*
 * Conexão pelo plano ChatGPT (spec 116 T03):
 * - `ChatgptButton`: "Continue with ChatGPT" (só o texto: o logo oficial só entra com o asset e as
 *   condições de uso confirmados). Pede a URL de autorização e segue para ela na mesma janela; num
 *   aparelho da rede, explica que o login é feito no computador onde o Nants roda.
 * - `PlanConnectionPanel`: a conexão pronta (depois do login) ou em edição: o nome, editável, e os
 *   modelos da conta.
 * - `SignOutDialog`: confirmação do "Sair".
 */
import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import { getConnectionModels, logoutChatgpt, putConnection, startChatgptLogin, type ApiFailure } from "../../api/client";
import type { ConnectionView } from "../../api/types";
import { Dialog } from "../../desktop/Dialog";
import { Icon } from "../../desktop/Icon";
import { useShell } from "../../desktop/shellContext";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { describeModelsError } from "../../settings/rules";
import { appStore } from "../../store";
import { PLAN_USAGE_URL, rememberReturnSpace } from "./chatgptPlan";

/** Navegação até a autorização (trocada nos testes: o jsdom não navega). */
export const authNavigation = {
  go(url: string): void {
    window.location.assign(url);
  },
};

/** Atualiza a conexão no store, mantendo o estado do login quando a resposta não o traz. */
export function storeConnection(view: ConnectionView): void {
  appStore.setState((s) => {
    if (s.config === null) return { config: null };
    const list = s.config.connections;
    const old = list.find((c) => c.id === view.id);
    const merged: ConnectionView = view.chatgpt === undefined && old?.chatgpt !== undefined ? { ...view, chatgpt: old.chatgpt } : view;
    const next = old !== undefined ? list.map((c) => (c.id === view.id ? merged : c)) : [...list, merged];
    return { config: { ...s.config, connections: next } };
  });
}

type StartState = { kind: "idle" } | { kind: "busy" } | { kind: "remote" } | { kind: "failed"; failure: ApiFailure };

/**
 * Início do login pelo plano: pede a URL de autorização e segue para ela na mesma janela (guardando
 * o espaço aberto). Num aparelho da rede (`local: false`), não segue: a tela explica.
 */
export function useChatgptStart(connectionId?: string): { start: () => void; busy: boolean; notice: ReactNode } {
  const { t } = useT();
  const shell = useShell();
  const [state, setState] = useState<StartState>({ kind: "idle" });
  const mounted = useRef(true);
  const busyRef = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  async function run(): Promise<void> {
    if (busyRef.current) return;
    busyRef.current = true;
    setState({ kind: "busy" });
    const result = await startChatgptLogin(connectionId === undefined ? {} : { connection_id: connectionId });
    busyRef.current = false;
    if (!mounted.current) return;
    if (!result.ok) {
      setState({ kind: "failed", failure: result.error });
      return;
    }
    if (!result.data.local) {
      setState({ kind: "remote" });
      return;
    }
    rememberReturnSpace(shell.spaceId);
    authNavigation.go(result.data.url);
  }

  const notice =
    state.kind === "remote" ? (
      <p role="status" className="nd-field-hint" data-chatgpt-remote="">
        {t("chatgpt.remoteOnly")}
      </p>
    ) : state.kind === "failed" ? (
      <p role="alert" className="nd-field-error">
        {t("chatgpt.startFailed", { message: apiFailureText(state.failure) })}
      </p>
    ) : null;
  return { start: () => void run(), busy: state.kind === "busy", notice };
}

interface ChatgptButtonProps {
  /** Conexão a reconectar; sem ela, o login cria uma conexão nova. */
  connectionId?: string;
  /** Rótulo do botão (padrão: "Continue with ChatGPT"). */
  label?: string;
  /** Rótulo para o leitor de tela (ex.: "Reconectar ChatGPT (ana@x.com)"). */
  ariaLabel?: string;
  small?: boolean;
}

/** "Continue with ChatGPT" (ou "Reconectar"), só com o texto: sem o asset oficial, sem logo. */
export function ChatgptButton({ connectionId, label, ariaLabel, small = false }: ChatgptButtonProps) {
  const { t } = useT();
  const { start, busy, notice } = useChatgptStart(connectionId);
  return (
    <div className="nd-stack-xs" data-chatgpt-start="">
      <button
        type="button"
        className={small ? "nd-btn nd-btn-sm nd-self-start" : "nd-btn nd-self-start"}
        data-variant="primary"
        data-chatgpt-button=""
        disabled={busy}
        aria-busy={busy}
        {...(ariaLabel !== undefined ? { "aria-label": ariaLabel } : {})}
        onClick={start}
      >
        {busy ? t("chatgpt.opening") : (label ?? t("chatgpt.continue"))}
      </button>
      {notice}
    </div>
  );
}

type ModelsLoad = { status: "loading" } | { status: "ready"; names: string[] } | { status: "error"; message: string };

/** Modelos da conta (o nome mostrado; no plano, o `display_name`). */
function AccountModels({ connectionId }: { connectionId: string }) {
  const { t } = useT();
  const [load, setLoad] = useState<ModelsLoad>({ status: "loading" });
  const headingId = useId();
  useEffect(() => {
    let alive = true;
    void getConnectionModels(connectionId).then((outcome) => {
      if (!alive) return;
      if (!outcome.ok) {
        setLoad({ status: "error", message: t("chatgpt.ready.modelsFailed") });
        return;
      }
      const data = outcome.data;
      if (typeof data.error === "string") {
        setLoad({ status: "error", message: describeModelsError(data.error, data.status) });
        return;
      }
      const names = data.models.flatMap((m) => {
        if (typeof m?.id !== "string") return [];
        return [typeof m.name === "string" && m.name.trim() !== "" ? m.name : m.id];
      });
      setLoad({ status: "ready", names });
    });
    return () => {
      alive = false;
    };
    // `t` lido na hora da resposta; recarrega só quando a conexão muda.
  }, [connectionId]);
  return (
    <section aria-labelledby={headingId} className="nd-stack-xs" data-plan-models="">
      <h4 id={headingId} className="nd-section-label">
        {t("chatgpt.ready.models")}
      </h4>
      {load.status === "loading" ? (
        <p role="status" className="nd-muted">
          {t("chatgpt.ready.modelsLoading")}
        </p>
      ) : load.status === "error" ? (
        <p className="nd-field-warn">{load.message}</p>
      ) : load.names.length === 0 ? (
        <p className="nd-muted">{t("chatgpt.ready.noModels")}</p>
      ) : (
        <ul className="nd-plan-models">
          {load.names.map((name) => (
            <li key={name} className="nd-pill">
              {name}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

interface PanelProps {
  connection: ConnectionView;
  title: string;
  onDone: () => void;
}

/** A conexão pelo plano pronta ou em edição: o nome (editável) e os modelos da conta. */
export function PlanConnectionPanel({ connection, title, onDone }: PanelProps) {
  const { t } = useT();
  const nameId = useId();
  const errorId = useId();
  const [name, setName] = useState(connection.name);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<ApiFailure | null>(null);
  const [nameError, setNameError] = useState(false);

  async function save(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (busy) return;
    const trimmed = name.trim();
    setFailure(null);
    setNameError(trimmed === "");
    if (trimmed === "") return;
    if (trimmed === connection.name) {
      onDone();
      return;
    }
    setBusy(true);
    const result = await putConnection({ id: connection.id, name: trimmed, base_url: "", kind: "chatgpt_plan" });
    setBusy(false);
    if (!result.ok) {
      setFailure(result.error);
      return;
    }
    storeConnection(result.data);
    onDone();
  }

  return (
    <section className="nd-settings-form" aria-label={title} data-plan-panel={connection.id}>
      <h3 className="nd-strong">{title}</h3>
      <form className="nd-form" onSubmit={(event) => void save(event)} noValidate aria-describedby={failure !== null ? errorId : undefined}>
        <div className="nd-field">
          <label htmlFor={nameId} className="nd-field-label">
            {t("settings.form.name")}
          </label>
          <input id={nameId} type="text" className="nd-input" value={name} onChange={(e) => setName(e.target.value)} autoComplete="off" />
          {nameError ? <p className="nd-field-error">{t("settings.validation.nameRequired")}</p> : null}
        </div>
        <AccountModels connectionId={connection.id} />
        {failure !== null ? (
          <p id={errorId} role="alert" className="nd-dialog-error">
            {apiFailureText(failure)}
          </p>
        ) : null}
        <div className="nd-dialog-actions">
          <button type="submit" className="nd-btn" data-variant="primary" disabled={busy} aria-busy={busy}>
            <Icon name="check" size={14} />
            {t("chatgpt.ready.done")}
          </button>
        </div>
      </form>
    </section>
  );
}

/** Confirmação do "Sair": revoga na OpenAI e apaga o login guardado neste computador. */
export function SignOutDialog({ connection, onClose }: { connection: ConnectionView; onClose: () => void }) {
  const { t } = useT();
  const titleId = useId();
  const textId = useId();
  const cancelRef = useRef<HTMLButtonElement | null>(null);
  const closeRef = useRef<HTMLButtonElement | null>(null);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<ApiFailure | null>(null);
  // A OpenAI não confirmou a revogação: o login local já saiu, mas o diálogo fica com o aviso.
  const [unconfirmed, setUnconfirmed] = useState(false);

  async function confirm(): Promise<void> {
    if (busy) return;
    setBusy(true);
    setFailure(null);
    const result = await logoutChatgpt(connection.id);
    setBusy(false);
    if (!result.ok) {
      setFailure(result.error);
      return;
    }
    storeConnection({ ...connection, chatgpt: { state: "disconnected" } });
    if (result.data.revoked) {
      onClose();
      return;
    }
    setUnconfirmed(true);
  }

  useEffect(() => {
    if (unconfirmed) closeRef.current?.focus({ preventScroll: true });
  }, [unconfirmed]);

  return (
    <Dialog titleId={titleId} title={t("chatgpt.signOutTitle", { name: connection.name })} describedById={textId} onClose={onClose} initialFocus={cancelRef} alert>
      <div className="nd-stack-sm">
        {unconfirmed ? (
          <div role="alert" className="nd-stack-sm" data-chatgpt-revoke-unconfirmed="">
            <p id={textId} className="nd-dialog-warning">
              {t("chatgpt.revokeUnconfirmed")}
            </p>
            <p className="nd-muted">{t("chatgpt.revokeUnconfirmedFix")}</p>
          </div>
        ) : (
          <p id={textId} className="nd-muted">
            {t("chatgpt.signOutText")}
          </p>
        )}
        {failure !== null ? (
          <p role="alert" className="nd-dialog-error">
            {t("chatgpt.signOutFailed", { message: apiFailureText(failure) })}
          </p>
        ) : null}
        <div className="nd-dialog-actions">
          {unconfirmed ? (
            <>
              <a className="nd-btn" data-variant="ghost" href={PLAN_USAGE_URL} target="_blank" rel="noopener noreferrer" data-plan-usage="">
                <Icon name="external" size={14} />
                {t("chatgpt.openSettings")}
              </a>
              <button ref={closeRef} type="button" className="nd-btn" data-variant="primary" onClick={onClose}>
                {t("common.close")}
              </button>
            </>
          ) : (
            <>
              <button ref={cancelRef} type="button" className="nd-btn" onClick={onClose} disabled={busy}>
                {t("common.cancel")}
              </button>
              <button type="button" className="nd-btn" data-variant="danger" disabled={busy} aria-busy={busy} data-chatgpt-signout-confirm="" onClick={() => void confirm()}>
                {busy ? t("chatgpt.signingOut") : t("chatgpt.signOut")}
              </button>
            </>
          )}
        </div>
      </div>
    </Dialog>
  );
}
