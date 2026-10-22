/*
 * Preços por modelo (aba Preços; specs 031/032/092): um cartão
 * por modelo em uso com entrada, saída e cache (opcional) em US$/1M tokens e o "US$ por imagem"
 * (092), selo estimado/manual/sem preço, "Usar estimativa" por modelo e "Atualizar estimativas".
 * Estimativas vêm do catálogo público (servidor); preço digitado é "manual" e o servidor nunca o
 * sobrescreve. Linha sem entrada e saída fica fora do mapa. Nunca inventa valores.
 *
 * Campo com unidade: o anel de foco fica no contêiner (`:focus-within`), não no campo.
 */
import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { autofillPricing, getPriceEstimate, putPricing } from "../../api/client";
import type { Agents, ModelPricing } from "../../api/types";
import { Icon } from "../../desktop/Icon";
import { t } from "../../i18n";
import { useT } from "../../i18n/react";
import { apiFailureText } from "../../i18n/serverCodes";
import { appStore } from "../../store";
import {
  buildPricing,
  EMPTY_PRICE_ROW,
  hasPrice,
  invalidPriceMessage,
  priceBadge,
  priceRowOf,
  priceRowsOf,
  type PriceRow,
  type PriceRows,
} from "./settingsModel";

interface PricingEditorProps {
  agents: Agents;
  pricing: Record<string, ModelPricing> | undefined;
  /** Preços salvos (a prontidão relê o nível recomendado). */
  onSaved?: () => void;
}

/** Mensagem da aba, montada na hora de exibir (no idioma ativo). */
type Message = () => string;

const FETCH_FAIL: Message = () => t("settings.pricing.fetchFailed");

function PriceField({ label, aria, value, unit, onChange }: { label: string; aria: string; value: string; unit: string; onChange: (v: string) => void }) {
  const id = useId();
  return (
    <div className="nd-field nd-price-field">
      <label htmlFor={id} className="nd-field-label">
        {label}
      </label>
      <span className="nd-affix">
        <input id={id} inputMode="decimal" aria-label={aria} value={value} onChange={(e) => onChange(e.target.value)} autoComplete="off" />
        <span aria-hidden="true" className="nd-affix-unit">
          {unit}
        </span>
      </span>
    </div>
  );
}

