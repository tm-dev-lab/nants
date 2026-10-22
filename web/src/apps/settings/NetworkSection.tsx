/*
 * Seção "Acesso pela rede" da aba Máquina (spec 097, decisão 5; spec 111, T03; spec 112, T02).
 *
 * Jornada guiada em passos (spec 112, decisão 8), numerados por ambiente:
 * - WSL NAT: ligar, preparar o Windows, adicionar; espelhado: ligar, firewall, adicionar;
 *   Linux direto: ligar, adicionar.
 * - Cada passo mostra o estado conferido pelo servidor (`GET /api/config/network`), e o seguinte
 *   só fica habilitado quando o anterior estiver pronto; desabilitado, ele diz o motivo. Com o
 *   Windows não conferido, o passo de adicionar fica habilitado com a ressalva.
 * - Volta automática: o estado é relido a cada 30 s com a aba visível; se o Windows deixar de
 *   estar pronto (o IP do WSL mudou), o passo 2 volta a ficar pendente sozinho, com o script.
 *
 * Passo 1: o interruptor "Acesso pela rede local" mostra e grava a opção salva (`PUT`); a mudança
 * vale a partir do próximo início (a seção mostra o comando para reiniciar). O `GET` leva cerca de
 * 1,5 s no WSL (consulta ao Windows): enquanto isso, só uma linha discreta.
 * Passo 2 (WSL): os scripts do PowerShell da 111, para ligar e para desfazer, em blocos recolhidos
 * (o Nants nunca os executa), e a coerência com o Windows. Com a rede desligada, só o de desfazer.
 * Passo 3: adicionar dispositivo e a lista (`DevicesStep`).
 *
 * Com o Nants na rede nesta execução (`lan_view_ports`), o endereço e a faixa de visualização,
 * uma vez só, e o estado: no WSL com o Windows conferido, o estado real; no Linux ou sem conferir,
 * Liberada ou Pendente pela marcação manual (a mesma do "Já liberei" do App).
 */
import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { getNetworkConfig, putNetworkConfig } from "../../api/client";
import type { NetworkConfig } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import { rangeLabel, rangeReleased } from "../../desktop/network";
import { useShell } from "../../desktop/shellContext";
import { useNetworkView } from "../../desktop/useNetwork";
import { useT } from "../../i18n/react";
import { reachEnvironment } from "../app/runSession";
import { copyText } from "../conversation/ResultMessage";
import { journeySteps, stepIsDone, type JourneyStep } from "./devicesModel";
import { DevicesStep } from "./DevicesStep";
import {
  NETWORK_TEXT,
  coherenceIsWarning,
  coherenceText,
  nantsLanAddress,
  networkScripts,
  parseNetworkConfig,
  restartNeeded,
  savedLan,
  scriptForCoherence,
  windowsReleased,
} from "./networkModel";

type Load = { status: "loading" } | { status: "error" } | { status: "ok"; net: NetworkConfig };

/** Releitura do estado para a volta automática (substituível nos testes). */
export const networkRefresh = {
  intervalMs: 30_000,
  visible: (): boolean => typeof document === "undefined" || document.visibilityState !== "hidden",
};

/** Script em bloco recolhido, com o botão de copiar. */
function ScriptBlock({ kind, title, script, open, children }: { kind: "enable" | "undo"; title: string; script: string; open: boolean; children: ReactNode }) {
  const { t } = useT();
  const [copy, setCopy] = useState<"idle" | "copied" | "failed">("idle");
  return (
    <details className="nd-details nd-settings-wsl" data-network-script-block={kind} open={open}>
      <summary>
        <Icon name="chevron-right" size={14} className="nd-details-chevron" />
        {title}
      </summary>
      <div className="nd-settings-wsl-body">
        {children}
        <pre className="nd-mono nd-code nd-settings-script" aria-label={`${NETWORK_TEXT.script}: ${title}`} data-network-script={kind}>
          {script}
        </pre>
        <div className="nd-row">
          <button
            type="button"
            className="nd-btn nd-btn-sm"
            data-network-copy={kind}
            onClick={() => {
              void copyText(script).then((ok) => setCopy(ok ? "copied" : "failed"));
            }}
          >
            <Icon name={copy === "copied" ? "check" : "copy"} size={14} />
            {copy === "copied" ? t("common.copied") : copy === "failed" ? t("common.copyManually") : t("common.copy")}
          </button>
        </div>
      </div>
    </details>
  );
}

