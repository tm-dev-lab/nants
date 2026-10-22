/*
 * Estado da árvore do explorador (spec 066/T03): lista plana da raiz, pastas ignoradas carregadas
 * sob demanda (`dir=`), pastas expandidas e invalidação. O componente que usa este hook é remontado
 * (key) ao trocar de projeto ou de fonte, então o estado nunca atravessa essa fronteira.
 * C-08: a cada `refreshKey` novo a lista é relida e só as pastas ignoradas expandidas são relidas.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getProjectFiles } from "./api/client";
import type { ApiFailure, FileEntry, FileSource } from "./api/client";
import { buildTree, visibleRows, withAncestors } from "./fileTree";
import type { LoadedDir, TreeNode, TreeRow } from "./fileTree";

type ListState =
  | { status: "loading" }
  | { status: "error"; message: string; failure: ApiFailure; reason?: string }
  | { status: "ok"; entries: readonly FileEntry[]; truncated: boolean; taskId?: string; changed: ReadonlySet<string> };

/** Falha: `message` é a reserva; a tela mostra `failure` no idioma ativo (`apiFailureText`). */
type DirState = { status: "loading" } | { status: "error"; message: string; failure: ApiFailure };

export interface FileTreeModel {
  list: ListState;
  rows: TreeRow[];
  expanded: ReadonlySet<string>;
  dirStates: ReadonlyMap<string, DirState>;
  loaded: ReadonlyMap<string, LoadedDir>;
  showHidden: boolean;
  setShowHidden: (value: boolean) => void;
  /** Filtro "só alterados" (apenas em `source=task`). */
  onlyChanged: boolean;
  setOnlyChanged: (value: boolean) => void;
  toggle: (node: TreeNode) => void;
  expand: (node: TreeNode) => void;
  collapse: (node: TreeNode) => void;
}

export function useFileTree(projectId: string, source: FileSource, refreshKey: number): FileTreeModel {
  const [list, setList] = useState<ListState>({ status: "loading" });
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const [loaded, setLoaded] = useState<ReadonlyMap<string, LoadedDir>>(() => new Map());
  const [dirStates, setDirStates] = useState<ReadonlyMap<string, DirState>>(() => new Map());
  const [showHidden, setShowHidden] = useState(false);
  const [onlyChanged, setOnlyChangedState] = useState(false);
  const listSeq = useRef(0);
  const dirSeq = useRef(new Map<string, number>());
  const expandedRef = useRef(expanded);
  expandedRef.current = expanded;
  const loadedRef = useRef(loaded);
  loadedRef.current = loaded;

  const loadDir = useCallback(
    async (path: string, background: boolean): Promise<void> => {
      const seq = (dirSeq.current.get(path) ?? 0) + 1;
      dirSeq.current.set(path, seq);
      if (!background) {
        setDirStates((prev) => new Map(prev).set(path, { status: "loading" }));
      }
      const result = await getProjectFiles(projectId, source, path);
      if (dirSeq.current.get(path) !== seq) return; // resposta superada
      if (result.ok) {
        setLoaded((prev) => new Map(prev).set(path, { entries: result.data.entries, truncated: result.data.truncated }));
        setDirStates((prev) => {
          const next = new Map(prev);
          next.delete(path);
          return next;
        });
      } else if (!background || !loadedRef.current.has(path)) {
        setDirStates((prev) => new Map(prev).set(path, { status: "error", message: result.error.message, failure: result.error }));
      }
    },
    [projectId, source],
  );

  // Lista da raiz: carga inicial e a cada invalidação (relê também as pastas ignoradas expandidas).
  const firstLoad = useRef(true);
  useEffect(() => {
    const seq = ++listSeq.current;
    const initial = firstLoad.current;
    firstLoad.current = false;
    void getProjectFiles(projectId, source).then((result) => {
      if (listSeq.current !== seq) return;
      if (result.ok) {
        setList({
          status: "ok",
          entries: result.data.entries,
          truncated: result.data.truncated,
          ...(result.data.taskId === undefined ? {} : { taskId: result.data.taskId }),
          changed: new Set(result.data.changed ?? []),
        });
      } else {
        const failure = {
          status: "error" as const,
          message: result.error.message,
          failure: result.error,
          ...(result.error.reason === undefined ? {} : { reason: result.error.reason }),
        };
        // 409 no_active_task é definitivo para esta fonte; outras falhas de fundo mantêm a lista.
        if (initial || failure.reason === "no_active_task") setList(failure);
        else setList((prev) => (prev.status === "ok" ? prev : failure));
      }
    });
    if (!initial) {
      for (const path of expandedRef.current) {
        if (loadedRef.current.has(path)) void loadDir(path, true);
      }
    }
  }, [projectId, source, refreshKey, loadDir]);

  const changed = list.status === "ok" ? list.changed : null;
  const filter = source === "task" && onlyChanged && changed !== null ? changed : undefined;
  const roots = useMemo(() => (list.status === "ok" ? buildTree(list.entries) : []), [list]);
  const rows = useMemo(
    () => visibleRows(roots, expanded, showHidden, loaded, filter),
    [roots, expanded, showHidden, loaded, filter],
  );

  const setOnlyChanged = useCallback(
    (value: boolean): void => {
      setOnlyChangedState(value);
      if (value && changed !== null) {
        // Abre as pastas que levam aos alterados, para o filtro mostrar os arquivos e não só as raízes.
        const ancestors = withAncestors(changed);
        setExpanded((prev) => {
          const next = new Set(prev);
          for (const path of ancestors) if (!changed.has(path)) next.add(path);
          return next;
        });
      }
    },
    [changed],
  );

  const expand = useCallback(
    (node: TreeNode): void => {
      if (node.kind !== "dir") return;
      setExpanded((prev) => (prev.has(node.path) ? prev : new Set(prev).add(node.path)));
      if (node.lazy && !loadedRef.current.has(node.path)) void loadDir(node.path, false);
    },
    [loadDir],
  );
  const collapse = useCallback((node: TreeNode): void => {
    setExpanded((prev) => {
      if (!prev.has(node.path)) return prev;
      const next = new Set(prev);
      next.delete(node.path);
      return next;
    });
  }, []);
  const toggle = useCallback(
    (node: TreeNode): void => {
      if (expandedRef.current.has(node.path)) collapse(node);
      else expand(node);
    },
    [collapse, expand],
  );

  return { list, rows, expanded, dirStates, loaded, showHidden, setShowHidden, onlyChanged, setOnlyChanged, toggle, expand, collapse };
}
