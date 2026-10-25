/* Markdown restrito do relatório e das mensagens: elementos semânticos e texto do agente nunca vira HTML, link nem imagem. */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, within } from "@testing-library/react";
import { ReportMarkdown } from "./ReportMarkdown";
import { parseReportMarkdown } from "../../reportMarkdown";
import { cssRuleBody, cssRules } from "../../desktop/test-utils";

afterEach(() => {
  cleanup();
});

function renderMarkdown(markdown: string) {
  return render(<ReportMarkdown blocks={parseReportMarkdown(markdown)} />);
}

describe("ReportMarkdown: elementos", () => {
  it("renderiza títulos, listas (com numeração), código e ênfase com elementos semânticos", () => {
    const { container } = renderMarkdown(
      "## Passos\n\n3. Instale `npm ci`\n4. **Rode** *tudo*\n\n- sub\n\n```bash\nnpm test\n```",
    );
    expect(container.querySelector("h5")?.textContent).toBe("Passos");
    const ol = container.querySelector("ol");
    expect(ol).not.toBeNull();
    expect(Array.from(ol?.querySelectorAll("li") ?? []).map((li) => li.value)).toEqual([3, 4]);
    expect(ol?.querySelector("code")?.textContent).toBe("npm ci");
    expect(ol?.querySelector("strong")?.textContent).toBe("Rode");
    expect(ol?.querySelector("em")?.textContent).toBe("tudo");
    expect(container.querySelector("ul")?.textContent).toBe("sub");
    expect(container.querySelector("pre code")?.textContent).toBe("npm test");
  });

  it("lista aninhada aparece dentro do item", () => {
    const { container } = renderMarkdown("1. Instale\n   - npm ci\n2. Rode");
    const outerItem = container.querySelector("ol > li");
    expect(outerItem?.querySelector("ul li")?.textContent).toBe("npm ci");
  });

  it("código de bloco é texto puro dentro de pre/code", () => {
    const { container } = renderMarkdown("```\n<b>x</b>\n```");
    expect(container.querySelector("pre code")?.textContent).toBe("<b>x</b>");
    expect(container.querySelector("b")).toBeNull();
  });
});

describe("ReportMarkdown: segurança (texto do agente não é confiável)", () => {
  it("<script>, <img onerror> e HTML bruto viram texto, sem elementos", () => {
    const { container } = renderMarkdown(
      '<script>alert(1)</script>\n\n<img src=x onerror="alert(2)">\n\n<iframe src="https://x"></iframe>',
    );
    expect(container.querySelector("script, img, iframe, [onerror]")).toBeNull();
    expect(container.textContent).toContain("<script>alert(1)</script>");
    expect(container.textContent).toContain('<img src=x onerror="alert(2)">');
  });

  it("link javascript: não vira link nem URL", () => {
    const { container } = renderMarkdown("[x](javascript:alert(1))");
    expect(container.querySelector("a")).toBeNull();
    expect(container.textContent).toBe("x");
  });

  it("imagem não gera <img>; fica o texto alternativo", () => {
    const { container } = renderMarkdown("![i](http://x)");
    expect(container.querySelector("img")).toBeNull();
    expect(container.textContent).toBe("i");
  });

  it("links http(s) não são clicáveis (nenhum <a> no relatório)", () => {
    const { container } = renderMarkdown("Abra [o app](http://localhost:3000) agora.");
    expect(container.querySelector("a")).toBeNull();
    expect(container.textContent).toContain("o app (http://localhost:3000)");
  });

  it("HTML dentro de bloco de código fica literal", () => {
    const { container } = renderMarkdown("```html\n<script>alert(3)</script>\n```");
    expect(container.querySelector("script")).toBeNull();
    expect(container.querySelector("pre code")?.textContent).toBe("<script>alert(3)</script>");
  });

  it("markdown muito longo renderiza sem quebrar", () => {
    const big = "- item de relatório com `código` e texto\n".repeat(1500);
    const { container } = renderMarkdown(big);
    const region = within(container);
    expect(region.getAllByRole("listitem")).toHaveLength(1500);
  });
});

describe("ReportMarkdown: tabelas (spec 126 T04)", () => {
  it("tabela GFM vira table com thead, tbody, th de coluna e alinhamento por classe", () => {
    const { container } = renderMarkdown("| Task | **Area** | Custo |\n|---|:-:|--:|\n| T01 | `infra` | 3 |\n| T02 | world |");
    const wrap = container.querySelector(".nd-md > .nd-md-table-wrap");
    expect(wrap).not.toBeNull();
    const table = wrap?.querySelector("table.nd-md-table");
    const heads = Array.from(table?.querySelectorAll("thead th") ?? []);
    expect(heads.map((th) => th.textContent)).toEqual(["Task", "Area", "Custo"]);
    expect(heads.every((th) => th.getAttribute("scope") === "col")).toBe(true);
    expect(heads[1]?.querySelector("strong")?.textContent).toBe("Area");
    expect(heads.map((th) => th.className)).toEqual(["", "nd-md-al-center", "nd-md-al-right"]);
    const rows = Array.from(table?.querySelectorAll("tbody tr") ?? []);
    expect(rows.map((tr) => Array.from(tr.querySelectorAll("td")).map((td) => td.textContent))).toEqual([
      ["T01", "infra", "3"],
      ["T02", "world", ""],
    ]);
    expect(rows[0]?.querySelector("td code")?.textContent).toBe("infra");
    expect(rows[0]?.querySelectorAll("td")[2]?.className).toBe("nd-md-al-right");
  });

  it("tabela sem corpo não gera tbody; HTML na célula fica literal", () => {
    const { container } = renderMarkdown("| <b>x</b> |\n|---|");
    expect(container.querySelector("tbody")).toBeNull();
    expect(container.querySelector("b")).toBeNull();
    expect(container.querySelector("th")?.textContent).toBe("<b>x</b>");
  });

  it("no celular a moldura rola na horizontal, sem alargar a página", () => {
    const css = readFileSync(resolve(process.cwd(), "src/apps/shared/apps.css"), "utf8");
    const wrap = cssRuleBody(css, ".nd-md-table-wrap");
    expect(wrap).toContain("overflow-x: auto;");
    expect(wrap).toContain("max-width: 100%;");
    expect(wrap).toContain("min-width: 0;");
    // A palavra não quebra no meio: a largura mínima da coluna é a maior palavra, e o excedente rola.
    // Os seletores levam `.nd-md` para vencer `.nd-content[data-size="narrow"] :where(td, table)`.
    const cell = cssRules(css).find((r) => r.at === null && r.selectors.includes(".nd-md .nd-md-table td"))?.body ?? "";
    expect(cell).toContain("overflow-wrap: normal;");
    expect(cell).toContain("word-break: normal;");
    expect(cssRuleBody(css, ".nd-md .nd-md-table")).toContain("max-width: none;");
    // O alinhamento da coluna é mais específico que o `text-align: left` da célula.
    expect(cssRuleBody(css, ".nd-md .nd-md-table .nd-md-al-right")).toContain("text-align: right;");
    expect(cssRuleBody(css, ".nd-md .nd-md-table .nd-md-al-center")).toContain("text-align: center;");
    expect(cssRuleBody(css, ".nd-md")).toContain("min-width: 0;");
  });
});