/** Dica curta do firewall do Linux (o Nants não mexe nele). */
function LinuxHint({ net, factsShown }: { net: NetworkConfig; factsShown: boolean }) {
  const { t } = useT();
  const ports = net.view_ports === null ? String(net.lan_port) : `${net.lan_port}, ${rangeLabel(net.view_ports)}`;
  return (
    <p className="nd-field-hint" data-network-linux="">
      {factsShown ? NETWORK_TEXT.linuxHintAbove : t("settings.network.linuxHint", { ports })}
    </p>
  );
}

/**
 * Scripts do WSL (passo 2). Com o passo bloqueado (rede desligada nesta execução), só o de desfazer,
 * como na 111; senão, o de ligar e o de desfazer.
 */
function WslScripts({ net, onlyUndo }: { net: NetworkConfig; onlyUndo: boolean }) {
  const scripts = networkScripts(net);
  if (scripts === null) return null;
  const mirrored = net.env === "wsl_mirrored";
  const open = scriptForCoherence(net.coherence);
  return (
    <div className="nd-settings-wsl-list" data-network-wsl={net.env}>
      {mirrored ? <p className="nd-field-hint">{NETWORK_TEXT.mirroredIntro}</p> : null}
      {net.env === "wsl_unknown" ? (
        <p className="nd-field-hint" data-network-unknown-note="">
          {NETWORK_TEXT.unknownNote}
        </p>
      ) : null}
      {onlyUndo ? null : (
        <ScriptBlock kind="enable" title={NETWORK_TEXT.enableTitle} script={scripts.enable} open={open === "enable"}>
          <p className="nd-field-hint">{mirrored ? NETWORK_TEXT.mirroredEnableIntro : NETWORK_TEXT.wslIntro}</p>
          {mirrored ? null : <p className="nd-field-hint">{NETWORK_TEXT.wslRestart}</p>}
          {net.env === "wsl_nat" ? <p className="nd-field-hint">{NETWORK_TEXT.wslMirrored}</p> : null}
        </ScriptBlock>
      )}
      <ScriptBlock kind="undo" title={NETWORK_TEXT.undoTitle} script={scripts.undo} open={open === "undo"}>
        <p className="nd-field-hint">{mirrored ? NETWORK_TEXT.mirroredUndoIntro : NETWORK_TEXT.undoIntro}</p>
      </ScriptBlock>
    </div>
  );
}

/** Marca do passo: o número, ou o visto quando pronto. */
function StepMark({ n, step }: { n: number; step: JourneyStep }) {
  return (
    <span className="nd-step-mark" data-step-mark={step.status} aria-hidden="true">
      {stepIsDone(step.status) ? <Icon name="check" size={14} /> : n}
    </span>
  );
}

/** Um passo: número, título, o estado conferido (ou o motivo) e o corpo. */
function Step({
  n,
  step,
  detailId,
  coherence,
  children,
}: {
  n: number;
  step: JourneyStep;
  detailId?: string;
  /** Aviso de coerência do servidor que diz o mesmo, mais preciso: entra no lugar do estado. */
  coherence?: { code: string; text: string } | null;
  children?: ReactNode;
}) {
  const { t } = useT();
  const warn = (step.status === "pending" && step.id === "windows") || (coherence ?? null) !== null;
  return (
    <li className="nd-step" data-step={step.id} data-step-status={step.status} aria-label={`${t("devices.stepNumber", { n })}: ${t(step.title)}`}>
      <StepMark n={n} step={step} />
      <div className="nd-step-body">
        <h4 className="nd-step-title nd-strong">{t(step.title)}</h4>
        <p id={detailId} className={warn ? "nd-field-warn" : "nd-field-hint"} data-step-detail="" data-network-coherence={coherence?.code}>
          {coherence?.text ?? t(step.detail)}
        </p>
        {children}
      </div>
    </li>
  );
}

