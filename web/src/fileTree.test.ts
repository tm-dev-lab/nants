import { describe, expect, it } from "vitest";
import type { FileEntry } from "./api/client";
import { buildTree, lazyChildren, visibleRows } from "./fileTree";
import type { LoadedDir } from "./fileTree";

const flat: FileEntry[] = [
  { path: "README.md", kind: "file" },
  { path: "src/main.rs", kind: "file" },
  { path: "src/lib.rs", kind: "file" },
  { path: "src/util/b.rs", kind: "file" },
  { path: "Zeta.txt", kind: "file" },
  { path: ".env.example", kind: "file" },
  { path: ".github/ci.yml", kind: "file" },
  { path: "link", kind: "symlink" },
  { path: "target", kind: "ignored_dir" },
  { path: "alpha/z.txt", kind: "file" },
];

const none = new Map<string, LoadedDir>();
const names = (rows: ReturnType<typeof visibleRows>) => rows.map((r) => `${r.level}:${r.node.name}`);

describe("buildTree", () => {
  it("monta pastas a partir dos caminhos, pastas primeiro e ordem alfabética", () => {
    const tree = buildTree(flat);
    expect(tree.map((n) => n.name)).toEqual([
      ".github",
      "alpha",
      "src",
      "target",
      ".env.example",
      "link",
      "README.md",
      "Zeta.txt",
    ]);
    const src = tree.find((n) => n.name === "src");
    expect(src?.children.map((n) => n.name)).toEqual(["util", "lib.rs", "main.rs"]);
  });

  it("ignored_dir vira pasta lazy rotulada; symlink é nó sem filhos", () => {
    const tree = buildTree(flat);
    const target = tree.find((n) => n.name === "target");
    expect(target).toMatchObject({ kind: "dir", lazy: true, ignoredRoot: true });
    expect(tree.find((n) => n.name === "link")).toMatchObject({ kind: "symlink", children: [] });
  });

  it("descarta caminhos suspeitos (absoluto ou com ..)", () => {
    const tree = buildTree([
      { path: "/etc/passwd", kind: "file" },
      { path: "a/../b", kind: "file" },
      { path: "ok.txt", kind: "file" },
    ]);
    expect(tree.map((n) => n.name)).toEqual(["ok.txt"]);
  });
});

describe("visibleRows", () => {
  const tree = buildTree(flat);

  it("oculta dotfiles por padrão e mostra com a opção", () => {
    expect(names(visibleRows(tree, new Set(), false, none))).toEqual([
      "1:alpha",
      "1:src",
      "1:target",
      "1:link",
      "1:README.md",
      "1:Zeta.txt",
    ]);
    expect(names(visibleRows(tree, new Set(), true, none))).toContain("1:.env.example");
  });

  it("só mostra filhos de pastas expandidas", () => {
    const rows = names(visibleRows(tree, new Set(["src"]), false, none));
    expect(rows).toEqual([
      "1:alpha",
      "1:src",
      "2:util",
      "2:lib.rs",
      "2:main.rs",
      "1:target",
      "1:link",
      "1:README.md",
      "1:Zeta.txt",
    ]);
  });

  it("pasta lazy expandida usa o que foi carregado (dir=) e sem carga não mostra filhos", () => {
    const expanded = new Set(["target"]);
    expect(names(visibleRows(tree, expanded, false, none))).not.toContain("2:debug");
    const loaded = new Map<string, LoadedDir>([
      [
        "target",
        {
          truncated: false,
          entries: [
            { path: "target/release", kind: "dir" },
            { path: "target/a.d", kind: "file" },
          ],
        },
      ],
    ]);
    const rows = visibleRows(tree, expanded, false, loaded);
    expect(names(rows)).toContain("2:release");
    expect(names(rows).indexOf("2:release")).toBeLessThan(names(rows).indexOf("2:a.d"));
    const target = tree.find((n) => n.name === "target");
    const kids = target === undefined ? [] : lazyChildren(target, loaded.get("target") as LoadedDir);
    expect(kids.every((k) => k.lazy)).toBe(true);
  });
});
