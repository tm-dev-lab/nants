/* Janela Plano (spec 093, T06). */
/*
 * Seção "Ambiente" do PlanCard (spec 030/T02): modo, serviços, imagem base e arquivos aprovados;
 * premissas, perfis e orientações. Desde a 095/T04 tudo isso fica no `<details>` recolhido do
 * plano: cada teste abre o `summary` antes de procurar o conteúdo (C-11).
 */
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { PlanCard } from "./PlanCard";
import { makeSpec, makeTask, resetStore } from "../../test-utils";

afterEach(cleanup);

/** Abre o `<details>` do plano pelo `summary` (começa recolhido) e devolve o rótulo do resumo. */
function openMore(): string {
  const details = document.querySelector<HTMLDetailsElement>("[data-plan-more]");
  if (details === null) throw new Error("detalhes ausentes");
  expect(details.open).toBe(false);
  const summary = details.querySelector("summary");
  if (summary === null) throw new Error("summary ausente");
  fireEvent.click(summary);
  details.open = true;
  return summary.textContent ?? "";
}

describe("PlanCard: ambiente", () => {
  it("mostra serviços, imagem base e arquivos quando o plano propõe docker", () => {
    resetStore({});
    const spec = {
      ...makeSpec("s1", "API .NET", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]),
      environment: {
        mode: "docker" as const,
        services: ["workspace", "db"],
        base_image: "mcr.microsoft.com/dotnet/sdk:8.0",
        files: [{ path: ".nants/compose.yaml", content: "services:\n  workspace: {}\n" }],
      },
    };
    render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
    expect(openMore()).toBe("Ambiente e checagens");
    expect(screen.getByRole("heading", { name: "Ambiente" })).toBeTruthy();
    expect(screen.getByText("workspace, db")).toBeTruthy();
    expect(screen.getByText("mcr.microsoft.com/dotnet/sdk:8.0")).toBeTruthy();
    expect(screen.getByText(".nants/compose.yaml")).toBeTruthy();
  });

  it("não mostra a seção sem ambiente", () => {
    resetStore({});
    const spec = makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]);
    render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
    expect(screen.queryByRole("heading", { name: "Ambiente" })).toBeNull();
  });
});

describe("PlanCard: premissas", () => {
  it("lista premissas quando existem", () => {
    resetStore({});
    const spec = { ...makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]), premises: ["SQLite local"] };
    render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
    expect(openMore()).toBe("Premissas e checagens");
    expect(screen.getByRole("heading", { name: "Premissas" })).toBeTruthy();
    expect(screen.getByText("SQLite local")).toBeTruthy();
  });
});

describe("PlanCard: perfis de especialista", () => {
  const base = { stack: ["clojure"], updated_at: "2026-10-16" };
  it("expande perfis criados ou alterados com o motivo e recolhe os reaproveitados", () => {
    resetStore({});
    const spec = {
      ...makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]),
      skills: [
        { ...base, role: "developer", status: "atualizado", reason: "stack mudou para shadow-cljs", text: "- usa deps.edn" },
        { ...base, role: "reviewer", status: "reaproveitado", text: "- cheque cljfmt" },
      ],
    };
    render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
    expect(openMore()).toBe("Perfis e checagens");
    expect(screen.getByRole("heading", { name: "Perfis de especialista" })).toBeTruthy();
    expect(screen.getByTestId("skill-developer").hasAttribute("open")).toBe(true);
    expect(screen.getByTestId("skill-reviewer").hasAttribute("open")).toBe(false);
    expect(screen.getByText("Motivo: stack mudou para shadow-cljs")).toBeTruthy();
    // O perfil é Markdown renderizado (094/T08, A-08): o item vira lista, sem o "- " cru.
    expect(screen.getByText("usa deps.edn").closest("li")).not.toBeNull();
    expect(screen.queryByText("- usa deps.edn")).toBeNull();
    expect(screen.getByText(/Desenvolvedor · atualizado/)).toBeTruthy();
    expect(screen.getByText(/Revisor · reaproveitado/)).toBeTruthy();
  });

  it("expande o perfil criado e não mostra a seção sem perfis", () => {
    resetStore({});
    const spec = {
      ...makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]),
      skills: [{ ...base, role: "developer", status: "criado", text: "- x" }],
    };
    const { unmount } = render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
    openMore();
    expect(screen.getByTestId("skill-developer").hasAttribute("open")).toBe(true);
    unmount();
    render(<PlanCard projectId="p1" spec={makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")])} onApproved={async () => {}} />);
    expect(screen.queryByText("Perfis de especialista")).toBeNull();
  });
});

describe("PlanCard: orientações por papel", () => {
  it("mostra as orientações recolhidas por papel", () => {
    resetStore({});
    const spec = {
      ...makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]),
      role_guidance: [
        { role: "Desenvolvedor", text: "Rode lein test" },
        { role: "Revisor", text: "Cheque o .gitignore" },
      ],
    };
    const { container } = render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
    const details = container.querySelector("details");
    expect(details).not.toBeNull();
    expect(details?.hasAttribute("open")).toBe(false);
    expect(openMore()).toBe("Orientações e checagens");
    expect(screen.getByText("Orientações por papel")).toBeTruthy();
    expect(screen.getByText("Rode lein test")).toBeTruthy();
    expect(screen.getByText("Cheque o .gitignore")).toBeTruthy();
  });

  it("A-08: orientações só com texto vazio não mostram a seção; o resumo tem o chevron", () => {
    resetStore({});
    const empty = {
      ...makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]),
      role_guidance: [{ role: "Desenvolvedor", text: "  " }],
    };
    const { unmount } = render(<PlanCard projectId="p1" spec={empty} onApproved={async () => {}} />);
    expect(screen.queryByText("Orientações por papel")).toBeNull();
    unmount();
    const spec = { ...empty, role_guidance: [{ role: "Desenvolvedor", text: "" }, { role: "Revisor", text: "Cheque o lint" }] };
    const { container } = render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
    // O chevron fica no resumo do detalhe; dentro, as orientações não são outra caixa (095/T04).
    expect(container.querySelector("[data-plan-more] > summary .nd-details-chevron")).not.toBeNull();
    openMore();
    expect(screen.getByText("Orientações por papel").closest("details")?.hasAttribute("data-plan-more")).toBe(true);
    expect(container.querySelectorAll("[data-plan-guidance] h4")).toHaveLength(1);
  });

  it("A-08: perfil de especialista com títulos e listas em Markdown, não cru", () => {
    resetStore({});
    const spec = {
      ...makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]),
      skills: [{ stack: ["js"], updated_at: "2026-10-16", role: "developer", status: "criado", text: "Você é especialista.\n\n## Implementação\n- Em src/, funções puras" }],
    };
    render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
    openMore();
    const skill = screen.getByTestId("skill-developer");
    expect(skill.textContent).not.toContain("## ");
    expect(skill.querySelector("[data-skill-text] li")?.textContent).toBe("Em src/, funções puras");
  });

  it("não mostra a seção sem orientações", () => {
    resetStore({});
    const spec = makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]);
    render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
    expect(screen.queryByText("Orientações por papel")).toBeNull();
  });
});