export function NetworkSection() {
  const shell = useShell();
  const { t } = useT();
  const network = useNetworkView();
  const [load, setLoad] = useState<Load>({ status: "loading" });
  const [changedHere, setChangedHere] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const alive = useRef(true);
  const addReasonId = useId();

  const seq = useRef(0);
  const reload = useCallback(async (): Promise<NetworkConfig | null> => {
    const mine = ++seq.current;
    const result = await getNetworkConfig();
    if (!alive.current || mine !== seq.current) return null;
    return result.ok ? parseNetworkConfig(result.data) : null;
  }, []);

  useEffect(() => {
    alive.current = true;
    void reload().then((net) => {
      if (!alive.current) return;
      setLoad(net === null ? { status: "error" } : { status: "ok", net });
    });
    return () => {
      alive.current = false;
    };
  }, [reload]);

  // Volta automática: relê o estado conferido de tempos em tempos (só com a aba visível).
  useEffect(() => {
    const timer = setInterval(() => {
      if (!networkRefresh.visible()) return;
      void reload().then((net) => {
        // `reload` só devolve a leitura mais recente: ela vale também depois de gravar (limpa o `stale`
        // se tomou o lugar da releitura do interruptor).
        if (!alive.current || net === null) return;
        setStale(false);
        setLoad((prev) => (prev.status === "ok" && JSON.stringify(prev.net) === JSON.stringify(net) ? prev : { status: "ok", net }));
      });
    }, networkRefresh.intervalMs);
    return () => clearInterval(timer);
  }, [reload]);

  const toggle = useCallback(async (net: NetworkConfig) => {
    const next = !savedLan(net);
    setSaving(true);
    setSaveError(null);
    const result = await putNetworkConfig(next);
    if (!alive.current) return;
    setSaving(false);
    if (!result.ok) {
      setSaveError(t("settings.network.saveFailed", { message: result.error.message }));
      return;
    }
    const saved = typeof result.data.saved_lan === "boolean" ? result.data.saved_lan : next;
    const command = typeof result.data.restart_command === "string" && result.data.restart_command !== "" ? result.data.restart_command : net.restart_command;
    setChangedHere(true);
    setLoad({ status: "ok", net: { ...net, saved_lan: saved, restart_command: command } });
    // A coerência depende da opção salva: relê o estado (a regra é do servidor).
    setStale(true);
    const fresh = await reload();
    if (!alive.current || fresh === null) return;
    setStale(false);
    setLoad({ status: "ok", net: fresh });
  }, [t, reload]);

  const range = network?.lan_view_ports ?? null;
  const released = range !== null && rangeReleased(shell.lanReleased, range);
  const net = load.status === "ok" ? load.net : null;
  const on = net !== null && savedLan(net);
  // Salva ligada e ainda fora da rede: o passo 1 já diz que vale ao reiniciar (sem repetir aqui).
  const restart = net !== null && restartNeeded(net, changedHere) && !(savedLan(net) && !net.lan);
  // A coerência vem do servidor; depois de gravar, ela é relida (até lá, nada é dito).
  const coherence = net === null || stale ? null : coherenceText(net.coherence, net);
  // No WSL com o Windows conferido, o estado real; a marcação manual só no Linux ou sem conferir.
  const checked = net === null ? null : windowsReleased(net);
  const steps = net === null ? [] : journeySteps(net);
  const factsShown = range !== null && network !== null;

  /** Aviso que pede o script de ligar, no lugar do estado do passo 2 (dizem o mesmo). */
  const windowsCoherence = (step: JourneyStep): { code: string; text: string } | null => {
    if (net === null || coherence === null || step.id !== "windows" || step.status === "blocked") return null;
    return scriptForCoherence(net.coherence) === "enable" ? { code: net.coherence, text: coherence } : null;
  };

  /** Texto de coerência no passo certo: a adoção no passo 1; o resto no passo 2 (no WSL). */
  const coherenceLine = (where: "enable" | "windows", step: JourneyStep): ReactNode => {
    if (net === null || coherence === null) return null;
    const inEnable = net.coherence === "will_adopt" || net.env === "linux";
    if ((where === "enable") !== inEnable) return null;
    // Passo 2 bloqueado (rede desligada nesta execução): só o aviso de desfazer.
    if (where === "windows" && step.status === "blocked" && net.coherence !== "still_redirected") return null;
    // O aviso que pede o script de ligar já está no estado do passo 2.
    if (where === "windows" && windowsCoherence(step) !== null) return null;
    return (
      <p className={coherenceIsWarning(net.coherence) ? "nd-field-warn" : "nd-field-hint"} data-network-coherence={net.coherence}>
        {coherence}
      </p>
    );
  };

  const stepBody = (step: JourneyStep): ReactNode => {
    if (net === null) return null;
    switch (step.id) {
      case "enable":
        return (
          <>
            <div className="nd-settings-lan">
              <button
                type="button"
                role="switch"
                aria-checked={on}
                aria-busy={saving}
                disabled={saving}
                className="nd-btn nd-toggle"
                data-variant="ghost"
                data-network-lan=""
                onClick={() => void toggle(net)}
              >
                <span className="nd-switch" data-on={on ? "true" : undefined} aria-hidden="true" />
                <span>{NETWORK_TEXT.lanLabel}</span>
              </button>
              <p className="nd-field-hint">{NETWORK_TEXT.lanHint}</p>
              <p className="nd-field-hint" data-network-security="">
                {NETWORK_TEXT.security}
              </p>
            </div>
            {saveError !== null ? (
              <p role="alert" className="nd-field-error" data-network-save-error="">
                {saveError}
              </p>
            ) : null}
            {restart ? (
              <p className="nd-field-hint" role="status" data-network-restart="">
                {net.lan ? NETWORK_TEXT.runOn : NETWORK_TEXT.runOff} {NETWORK_TEXT.nextStart} <kbd>Ctrl+C</kbd> {NETWORK_TEXT.restartTail} <code className="nd-mono nd-code">{net.restart_command}</code>
              </p>
            ) : changedHere ? (
              <p className="nd-field-hint" role="status" data-network-saved="">
                {NETWORK_TEXT.savedNow}
              </p>
            ) : null}
            {net.mode_source === "flag" ? (
              <p className="nd-muted" data-network-flag="">
                {NETWORK_TEXT.fromFlag}
              </p>
            ) : null}
            {coherenceLine("enable", step)}
            {net.env === "linux" ? <LinuxHint net={net} factsShown={factsShown} /> : null}
          </>
        );
      case "windows":
        return (
          <>
            {coherenceLine("windows", step)}
            <WslScripts net={net} onlyUndo={step.status === "blocked"} />
          </>
        );
      case "add":
        return <DevicesStep step={step} lanPort={net.lan_port} reasonId={addReasonId} />;
    }
  };

  return (
    <section className="nd-settings-network" aria-label={NETWORK_TEXT.title} data-network-section="">
      <h3 className="nd-strong">{NETWORK_TEXT.title}</h3>
      {load.status === "loading" ? (
        <p className="nd-muted" aria-live="polite" data-network-loading="">
          {NETWORK_TEXT.loading}
        </p>
      ) : null}
      {load.status === "error" ? (
        <p className="nd-muted" data-network-load-failed="">
          {NETWORK_TEXT.loadFailed}
        </p>
      ) : null}
      {factsShown ? (
        <>
          <p className="nd-field-hint">{NETWORK_TEXT.hint}</p>
          <dl className="nd-facts">
            <dt>{NETWORK_TEXT.address}</dt>
            <dd data-network-address="">{nantsLanAddress(reachEnvironment.pageHostname(), network.nants_port)}</dd>
            <dt>{NETWORK_TEXT.ports}</dt>
            <dd data-network-range="">{rangeLabel(range)}</dd>
            <dt>{NETWORK_TEXT.state}</dt>
            {checked !== null ? (
              <dd data-network-state={checked ? "windows-released" : "windows-pending"}>{checked ? NETWORK_TEXT.windowsReleased : NETWORK_TEXT.windowsPending}</dd>
            ) : (
              <dd data-network-state={released ? "released" : "pending"}>{released ? NETWORK_TEXT.released : NETWORK_TEXT.pending}</dd>
            )}
          </dl>
          {checked === null ? (
            <>
              <p className="nd-field-hint">{NETWORK_TEXT.unverified}</p>
              <div className="nd-row">
                <button
                  type="button"
                  className="nd-btn nd-btn-sm"
                  data-network-toggle=""
                  onClick={() => shell.setLanReleased(released ? null : range)}
                >
                  <Icon name={released ? "undo" : "check"} size={14} />
                  {released ? NETWORK_TEXT.markPending : NETWORK_TEXT.markReleased}
                </button>
              </div>
            </>
          ) : null}
        </>
      ) : null}
      {net !== null ? (
        <ol className="nd-steps" aria-label={t("devices.steps")} data-network-steps={net.env}>
          {steps.map((step, i) => (
            <Step key={step.id} n={i + 1} step={step} detailId={step.id === "add" ? addReasonId : undefined} coherence={windowsCoherence(step)}>
              {stepBody(step)}
            </Step>
          ))}
        </ol>
      ) : null}
    </section>
  );
}
