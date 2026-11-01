/*
 * A lista de modelos por conexão (saiu do `AgentsForm` na spec 139 T09): `GET .../models` da conexão
 * salva, uma carga por endpoint, compartilhada entre os campos que usam a mesma conexão. Serve aos
 * Agentes e aos Ajustes do Nanty.
 */
import { useEffect, useRef, useState } from "react";
import { getConnectionModels, type ApiResult } from "../../api/client";
import type { ConnectionView, ModelsResult } from "../../api/types";
import { t } from "../../i18n";
import { apiFailureText } from "../../i18n/serverCodes";
import { describeModelsError, MODELS_MESSAGES } from "../../settings/rules";
import type { ModelOption } from "./ModelCombobox";

export type ModelsEntry =
  | { status: "loading" }
  | { status: "ready"; models: ModelOption[]; truncated: boolean }
  | { status: "error"; message: string };

function endpointKey(connection: ConnectionView): string {
  return `${connection.id}|${connection.base_url}|${connection.api_key_masked}`;
}

function toModelsEntry(outcome: ApiResult<ModelsResult>): ModelsEntry {
  if (!outcome.ok) return { status: "error", message: t("settings.models.requestFailed", { message: apiFailureText(outcome.error) }) };
  const data = outcome.data;
  if (typeof data.error === "string") return { status: "error", message: describeModelsError(data.error, data.status) };
  const models = data.models.flatMap((model): ModelOption[] => {
    if (typeof model?.id !== "string") return [];
    return typeof model.name === "string" && model.name.trim() !== "" ? [{ id: model.id, name: model.name }] : [{ id: model.id }];
  });
  return { status: "ready", models, truncated: data.truncated === true };
}

/** A dica abaixo do campo do modelo, pelo estado da lista; `null` sem nada a dizer. */
export function modelsHint(entry: ModelsEntry | undefined, notListed = false): { text: string; warn: boolean } | null {
  if (entry?.status === "loading") return { text: MODELS_MESSAGES.loading, warn: false };
  if (entry?.status === "error") return { text: entry.message, warn: true };
  if (entry?.status === "ready" && entry.models.length === 0) return { text: MODELS_MESSAGES.empty, warn: false };
  if (entry?.status === "ready" && entry.truncated) return { text: MODELS_MESSAGES.truncated, warn: false };
  return notListed ? { text: MODELS_MESSAGES.notListed, warn: false } : null;
}

export interface ConnectionModels {
  /** A lista de uma conexão; `undefined` sem conexão ou antes da primeira carga. */
  entryOf: (connection: ConnectionView | undefined) => ModelsEntry | undefined;
  /** Carrega de novo; com `refresh`, o servidor ignora o cache dele. */
  load: (connection: ConnectionView, refresh: boolean) => void;
  /** Depois de escolher a conexão: uma nova tentativa, só se a lista dela tinha falhado. */
  retryFailed: (connection: ConnectionView) => void;
}

/**
 * Mantém a lista das conexões em uso (`active`, com repetição à vontade). `connections` são todas as
 * cadastradas: a conexão que mudou de endereço ou de chave perde a lista guardada.
 */
export function useConnectionModels(active: readonly ConnectionView[], connections: readonly ConnectionView[]): ConnectionModels {
  const [models, setModels] = useState<ReadonlyMap<string, ModelsEntry>>(() => new Map());
  const entriesRef = useRef<Map<string, ModelsEntry>>(new Map());
  const tokensRef = useRef<Map<string, number>>(new Map());
  const activeRef = useRef<Set<string>>(new Set());
  const mountedRef = useRef(true);

  const activeConns = new Map<string, ConnectionView>();
  for (const conn of active) activeConns.set(endpointKey(conn), conn);
  const savedKey = [...activeConns.keys()].sort().join("\n");

  function storeEntry(key: string, entry: ModelsEntry | null): void {
    if (entry === null) entriesRef.current.delete(key);
    else entriesRef.current.set(key, entry);
    setModels(new Map(entriesRef.current));
  }

  function load(conn: ConnectionView, refresh: boolean): void {
    const key = endpointKey(conn);
    const token = (tokensRef.current.get(key) ?? 0) + 1;
    tokensRef.current.set(key, token);
    storeEntry(key, { status: "loading" });
    void getConnectionModels(conn.id, { refresh }).then((outcome) => {
      if (!mountedRef.current || tokensRef.current.get(key) !== token) return;
      if (!activeRef.current.has(key)) {
        storeEntry(key, null);
        return;
      }
      storeEntry(key, toModelsEntry(outcome));
    });
  }

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    const current = new Set(savedKey === "" ? [] : savedKey.split("\n"));
    activeRef.current = current;
    const keyById = new Map(connections.map((c) => [c.id, endpointKey(c)]));
    for (const [key, entry] of [...entriesRef.current]) {
      const id = key.split("|")[0] ?? key;
      const stale = keyById.has(id) && keyById.get(id) !== key;
      const droppable = stale || (current.has(key) ? entry.status === "error" : entry.status !== "ready");
      if (droppable) entriesRef.current.delete(key);
    }
    setModels(new Map(entriesRef.current));
    for (const [key, conn] of activeConns) {
      if (!entriesRef.current.has(key)) load(conn, false);
    }
  }, [savedKey]);

  return {
    entryOf: (connection) => (connection === undefined ? undefined : models.get(endpointKey(connection))),
    load,
    retryFailed: (connection) => {
      if (entriesRef.current.get(endpointKey(connection))?.status === "error") load(connection, false);
    },
  };
}
