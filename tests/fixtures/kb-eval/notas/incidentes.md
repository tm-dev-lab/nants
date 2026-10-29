# Gestão de incidentes

O que fazer quando algo quebra em produção.

## Severidades

- SEV1: o produto está fora do ar para todos os clientes ou há vazamento de dados.
- SEV2: uma função central está fora para parte dos clientes (pagamento, login).
- SEV3: falha parcial com contorno conhecido.
- SEV4: problema cosmético ou interno, sem efeito no cliente.

## Plantão

O plantão é semanal, de segunda às 9h até a segunda seguinte às 9h. A pessoa de plantão atende o
alerta em até 5 minutos para SEV1 e SEV2 e em até 30 minutos para SEV3. Quem não consegue atender
passa o bastão para a segunda linha pelo aplicativo de escala, nunca por mensagem direta.

## Comunicação com o cliente

Em SEV1 e SEV2, a página de status é atualizada em até 15 minutos depois do início, e a cada 30
minutos até o fim. O texto público não cita causa técnica nem fornecedor; só o efeito e a
previsão.

## Sala de guerra

O comandante do incidente abre um canal `#inc-<data>-<tema>` e uma chamada de voz. O comandante
coordena e não digita comandos; quem investiga anota tudo no canal com horário.

## Pós-mortem

Todo SEV1 e SEV2 tem pós-mortem sem culpados em até 5 dias úteis, com linha do tempo, causa raiz,
o que funcionou e as ações com dono e prazo. O documento vai para a pasta compartilhada de
pós-mortens e é lido na reunião mensal de confiabilidade.
