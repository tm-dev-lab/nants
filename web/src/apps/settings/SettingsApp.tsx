/*
 * Janela Configurações (`DEFAULT_APPS.settings.Content`, em `memo`). Abas:
 * - Conexões: linhas salvas (só a máscara da chave; "Testar" usa o id salvo) e o formulário de
 *   criar/editar (protocolo; a chave só existe no campo `type=password`, limpo no envio);
 * - Agentes: conexão, modelo, esforço e limite de passos (com "Usar o mesmo modelo para todos");
 * - Preços: por modelo, com "US$ por imagem";
 * - Máquina: pré-requisitos com "Verificar de novo";
 * - Aparência: tema e papel de parede, iguais ao Personalizar, e o idioma (spec 101: "Idioma /
 *   Language", com os nomes sempre no próprio idioma; troca na hora e fica salva no servidor);
 * - Projetos: a raiz da pasta de projetos.
 *
 * Abertura direta: "Configurar" (prontidão, Custos...) chama `openSettings(aba)`; esta janela
 * aplica o `settingsRequest` e o consome. A aba ativa fica em `selection` (com espaço) e, nas
 * boas-vindas, só nesta sessão da janela. Cada aba mostra o que falta nela (prontidão).
 * 095/T06 (`reference/Janelas.dc.html`): as abas ficam no slot da barra de título (no celular, a
 * linha abaixo dela); estreita (`data-size`), cada aba mostra só o ícone. O conteúdo é lista com
 * `--divider`, sem caixa dentro de caixa: Conexões em linhas (ponto, nome, detalhe, "usada por" e
 * o menu "…") com "Nova conexão" abaixo.
 */
