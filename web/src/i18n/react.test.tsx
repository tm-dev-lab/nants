import { act, cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { renderWith } from "../test-utils";
import { getLanguage, setLanguage } from "./index";
import { useLanguage, useT } from "./react";

function Sample() {
  const { t, tp } = useT();
  return (
    <div>
      <h1>{t("window.settings")}</h1>
      <p>{tp("notifications.count", 2)}</p>
    </div>
  );
}

afterEach(cleanup);

describe("useT e renderWith", () => {
  it("renderWith sem idioma usa pt-BR", () => {
    renderWith(<Sample />);
    expect(screen.getByRole("heading").textContent).toBe("Configurações");
    expect(screen.getByText("2 avisos")).toBeTruthy();
  });

  it("renderWith({ lang: 'en' }) renderiza em inglês", () => {
    renderWith(<Sample />, { lang: "en" });
    expect(getLanguage()).toBe("en");
    expect(screen.getByRole("heading").textContent).toBe("Settings");
    expect(screen.getByText("2 notices")).toBeTruthy();
  });

  it("o idioma volta a pt-BR no teste seguinte", () => {
    expect(getLanguage()).toBe("pt-BR");
  });

  it("useLanguage acompanha a troca", () => {
    function Current() {
      return <span>{useLanguage()}</span>;
    }
    renderWith(<Current />, { lang: "en" });
    expect(screen.getByText("en")).toBeTruthy();
    act(() => setLanguage("pt-BR", { persist: false }));
    expect(screen.getByText("pt-BR")).toBeTruthy();
  });

  it("troca ao vivo re-renderiza sem recarregar", () => {
    renderWith(<Sample />);
    expect(screen.getByRole("heading").textContent).toBe("Configurações");
    act(() => setLanguage("en", { persist: false }));
    expect(screen.getByRole("heading").textContent).toBe("Settings");
    act(() => setLanguage("pt-BR", { persist: false }));
    expect(screen.getByRole("heading").textContent).toBe("Configurações");
  });
});
