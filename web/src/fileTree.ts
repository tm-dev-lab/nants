/*
 * Árvore do explorador de arquivos (spec 066/T03), montada no cliente a partir da lista plana do
 * servidor. Funções puras: pastas primeiro, ordem alfabética, dotfiles opcionais e linhas visíveis
 * conforme as pastas expandidas. Pastas ignoradas pelo git (e o que há dentro delas) são "lazy":
 * os filhos vêm de `dir=` ao expandir, guardados em `loaded`.
 */
import type { FileEntry } from "./api/client";

type TreeKind = "dir" | "file" | "symlink";

export interface TreeNode {
  name: string;
  /** Caminho completo a partir da raiz. */
  path: string;
  kind: TreeKind;
  /** Pasta ignorada pelo git (ou dentro de uma): filhos carregados sob demanda. */
  lazy: boolean;
  /** Raiz de pasta ignorada pelo git (recebe o rótulo "ignorado pelo git"). */
  ignoredRoot: boolean;
  children: TreeNode[];
}

/** Filhos já carregados por pasta lazy (`dir=`), com o aviso de teto. */
export interface LoadedDir {
  entries: readonly FileEntry[];
  truncated: boolean;
}

export interface TreeRow {
  node: TreeNode;
  level: number;
}

function baseName(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? path : path.slice(i + 1);
}

function compareNodes(a: TreeNode, b: TreeNode): number {
  const da = a.kind === "dir" ? 0 : 1;
  const db = b.kind === "dir" ? 0 : 1;
  if (da !== db) return da - db;
  const la = a.name.toLowerCase();
  const lb = b.name.toLowerCase();
  if (la !== lb) return la < lb ? -1 : 1;
  return a.name < b.name ? -1 : a.name > b.name ? 1 : 0;
}

function sortDeep(nodes: TreeNode[]): void {
  nodes.sort(compareNodes);
  for (const node of nodes) sortDeep(node.children);
}

/** Monta a árvore da lista plana (`git ls-files` + pastas ignoradas), sem filtrar nada. */
export function buildTree(entries: readonly FileEntry[]): TreeNode[] {
  const roots: TreeNode[] = [];
  const dirs = new Map<string, TreeNode>();

  const ensureDir = (path: string): TreeNode => {
    const existing = dirs.get(path);
    if (existing !== undefined) return existing;
    const node: TreeNode = { name: baseName(path), path, kind: "dir", lazy: false, ignoredRoot: false, children: [] };
    dirs.set(path, node);
    const i = path.lastIndexOf("/");
    (i < 0 ? roots : ensureDir(path.slice(0, i)).children).push(node);
    return node;
  };

  for (const entry of entries) {
    const path = entry.path;
    if (path === "" || path.startsWith("/") || path.split("/").includes("..")) continue;
    if (entry.kind === "ignored_dir" || entry.kind === "dir") {
      const dir = ensureDir(path);
      if (entry.kind === "ignored_dir") {
        dir.lazy = true;
        dir.ignoredRoot = true;
      }
      continue;
    }
    const i = path.lastIndexOf("/");
    const parent = i < 0 ? roots : ensureDir(path.slice(0, i)).children;
    parent.push({
      name: baseName(path),
      path,
      kind: entry.kind === "symlink" ? "symlink" : "file",
      lazy: false,
      ignoredRoot: false,
      children: [],
    });
  }
  sortDeep(roots);
  return roots;
}

/** Filhos de uma pasta lazy, a partir da listagem `dir=`. Subpastas continuam lazy. */
export function lazyChildren(dir: TreeNode, loaded: LoadedDir): TreeNode[] {
  const prefix = `${dir.path}/`;
  const out: TreeNode[] = [];
  for (const entry of loaded.entries) {
    if (!entry.path.startsWith(prefix) || entry.path.length === prefix.length) continue;
    const rest = entry.path.slice(prefix.length);
    if (rest.includes("/") || rest === "..") continue;
    out.push({
      name: rest,
      path: entry.path,
      kind: entry.kind === "dir" || entry.kind === "ignored_dir" ? "dir" : entry.kind === "symlink" ? "symlink" : "file",
      lazy: true,
      ignoredRoot: false,
      children: [],
    });
  }
  out.sort(compareNodes);
  return out;
}

function isHiddenName(name: string): boolean {
  return name.startsWith(".");
}

/** Linhas visíveis em ordem de exibição, respeitando expansão, ocultos e carga das pastas lazy. */
export function visibleRows(
  roots: readonly TreeNode[],
  expanded: ReadonlySet<string>,
  showHidden: boolean,
  loaded: ReadonlyMap<string, LoadedDir>,
  onlyChanged?: ReadonlySet<string>,
): TreeRow[] {
  const rows: TreeRow[] = [];
  const keep = onlyChanged === undefined ? null : withAncestors(onlyChanged);
  const walk = (nodes: readonly TreeNode[], level: number): void => {
    for (const node of nodes) {
      if (!showHidden && isHiddenName(node.name)) continue;
      if (keep !== null && !keep.has(node.path)) continue;
      rows.push({ node, level });
      if (node.kind !== "dir" || !expanded.has(node.path)) continue;
      if (node.lazy) {
        const data = loaded.get(node.path);
        if (data !== undefined) walk(lazyChildren(node, data), level + 1);
      } else {
        walk(node.children, level + 1);
      }
    }
  };
  walk(roots, 1);
  return rows;
}

/** Pasta pai de um caminho (`null` na raiz). */
export function parentPath(path: string): string | null {
  const i = path.lastIndexOf("/");
  return i < 0 ? null : path.slice(0, i);
}

/** Os caminhos e todas as pastas ancestrais deles (para o filtro "só alterados"). */
export function withAncestors(paths: ReadonlySet<string>): Set<string> {
  const out = new Set<string>();
  for (const path of paths) {
    let current: string | null = path;
    while (current !== null && !out.has(current)) {
      out.add(current);
      current = parentPath(current);
    }
  }
  return out;
}
