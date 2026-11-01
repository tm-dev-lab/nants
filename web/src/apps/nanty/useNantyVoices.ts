/*
 * O estado da voz e a lista de vozes (spec 139 T09), pela rota `GET /api/nanty/voices`: a resposta
 * dela é a regra do servidor (pronta, sem chave ou sem conexão), para a conexão escolhida, mesmo antes
 * de salvar. `signature` muda quando as conexões cadastradas mudam: quem acabou de criar a conexão ou
 * colocar a chave em Configurações vê o estado novo sem reabrir os Ajustes.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { getNantyVoices } from "./nantyApi";
import { voiceCheckOf, type VoiceCheck } from "./nantySettingsModel";

export function useNantyVoices(enabled: boolean, connectionId: string, signature: string): { check: VoiceCheck; reload: () => void } {
  const [check, setCheck] = useState<VoiceCheck>({ status: "loading" });
  const token = useRef(0);

  const load = useCallback((): void => {
    const mine = ++token.current;
    setCheck({ status: "loading" });
    void getNantyVoices(connectionId).then((result) => {
      if (token.current === mine) setCheck(voiceCheckOf(result));
    });
  }, [connectionId]);

  useEffect(() => {
    if (enabled) load();
    return () => {
      token.current += 1;
    };
  }, [enabled, load, signature]);

  return { check, reload: load };
}
