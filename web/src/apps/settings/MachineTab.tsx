/*
 * Aba Máquina: diagnóstico de Git, Docker e Compose, com a
 * versão, o detalhe e o comando de correção para copiar (o Nants nunca o executa), e "Verificar de
 * novo", que confere a máquina no servidor (`POST /api/readiness/refresh`) e relê a lista.
 * Em `--lan`, abaixo, a seção "Acesso pela rede" (spec 097, `NetworkSection`).
 * No fim, a versão do Nants (spec 103 T03), que vem só do `Cargo.toml` por `GET /api/config`.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { getPrereqs } from "../../api/client";
import type { PrereqItem } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import { useShell } from "../../desktop/shellContext";
import { useT } from "../../i18n/react";
import { useAppStore } from "../../store";
import { prereqDetail, prereqFix, prereqLabel } from "../shared/readiness";
import { NetworkSection } from "./NetworkSection";

/** Copia o comando de correção (sem rede; falha vira "Copie manualmente"). */
function CopyFix({ text, label }: { text: string; label: string }) {
  const { t } = useT();
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  return (
    <span className="nd-row">
      <code className="nd-mono nd-code">{text}</code>
      <button
        type="button"
        className="nd-btn nd-btn-sm"
        aria-label={t("common.copyCommandFor", { label })}
        onClick={() => {
          const clip = typeof navigator !== "undefined" ? navigator.clipboard : undefined;
          if (clip === undefined) {
            setState("failed");
            return;
          }
          clip.writeText(text).then(
            () => setState("copied"),
            () => setState("failed"),
          );
        }}
      >
        <Icon name={state === "copied" ? "check" : "copy"} size={14} />
        {state === "copied" ? t("common.copied") : state === "failed" ? t("common.copyManually") : t("common.copy")}
      </button>
    </span>
  );
}

type Load = { status: "loading" } | { status: "error" } | { status: "ok"; items: PrereqItem[] };

export function MachineTab() {
  const shell = useShell();
  const { t } = useT();
  const version = useAppStore((s) => s.config?.version);
  const [load, setLoad] = useState<Load>({ status: "loading" });
  const [busy, setBusy] = useState(false);
  const [recheckFailed, setRecheckFailed] = useState(false);
  const seq = useRef(0);
  const alive = useRef(true);

  const reload = useCallback(async (): Promise<void> => {
    const mine = ++seq.current;
    const result = await getPrereqs();
    if (!alive.current || mine !== seq.current) return;
    setLoad(result.ok ? { status: "ok", items: result.data } : { status: "error" });
  }, []);

  useEffect(() => {
    alive.current = true;
    void reload();
    return () => {
      alive.current = false;
    };
  }, [reload]);

  const recheck = async (): Promise<void> => {
    setBusy(true);
    setRecheckFailed(false);
    const ok = await shell.recheckMachine();
    if (!alive.current) return;
    await reload();
    if (!alive.current) return;
    setBusy(false);
    setRecheckFailed(!ok);
  };

  return (
    <div className="nd-form" data-machine-tab="">
      <p className="nd-field-hint">{t("settings.machine.intro")}</p>
      {load.status === "loading" ? (
        <p role="status" className="nd-muted">
          {t("settings.machine.checking")}
        </p>
      ) : null}
      {load.status === "error" ? (
        <p role="alert" className="nd-field-error">
          {t("settings.machine.failed")}
        </p>
      ) : null}
      {load.status === "ok" ? (
        <ul className="nd-prereq-list" aria-label={t("settings.machine.list")}>
          {load.items.map((item) => {
            // Rótulo, detalhe e correção no idioma ativo (spec 102 T05); a versão como veio.
            const label = prereqLabel(item);
            const detail = item.ok ? undefined : prereqDetail(item);
            const fix = item.ok ? undefined : prereqFix(item);
            return (
              <li key={item.id} className="nd-prereq" data-prereq={item.id} data-ok={item.ok ? "true" : "false"}>
                <span className="nd-row">
                  <span className="nd-prereq-mark" data-ok={item.ok ? "true" : "false"}>
                    <Icon name={item.ok ? "check" : "alert"} size={14} />
                  </span>
                  <span className="nd-strong">{label}</span>
                  <span className="nd-sr-only">{`: ${t(item.ok ? "setup.status.ok" : "setup.status.fail")}`}</span>
                  {item.version !== undefined && item.version !== "" ? <span className="nd-muted">{item.version}</span> : null}
                </span>
                {detail !== undefined ? <p className="nd-muted">{detail}</p> : null}
                {fix !== undefined ? <CopyFix text={fix} label={label} /> : null}
              </li>
            );
          })}
        </ul>
      ) : null}
      {recheckFailed ? (
        <p role="alert" className="nd-field-error">
          {t("settings.machine.recheckFailed")}
        </p>
      ) : null}
      <div className="nd-row">
        <button type="button" className="nd-btn" disabled={busy} aria-busy={busy} data-recheck="" onClick={() => void recheck()}>
          <Icon name="refresh" size={14} />
          {busy ? t("common.checking") : t("common.checkAgain")}
        </button>
      </div>
      <NetworkSection />
      {version !== undefined && version !== "" ? (
        <p className="nd-muted" data-nants-version="">
          {t("settings.machine.version", { version })}
        </p>
      ) : null}
    </div>
  );
}
