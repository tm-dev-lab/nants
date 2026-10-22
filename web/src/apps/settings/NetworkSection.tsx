/*
 * Seção "Acesso pela rede" da aba Máquina (spec 097, decisão 5; spec 111, T03).
 *
 * - O interruptor "Acesso pela rede local" mostra e grava a opção salva (`GET`/`PUT
 *   /api/config/network`). A mudança vale a partir do próximo início: a seção diz isso e mostra o
 *   comando para reiniciar. O `GET` leva cerca de 1,5 s no WSL (consulta ao Windows): enquanto
 *   isso, só uma linha discreta, sem travar o resto das Configurações.
 * - Instruções por ambiente: no Linux, uma dica do firewall; no WSL, os scripts do PowerShell para
 *   ligar e para desfazer, em blocos recolhidos (o Nants nunca os executa).
 * - Coerência com o Windows: falta rodar o script, ou o Windows ainda redireciona as portas.
 * - Com o Nants na rede nesta execução (`lan_view_ports`), o endereço e a faixa de visualização,
 *   uma vez só, e o estado: no WSL com o Windows conferido, o estado real; no Linux ou sem conferir,
 *   Liberada ou Pendente pela marcação manual (a mesma do "Já liberei" do App).
 */
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { getNetworkConfig, putNetworkConfig } from "../../api/client";
import type { NetworkConfig } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import { rangeLabel, rangeReleased } from "../../desktop/network";
import { useShell } from "../../desktop/shellContext";
import { useNetworkView } from "../../desktop/useNetwork";
import { useT } from "../../i18n/react";
import { reachEnvironment } from "../app/runSession";
import { copyText } from "../conversation/ResultMessage";
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

/**
 * Instruções do ambiente: dica do Linux ou os scripts do WSL. Com o endereço e a faixa já na lista
 * acima (`factsShown`), a dica do Linux só aponta para eles, sem repetir.
 */
function EnvInstructions({ net, factsShown }: { net: NetworkConfig; factsShown: boolean }) {
  const { t } = useT();
  const scripts = networkScripts(net);
  if (scripts === null) {
    const ports = net.view_ports === null ? String(net.lan_port) : `${net.lan_port}, ${rangeLabel(net.view_ports)}`;
    return (
      <p className="nd-field-hint" data-network-linux="">
        {factsShown ? NETWORK_TEXT.linuxHintAbove : t("settings.network.linuxHint", { ports })}
      </p>
    );
  }
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
      <ScriptBlock kind="enable" title={NETWORK_TEXT.enableTitle} script={scripts.enable} open={open === "enable"}>
        <p className="nd-field-hint">{mirrored ? NETWORK_TEXT.mirroredEnableIntro : NETWORK_TEXT.wslIntro}</p>
        <p className="nd-field-hint" data-network-private-note="">
          {NETWORK_TEXT.wslPrivate}
        </p>
        {mirrored ? null : <p className="nd-field-hint">{NETWORK_TEXT.wslRestart}</p>}
        {net.env === "wsl_nat" ? <p className="nd-field-hint">{NETWORK_TEXT.wslMirrored}</p> : null}
      </ScriptBlock>
      <ScriptBlock kind="undo" title={NETWORK_TEXT.undoTitle} script={scripts.undo} open={open === "undo"}>
        <p className="nd-field-hint">{mirrored ? NETWORK_TEXT.mirroredUndoIntro : NETWORK_TEXT.undoIntro}</p>
      </ScriptBlock>
    </div>
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
  const restart = net !== null && restartNeeded(net, changedHere);
  // A coerência vem do servidor; depois de gravar, ela é relida (até lá, nada é dito).
  const coherence = net === null || stale ? null : coherenceText(net.coherence, net);
  // No WSL com o Windows conferido, o estado real; a marcação manual só no Linux ou sem conferir.
  const checked = net === null ? null : windowsReleased(net);

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
      {net !== null ? (
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
          {coherence !== null ? (
            <p className={coherenceIsWarning(net.coherence) ? "nd-field-warn" : "nd-field-hint"} data-network-coherence={net.coherence}>
              {coherence}
            </p>
          ) : null}
        </>
      ) : null}
      {range !== null && network !== null ? (
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
      {net !== null ? <EnvInstructions net={net} factsShown={range !== null} /> : null}
    </section>
  );
}