export function PricingEditor({ agents, pricing, onSaved }: PricingEditorProps) {
  const headingId = useId();
  const newModelId = useId();
  const [rows, setRows] = useState<PriceRows>(() => priceRowsOf(pricing));
  const [extra, setExtra] = useState<string[]>([]);
  const [newModel, setNewModel] = useState("");
  const { t: tr } = useT();
  const [message, setMessage] = useState<Message | null>(null);
  const [busy, setBusy] = useState(false);
  /** Modelos com estimativa no catálogo; ausente = ainda não consultado (botão visível). */
  const [avail, setAvail] = useState<Record<string, boolean>>({});
  const alive = useRef(true);
  const inUse = [...new Set([agents.architect.model, agents.developer.model, agents.reviewer.model])].filter((m) => m !== "");
  const models = [...new Set([...inUse, ...Object.keys(rows), ...extra])].filter((m) => m !== "");

  /** Aplica o mapa do servidor: só substitui linhas vazias ou já estimadas (nunca edição manual). */
  function mergeServer(data: Record<string, ModelPricing>): void {
    setRows((current) => {
      const next = { ...current };
      for (const [model, p] of Object.entries(data)) {
        if (typeof p?.input_per_mtok !== "number" || typeof p.output_per_mtok !== "number") continue;
        const cur = current[model];
        if (cur === undefined || !hasPrice(cur) || cur.source === "estimated") {
          // O preço por imagem digitado (ainda não salvo) sobrevive à estimativa nova.
          const row = priceRowOf(p);
          next[model] = row.image === "" && cur !== undefined ? { ...row, image: cur.image } : row;
        }
      }
      return next;
    });
    appStore.setState((s) => (s.config === null ? s : { config: { ...s.config, pricing: data } }));
  }

  /** Consulta quais modelos têm estimativa (sem gravar). */
  function checkAvail(names: string[]): void {
    if (names.length === 0) return;
    void getPriceEstimate(names).then((r) => {
      if (!alive.current || !r.ok) return;
      setAvail((a) => ({ ...a, ...Object.fromEntries(names.map((n) => [n, r.data[n] !== undefined])) }));
    });
  }

  // Ao abrir: pré-cadastra a estimativa dos modelos em uso (silencioso se o catálogo não responder).
  useEffect(() => {
    alive.current = true;
    checkAvail(models);
    if (inUse.length > 0) {
      void autofillPricing(false).then((r) => {
        if (!alive.current) return;
        if (r.ok) mergeServer(r.data);
        else setMessage(() => FETCH_FAIL);
      });
    }
    return () => {
      alive.current = false;
    };
    // Só na abertura da aba.
  }, []);

  function edit(model: string, field: "input" | "output" | "cached" | "image", value: string): void {
    setRows((r) => {
      const base: PriceRow = r[model] ?? EMPTY_PRICE_ROW;
      // O preço por imagem é do usuário e não muda a origem dos preços de tokens.
      if (field === "image") return { ...r, [model]: { ...base, image: value } };
      const { estimatedAt: _drop, ...rest } = base;
      return { ...r, [model]: { ...rest, [field]: value, source: "manual" } };
    });
  }

  function addModel(): void {
    const name = newModel.trim();
    if (name === "") return;
    setExtra((e) => (e.includes(name) ? e : [...e, name]));
    checkAvail([name]);
    setNewModel("");
  }

  async function useEstimate(model: string): Promise<void> {
    setBusy(true);
    const r = await getPriceEstimate([model]);
    if (!alive.current) return;
    setBusy(false);
    const found = r.ok ? r.data[model] : undefined;
    if (found === undefined) {
      const failure = r.ok ? null : r.error;
      setMessage(() => () => (failure === null ? t("settings.pricing.noEstimate", { model }) : apiFailureText(failure)));
      return;
    }
    setRows((cur) => ({ ...cur, [model]: { ...priceRowOf(found), image: cur[model]?.image ?? "" } }));
    setMessage(() => () => t("settings.pricing.estimateApplied", { model }));
  }

  async function refresh(): Promise<void> {
    setBusy(true);
    const r = await autofillPricing(true);
    if (!alive.current) return;
    setBusy(false);
    if (!r.ok) {
      setMessage(() => FETCH_FAIL);
      return;
    }
    mergeServer(r.data);
    setMessage(() => () => t("settings.pricing.refreshed"));
  }

  async function save(event: FormEvent): Promise<void> {
    event.preventDefault();
    const built = buildPricing(models, rows);
    if (!built.ok) {
      const model = built.model;
      setMessage(() => () => invalidPriceMessage(model));
      return;
    }
    const r = await putPricing(built.pricing);
    if (!alive.current) return;
    if (!r.ok) {
      const failure = r.error;
      setMessage(() => () => apiFailureText(failure));
      return;
    }
    appStore.setState((s) => (s.config === null ? s : { config: { ...s.config, pricing: r.data } }));
    setMessage(() => () => t("settings.pricing.saved"));
    onSaved?.();
  }

  return (
    <section aria-labelledby={headingId} className="nd-form" data-pricing-editor="">
      <div className="nd-row nd-row-between">
        <h3 id={headingId} className="nd-strong">
          {tr("settings.pricing.title")}
        </h3>
        <button type="button" className="nd-btn nd-btn-sm" disabled={busy} onClick={() => void refresh()}>
          <Icon name="refresh" size={14} />
          {tr("settings.pricing.refresh")}
        </button>
      </div>
      <p className="nd-field-hint">{tr("settings.pricing.hint")}</p>
      <form onSubmit={(e) => void save(e)} className="nd-form">
        <ul className="nd-price-list">
          {models.map((model) => {
            const badge = priceBadge(rows[model]);
            const row = rows[model];
            return (
              <li key={model} data-model={model} className="nd-price-card">
                <div className="nd-row nd-row-between">
                  <span className="nd-strong nd-break">{model}</span>
                  <span className="nd-chip-static" data-tone={badge.tone}>
                    {badge.text}
                  </span>
                </div>
                <div className="nd-price-grid">
                  <PriceField label={tr("settings.pricing.input")} aria={tr("settings.pricing.inputOf", { model })} unit="US$/1M" value={row?.input ?? ""} onChange={(v) => edit(model, "input", v)} />
                  <PriceField label={tr("settings.pricing.output")} aria={tr("settings.pricing.outputOf", { model })} unit="US$/1M" value={row?.output ?? ""} onChange={(v) => edit(model, "output", v)} />
                  <PriceField label={tr("settings.pricing.cached")} aria={tr("settings.pricing.cachedOf", { model })} unit="US$/1M" value={row?.cached ?? ""} onChange={(v) => edit(model, "cached", v)} />
                  <PriceField
                    label={tr("settings.pricing.image")}
                    aria={tr("settings.pricing.imageOf", { model })}
                    unit="US$"
                    value={row?.image ?? ""}
                    onChange={(v) => edit(model, "image", v)}
                  />
                </div>
                {avail[model] === false ? (
                  <p className="nd-muted">{tr("settings.pricing.noCatalog")}</p>
                ) : (
                  <button type="button" className="nd-btn nd-btn-sm nd-self-start" disabled={busy} aria-label={tr("settings.pricing.useEstimateOf", { model })} onClick={() => void useEstimate(model)}>
                    <Icon name="sparkles" size={14} />
                    {tr("settings.pricing.useEstimate")}
                  </button>
                )}
              </li>
            );
          })}
        </ul>
        <div className="nd-row nd-price-add">
          <div className="nd-field nd-grow">
            <label htmlFor={newModelId} className="nd-field-label">
              {tr("settings.pricing.addModel")}
            </label>
            <input id={newModelId} className="nd-input" value={newModel} onChange={(e) => setNewModel(e.target.value)} autoComplete="off" spellCheck={false} />
          </div>
          <button type="button" className="nd-btn" onClick={addModel}>
            <Icon name="plus" size={14} />
            {tr("settings.pricing.add")}
          </button>
          <button type="submit" className="nd-btn" data-variant="primary">
            <Icon name="save" size={14} />
            {tr("settings.pricing.save")}
          </button>
        </div>
        {message !== null ? (
          <p role="status" className="nd-muted">
            {message()}
          </p>
        ) : null}
      </form>
    </section>
  );
}
