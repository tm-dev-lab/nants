/*
 * Seção "Acesso pela rede" (spec 097, decisão 5), na aba Máquina, só com o Nants em `--lan` (a API
 * devolve a faixa de portas de visualização): o endereço do Nants na rede (host da página), a faixa
 * e o estado Liberada ou Pendente, com a ação de marcar ou desmarcar (a mesma marca do "Já liberei"
 * do App, no estado da interface global).
 *
 * Só com o servidor no WSL, um bloco recolhido traz o script do PowerShell para copiar, gerado com
 * os valores reais. O Nants nunca o executa. Fora do WSL, nenhum comando aparece.
 */
import { useState } from "react";
import { Icon } from "../../desktop/Icon";
import { rangeLabel, rangeReleased } from "../../desktop/network";
import { useShell } from "../../desktop/shellContext";
import { useNetworkView } from "../../desktop/useNetwork";
import { useT } from "../../i18n/react";
import { reachEnvironment } from "../app/runSession";
import { copyText } from "../conversation/ResultMessage";
import { NETWORK_TEXT, nantsLanAddress, wslPowerShellScript } from "./networkModel";

export function NetworkSection() {
  const shell = useShell();
  const { t } = useT();
  const network = useNetworkView();
  const [copy, setCopy] = useState<"idle" | "copied" | "failed">("idle");
  const range = network?.lan_view_ports ?? null;
  if (network === null || range === null) return null;
  const released = rangeReleased(shell.lanReleased, range);
  const script = network.wsl ? wslPowerShellScript(network.nants_port, range) : null;
  return (
    <section className="nd-settings-network" aria-label={NETWORK_TEXT.title} data-network-section="">
      <h3 className="nd-strong">{NETWORK_TEXT.title}</h3>
      <p className="nd-field-hint">{NETWORK_TEXT.hint}</p>
      <dl className="nd-facts">
        <dt>{NETWORK_TEXT.address}</dt>
        <dd data-network-address="">
          {nantsLanAddress(reachEnvironment.pageHostname(), network.nants_port)}
        </dd>
        <dt>{NETWORK_TEXT.ports}</dt>
        <dd data-network-range="">
          {rangeLabel(range)}
        </dd>
        <dt>{NETWORK_TEXT.state}</dt>
        <dd data-network-state={released ? "released" : "pending"}>{released ? NETWORK_TEXT.released : NETWORK_TEXT.pending}</dd>
      </dl>
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
      {script !== null ? (
        <details className="nd-details nd-settings-wsl" data-network-wsl="">
          <summary>
            <Icon name="chevron-right" size={14} className="nd-details-chevron" />
            {NETWORK_TEXT.wslTitle}
          </summary>
          <div className="nd-settings-wsl-body">
            <p className="nd-field-hint">{NETWORK_TEXT.wslIntro}</p>
            <pre className="nd-mono nd-code nd-settings-script" aria-label={NETWORK_TEXT.script} data-network-script="">
              {script}
            </pre>
            <div className="nd-row">
              <button
                type="button"
                className="nd-btn nd-btn-sm"
                data-network-copy=""
                onClick={() => {
                  void copyText(script).then((ok) => setCopy(ok ? "copied" : "failed"));
                }}
              >
                <Icon name={copy === "copied" ? "check" : "copy"} size={14} />
                {copy === "copied" ? t("common.copied") : copy === "failed" ? t("common.copyManually") : t("common.copy")}
              </button>
            </div>
            <p className="nd-field-hint" data-network-private-note="">
              {NETWORK_TEXT.wslPrivate}
            </p>
            <p className="nd-field-hint">{NETWORK_TEXT.wslRestart}</p>
            <p className="nd-field-hint">{NETWORK_TEXT.wslMirrored}</p>
          </div>
        </details>
      ) : null}
    </section>
  );
}
