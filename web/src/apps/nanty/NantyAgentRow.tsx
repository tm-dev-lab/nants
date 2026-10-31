/*
 * O Nanty em Configurações > Agentes (spec 138 T11): uma linha com o modelo atual e "Abrir o
 * Nanty", que abre a janela dele já na aba Ajustes. Os campos ficam só lá (nada duplicado aqui).
 */
import "./nanty.css";
import { useEffect, useState } from "react";
import type { ConnectionView } from "../../api/types";
import { useShell } from "../../desktop/shellContext";
import { useT } from "../../i18n/react";
import { NantyAvatar } from "./NantyAvatar";
import { getNantySettings, type NantySettings } from "./nantyApi";
import { requestNantyTab } from "./nantyStore";

export function NantyAgentRow({ connections }: { connections: readonly ConnectionView[] }) {
  const { t } = useT();
  const shell = useShell();
  const [settings, setSettings] = useState<NantySettings | null>(null);

  useEffect(() => {
    let alive = true;
    void getNantySettings().then((r) => {
      if (alive && r.ok) setSettings(r.data);
    });
    return () => {
      alive = false;
    };
  }, []);

  let model: string | null = null;
  if (settings !== null) {
    const configured = settings.connection_id !== "" && settings.model.trim() !== "";
    const connection = connections.find((c) => c.id === settings.connection_id)?.name ?? settings.connection_id;
    model = configured ? t("settings.agents.nanty.model", { model: settings.model, connection }) : t("settings.agents.nanty.none");
  }

  return (
    <section className="nd-nanty-agent" data-nanty-agent="" aria-label={t("settings.agents.nanty.title")}>
      <NantyAvatar mood="attentive" size={32} label={t("settings.agents.nanty.title")} />
      <div className="nd-nanty-agent-text">
        <h4 className="nd-role-title" data-tone="violet">
          {t("settings.agents.nanty.title")}
        </h4>
        {model !== null ? (
          <p className="nd-nanty-agent-model" data-nanty-agent-model="">
            {model}
          </p>
        ) : null}
        <p className="nd-field-hint">{t("settings.agents.nanty.hint")}</p>
      </div>
      <button
        type="button"
        className="nd-btn nd-btn-sm"
        data-nanty-agent-open=""
        onClick={() => {
          requestNantyTab("settings");
          shell.openWindow("nanty");
        }}
      >
        {t("settings.agents.nanty.open")}
      </button>
    </section>
  );
}