import "./settings.css";
import { memo, useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import type { ConnectionView } from "../../api/types";
import { loadConfig } from "../../configLoader";
import { AppearanceChoices } from "../../desktop/AppearanceChoices";
import { LANGS } from "../../i18n/lang";
import { useT } from "../../i18n/react";
import { messageText } from "../../i18n/serverCodes";
import { readinessItemLabel } from "../shared/readiness";
import { SETTINGS_WINDOW_TABS, type SettingsWindowTab } from "../../desktop/api";
import { Icon } from "../../desktop/Icon";
import { WindowTools } from "../../desktop/Window";
import { useShell, useSpace } from "../../desktop/shellContext";
import { useAppearance } from "../../desktop/theme/appearanceContext";
import type { ContainerSize } from "../../desktop/useContainerSize";
import type { UiSelection } from "../../desktop/uiState";
import { useAppStore } from "../../store";
import type { AppContentProps } from "../registry";
import "../shared/apps.css";
import { AgentsForm } from "./AgentsForm";
import { ConnectionRow } from "./ConnectionRow";
import { ConnectionForm } from "./ConnectionForm";
import { MachineTab } from "./MachineTab";
import { PricingEditor } from "./PricingEditor";
import { ProjectsRootTab } from "./ProjectsRootTab";
import { connectionUsers, DEFAULT_SETTINGS_TAB, SETTINGS_TAB_ICONS, SETTINGS_TAB_LABELS, tabByKey, tabMissing } from "./settingsModel";

/** O que falta nesta aba (prontidão), com o comando de correção quando houver. */
function TabMissing({ tab }: { tab: SettingsWindowTab }) {
  const { t } = useT();
  const readiness = useShell().readiness;
  const items = tabMissing(readiness, tab);
  if (items.length === 0) return null;
  return (
    <div role="note" className="nd-strip nd-settings-missing" data-tone="amber" data-tab-missing={tab}>
      <Icon name="alert" size={14} />
      <span>{t("prereq.missing", { items: items.map(readinessItemLabel).join(", ") })}</span>
    </div>
  );
}

/** Configuração ainda carregando ou com erro (com "Tentar de novo"); `null` quando pronta. */
function ConfigGate() {
  const { t } = useT();
  const config = useAppStore((s) => s.config);
  const configError = useAppStore((s) => s.configError);
  if (config !== null) return null;
  if (configError === null) {
    return (
      <p role="status" className="nd-muted">
        {t("windowEmpty.settings")}
      </p>
    );
  }
  return (
    <div role="alert" className="nd-stack-sm">
      <p className="nd-field-error">{messageText(configError)}</p>
      <button type="button" className="nd-btn nd-btn-sm nd-self-start" onClick={() => void loadConfig()}>
        <Icon name="refresh" size={14} />
        {t("common.tryAgain")}
      </button>
    </div>
  );
}

function ConnectionsTab() {
  const { t } = useT();
  const shell = useShell();
  const config = useAppStore((s) => s.config);
  // `null` = só a lista; `"new"` = criação; conexão = edição.
  const [editing, setEditing] = useState<ConnectionView | "new" | null>(null);
  if (config === null) return <ConfigGate />;
  const connections = config.connections;
  const showForm = editing !== null || connections.length === 0;
  return (
    <div className="nd-settings-section" data-connections-tab="">
      {connections.length > 0 ? (
        <ul className="nd-conn-list" aria-label={t("settings.savedConnections")}>
          {connections.map((c) => (
            <ConnectionRow
              key={c.id}
              connection={c}
              users={connectionUsers(config.agents, c.id)}
              onEdit={(conn) => setEditing(conn)}
              onTested={() => void shell.reloadReadiness()}
            />
          ))}
        </ul>
      ) : (
        <p className="nd-muted">{t("settings.noConnections")}</p>
      )}
      {showForm ? (
        <section className="nd-settings-form" aria-label={editing !== null && editing !== "new" ? t("settings.editConnectionLabel", { name: editing.name }) : t("settings.newConnection")}>
          <h3 className="nd-strong">{editing !== null && editing !== "new" ? t("settings.editConnectionTitle", { name: editing.name }) : t("settings.newConnection")}</h3>
          <ConnectionForm
            key={editing !== null && editing !== "new" ? editing.id : "new"}
            existingIds={connections.map((c) => c.id)}
            {...(editing !== null && editing !== "new" ? { initial: editing } : {})}
            onSaved={() => {
              setEditing(null);
              void shell.reloadReadiness();
            }}
            {...(connections.length > 0 ? { onCancel: () => setEditing(null) } : {})}
          />
        </section>
      ) : (
        <button type="button" className="nd-btn nd-self-start" onClick={() => setEditing("new")}>
          <Icon name="plus" size={14} />
          {t("settings.newConnection")}
        </button>
      )}
    </div>
  );
}

function AgentsTab({ onGoTo }: { onGoTo: (tab: SettingsWindowTab) => void }) {
  const { t } = useT();
  const shell = useShell();
  const config = useAppStore((s) => s.config);
  if (config === null) return <ConfigGate />;
  if (config.connections.length === 0) {
    return (
      <div className="nd-stack-sm">
        <p className="nd-field-warn">{t("settings.agentsNeedConnection")}</p>
        <button type="button" className="nd-btn nd-btn-sm nd-self-start" onClick={() => onGoTo("connections")}>
          <Icon name="key" size={14} />
          {t("settings.goToConnections")}
        </button>
      </div>
    );
  }
  return <AgentsForm agents={config.agents} connections={config.connections} onSaved={() => void shell.reloadReadiness()} />;
}

function PricingTab() {
  const shell = useShell();
  const config = useAppStore((s) => s.config);
  if (config === null) return <ConfigGate />;
  return <PricingEditor agents={config.agents} pricing={config.pricing} onSaved={() => void shell.reloadReadiness()} />;
}

/**
 * Idioma (spec 101, decisão 3): "Português (Brasil)" e "English", sempre escritos no próprio
 * idioma. A troca vale na hora (sem recarregar) e fica na seção global `language`.
 */
function LanguageChoice() {
  const { t, lang } = useT();
  const { setLanguage } = useAppearance();
  const labelId = useId();
  return (
    <div role="group" aria-labelledby={labelId} data-language-choice-group="">
      <div id={labelId} className="nd-label">
        {t("language.label")}
      </div>
      <div className="nd-choice-grid" data-columns="2">
        {LANGS.map((option) => {
          const on = option === lang;
          return (
            <button
              key={option}
              type="button"
              className="nd-choice"
              aria-pressed={on}
              lang={option}
              data-language-choice={option}
              onClick={() => {
                if (!on) setLanguage(option);
              }}
            >
              <span className="nd-choice-row">
                {t(`language.name.${option}`)}
                {on ? <Icon name="check" size={14} className="nd-choice-check" /> : null}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}

function AppearanceTab() {
  const { t } = useT();
  const { appearance, theme, setAppearance } = useAppearance();
  return (
    <div className="nd-settings-section nd-settings-appearance" data-appearance-tab="">
      <p className="nd-field-hint">{t("appearance.settingsHint")}</p>
      <LanguageChoice />
      <AppearanceChoices appearance={appearance} theme={theme} onChange={setAppearance} />
    </div>
  );
}

/**
 * As abas com nome cabem no slot da barra de título? Com nome e transbordando, passa a só ícone e
 * guarda a largura da barra em que isso aconteceu; só tenta o nome de novo quando a barra cresce
 * além dela (sem vaivém). Sem `ResizeObserver` (jsdom), fica com o nome.
 */
function useLabelsFit(list: HTMLElement | null): boolean {
  const [fit, setFit] = useState(true);
  const overflowAt = useRef<number | null>(null);
  const bar = list === null ? null : (list.closest("[data-window-tools]")?.parentElement ?? list.parentElement);

  const measure = useCallback((): void => {
    if (list === null || bar === null) return;
    const width = bar.getBoundingClientRect().width;
    if (width <= 0) return;
    if (fit) {
      if (list.scrollWidth > list.clientWidth + 1) {
        overflowAt.current = width;
        setFit(false);
      }
    } else if (overflowAt.current === null || width > overflowAt.current) {
      setFit(true);
    }
  }, [list, bar, fit]);

  useLayoutEffect(measure, [measure]);
  useEffect(() => {
    if (bar === null || typeof ResizeObserver !== "function") return undefined;
    const observer = new ResizeObserver(() => measure());
    observer.observe(bar);
    return () => observer.disconnect();
  }, [bar, measure]);

  return fit;
}

interface PanelProps {
  size: ContainerSize;
  /** Aba salva no `selection` do espaço (sem espaço: `undefined`). */
  savedTab: SettingsWindowTab | undefined;
  setSelection: ((patch: Partial<UiSelection>) => void) | null;
}

const SettingsPanel = memo(function SettingsPanel({ size, savedTab, setSelection }: PanelProps) {
  const shell = useShell();
  // Spec 101: o painel (memo) re-renderiza na troca de idioma (nomes das abas).
  const { t: tr, tp: trp } = useT();
  const readiness = shell.readiness;
  const baseId = useId();
  const [local, setLocal] = useState<SettingsWindowTab | null>(null);
  const tab = local ?? savedTab ?? DEFAULT_SETTINGS_TAB;
  const tabRefs = useRef(new Map<SettingsWindowTab, HTMLButtonElement>());

  const choose = useCallback(
    (next: SettingsWindowTab) => {
      setLocal(next);
      setSelection?.({ settingsTab: next });
    },
    [setSelection],
  );

  // "Configurar" de uma pendência (ou outro `openSettings`): aplica a aba e consome o pedido.
  const request = shell.settingsRequest;
  const consume = shell.consumeSettingsRequest;
  useEffect(() => {
    if (request === null) return;
    choose(request.tab);
    consume(request.id);
  }, [request, choose, consume]);

  const onTabKeyDown = (event: ReactKeyboardEvent<HTMLElement>): void => {
    const next = tabByKey(tab, event.key);
    if (next === null) return;
    event.preventDefault();
    choose(next);
    tabRefs.current.get(next)?.focus();
  };

  const tabId = (t: SettingsWindowTab): string => `${baseId}-tab-${t}`;
  const panelId = `${baseId}-panel`;
  // Janela estreita, ou abas que não cabem na barra: só o ícone de cada aba (o nome fica para o
  // leitor de tela e na dica).
  const [list, setList] = useState<HTMLDivElement | null>(null);
  const labelsFit = useLabelsFit(list);
  const compact = size === "narrow" || !labelsFit;

  let body;
  switch (tab) {
    case "connections":
      body = <ConnectionsTab />;
      break;
    case "agents":
      body = <AgentsTab onGoTo={choose} />;
      break;
    case "pricing":
      body = <PricingTab />;
      break;
    case "machine":
      body = <MachineTab />;
      break;
    case "appearance":
      body = <AppearanceTab />;
      break;
    case "projects":
      body = <ProjectsRootTab />;
      break;
  }

  return (
    <div className="nd-settings" data-settings="" data-size={size} data-tab={tab}>
      {/* Abas no slot da barra de título (095/T06, C-07); no celular, a linha abaixo da barra. */}
      <WindowTools inlineOutsideWindow>
        <div
          ref={setList}
          role="tablist"
          aria-label={tr("settings.tabs")}
          aria-orientation="horizontal"
          className="nd-settings-tabs"
          data-compact={compact ? "true" : undefined}
          onKeyDown={onTabKeyDown}
        >
          {SETTINGS_WINDOW_TABS.map((t) => {
            const selected = t === tab;
            const pending = tabMissing(readiness, t).length;
            return (
              <button
                key={t}
                ref={(el) => {
                  if (el === null) tabRefs.current.delete(t);
                  else tabRefs.current.set(t, el);
                }}
                type="button"
                role="tab"
                id={tabId(t)}
                aria-selected={selected}
                aria-controls={panelId}
                tabIndex={selected ? 0 : -1}
                className="nd-settings-tab"
                data-settings-tab={t}
                title={compact ? SETTINGS_TAB_LABELS[t] : undefined}
                onClick={() => choose(t)}
              >
                {compact ? <Icon name={SETTINGS_TAB_ICONS[t]} size={16} /> : null}
                <span className={compact ? "nd-sr-only" : "nd-settings-tab-label"}>{SETTINGS_TAB_LABELS[t]}</span>
                {pending > 0 ? (
                  <span className="nd-settings-tab-count" data-pending={pending}>
                    {pending}
                    <span className="nd-sr-only">{` ${trp("settings.tabPending", pending)}`}</span>
                  </span>
                ) : null}
              </button>
            );
          })}
        </div>
      </WindowTools>
      <section role="tabpanel" id={panelId} aria-labelledby={tabId(tab)} className="nd-settings-panel nd-app-scroll" tabIndex={-1}>
        {/* A aba já nomeia a seção: o título fica só para o leitor de tela (sem rótulo repetido). */}
        <h2 className="nd-sr-only">{SETTINGS_TAB_LABELS[tab]}</h2>
        <TabMissing tab={tab} />
        {body}
      </section>
    </div>
  );
});

function SettingsContentView({ size }: AppContentProps) {
  const space = useSpace();
  return <SettingsPanel size={size} savedTab={space?.selection.settingsTab} setSelection={space?.setSelection ?? null} />;
}

export const SettingsContent = memo(SettingsContentView);
