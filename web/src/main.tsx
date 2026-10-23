import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
// Fontes locais (woff2 empacotados pelo Vite); nenhuma requisição a CDN.
import "@fontsource/inter/latin-300.css";
import "@fontsource/inter/latin-400.css";
import "@fontsource/inter/latin-500.css";
import "@fontsource/inter/latin-600.css";
import "@fontsource/jetbrains-mono/latin-400.css";
import "@fontsource/jetbrains-mono/latin-500.css";
import "./styles.css";
import { Shell } from "./desktop/Shell";
import { ENTRY_PATH } from "./api/client";
import { ConnectPage } from "./connect/ConnectPage";
import { clearStaleStorage } from "./staleStorage";
import { UiStateSync } from "./desktop/uiState";
import { initLanguage } from "./i18n";
import { bindLanguageToUiState } from "./i18n/uiStateBinding";
import { captureChatgptReturn } from "./apps/settings/chatgptPlan";

clearStaleStorage();

// Página de entrada (spec 112): o aparelho sem dispositivo autorizado só vê o pedido do código.
// Nenhuma chamada à API além da troca do código (as outras respondem 401).
const entry = window.location.pathname === ENTRY_PATH;

// Idioma (spec 101): `<html lang>` e o idioma ativo saem do espelho local e do navegador antes do
// primeiro render; depois a preferência salva no servidor (seção global `language`) vence. Este
// cliente só lê (sem cache próprio); as trocas de outras abas chegam pelo canal do ui-state.
initLanguage();
if (!entry) {
  const languageSource = new UiStateSync({ kind: "global" }, { storage: null });
  bindLanguageToUiState(languageSource);
  void languageSource.start();
}

// Volta do login pelo plano ChatGPT (spec 116): o resultado fica guardado antes do primeiro render
// (a rota do desktop normaliza a URL) e a página volta ao espaço em que o login começou.
if (!entry) {
  const back = captureChatgptReturn(window.location.search);
  if (back !== null) window.history.replaceState(window.history.state, "", back);
}

const container = document.getElementById("root");
if (container === null) {
  throw new Error("elemento #root ausente");
}

createRoot(container).render(
  <StrictMode>{entry ? <ConnectPage /> : <Shell />}</StrictMode>,
);
