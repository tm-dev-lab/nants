//! Dicionário pt-BR do servidor e do terminal (spec 102). O `en` tem as mesmas chaves e os mesmos
//! parâmetros (teste em `i18n::tests`). Sem travessão nem emoji (`tests/text_style.rs`).

pub const MESSAGES: &[(&str, &str)] = &[
    // Ajuda e mensagens do executável.
    (
        "cli.usage",
        "uso: nants [--no-open] [--lan] [--port N] [--config PATH] | nants serve [--port N] [--host H | --lan] [--config PATH] [--allow-remote] [--no-open] | nants doctor [--config PATH] | nants uninstall [--purge] | nants --version\n\
nants         sobe o Nants e abre o navegador (--no-open desliga); em 127.0.0.1, ou na rede local se ligada em Configurações > Acesso pela rede\n\
nants serve   sobe o Nants sem abrir o navegador\n\
nants doctor  confere o ambiente (Docker, Compose, Git, porta, disco) sem alterar nada\n\
nants uninstall          remove o executável; mantém configuração, dados e projetos (pede confirmação)\n\
nants uninstall --purge  remove também a configuração e os dados; nunca os projetos nem o Docker\n\
--port N      porta fixa; se estiver ocupada, falha (sem --port, a padrão 3000 ou a próxima livre)\n\
--lan         aceita acesso pela rede local (0.0.0.0) só nesta execução, com as portas das prévias dos apps logo após a porta (3001 a 3010); para deixar ligado, use Configurações > Acesso pela rede; em modo rede, só aparelhos que você adicionar abrem o Nants, e o terminal mostra o link de entrada deste computador (uso único, vale 10 minutos)\n\
--allow-remote  permite um --host fora do loopback (o servidor fica exposto nesse endereço; como no modo rede, só aparelhos que você adicionar abrem o Nants, e o terminal mostra o link de entrada deste computador)",
    ),
    ("cli.unknown_command", "comando desconhecido: {command}"),
    ("cli.unknown_arg", "argumento desconhecido: {arg}"),
    ("cli.needs_value", "{flag} requer um valor"),
    ("cli.bad_port", "porta inválida: {value}"),
    ("cli.lan_with_host", "--lan não pode ser combinado com --host (o modo LAN usa 0.0.0.0)"),
    ("cli.error", "erro: {message}"),
    ("cli.config_invalid_at", "config inválido (posição de byte {position})"),
    ("cli.config_invalid", "config inválido (TOML malformado)"),
    ("cli.config_serialize", "falha ao serializar a configuração"),
    ("cli.config_io", "erro de I/O: {kind}"),
    ("cli.config_missing_env", "variável de ambiente ausente: {name}"),
    ("cli.config_load_failed", "erro ao carregar {path}: {reason}"),
    ("cli.doctor_config_unreadable", "aviso: não foi possível ler {path} ({reason}); usando a configuração padrão"),
    ("cli.doctor_config_default", "aviso: {reason}; usando a configuração padrão"),
    ("cli.port_fallback", "a porta {port} está ocupada; usando a porta {next}"),
    (
        "cli.ports_busy",
        "as portas {port} a {last} estão ocupadas em {host}; libere uma delas ou escolha outra com --port N",
    ),
    ("cli.port_in_use", "a porta {port} já está em uso em {host}; feche o programa que a usa ou escolha outra com --port N"),
    ("cli.bind_failed", "erro ao abrir {host}:{port}: {error}"),
    ("cli.not_loopback", "host {host} não é loopback; use --allow-remote para permitir"),
    ("cli.exposed", "aviso: servidor exposto em {host}; a validação de Host aceita apenas nomes de loopback"),
    ("cli.local_addr_failed", "erro ao obter endereço local: {error}"),
    ("cli.view_ports", "  portas de visualização dos apps: {start} a {end}"),
    ("cli.lan_warning", "aviso: acesso pela rede local ligado; só aparelhos que você adicionar abrem o Nants (o tráfego na rede é HTTP, sem criptografia)"),
    (
        "cli.lan_adopted",
        "Acesso pela rede local ligado (o Windows já está configurado). Para desligar: Configurações > Acesso pela rede.",
    ),
    ("cli.lan_save_failed", "aviso: não foi possível salvar a opção na configuração ({error}); ela vale só nesta execução"),
    (
        "cli.port_redirected",
        "a porta {port} está redirecionada pelo Windows (acesso pela rede configurado antes); o Nants está na porta {next}. Para usar a {port} de novo, ligue o acesso pela rede local em Configurações > Acesso pela rede ou desfaça o redirecionamento lá.",
    ),
    (
        "cli.port_redirected_explicit",
        "a porta {port} está redirecionada pelo Windows (acesso pela rede configurado antes) e, no modo local, o navegador não abriria a página. Escolha outra com --port N, ligue o acesso pela rede local em Configurações > Acesso pela rede ou desfaça o redirecionamento lá.",
    ),
    ("cli.open_browser", "abra no navegador: {url}"),
    ("cli.open_failed", "não foi possível abrir o navegador; abra {url} manualmente"),
    ("cli.server_error", "erro no servidor: {error}"),
    // nants uninstall (spec 103, decisão 7).
    ("uninstall.will_remove", "O nants uninstall vai remover:"),
    ("uninstall.item.executable", "  o executável: {path}"),
    ("uninstall.item.config", "  a configuração: {path}"),
    ("uninstall.item.data", "  os dados do Nants: {path}"),
    ("uninstall.keeps", "Não são removidos:"),
    (
        "uninstall.keep.config_data",
        "  a configuração ({config}) e os dados ({data}); para apagar também, use nants uninstall --purge",
    ),
    (
        "uninstall.keep.config_unreadable",
        "  {path}, porque a configuração {config} não pôde ser lida e a pasta dos projetos pode estar ali dentro; corrija o arquivo e rode de novo",
    ),
    ("uninstall.keep.inside_projects", "  {path}, porque contém a pasta dos projetos ou está dentro dela"),
    ("uninstall.keep.projects", "  os seus projetos em {path}"),
    (
        "uninstall.keep.docker",
        "  os recursos Docker dos projetos (rótulo nants.project). Para listá-los: {command}",
    ),
    ("uninstall.keep.other_exe", "  este executável ({path}) fica onde está: o instalador usa só {installed}"),
    ("uninstall.nothing", "Nada para remover: o Nants não está instalado em {path}."),
    ("uninstall.prompt", "Confirma? Digite s e Enter para remover (só Enter cancela):"),
    (
        "uninstall.no_terminal",
        "Sem terminal para confirmar: nada foi removido. Rode {command} direto num terminal e responda s.",
    ),
    ("uninstall.canceled", "Cancelado: nada foi removido."),
    ("uninstall.removed", "removido: {path}"),
    ("uninstall.failed", "não foi possível remover {path}: {error}"),
    ("uninstall.partial", "Algumas remoções falharam; veja acima."),
    ("uninstall.done", "Nants desinstalado. Para instalar de novo: {command}"),
    ("uninstall.no_home", "não foi possível achar a pasta pessoal (HOME); nada foi removido"),
    // nants doctor: moldura do relatório.
    ("doctor.title", "nants {version}: diagnóstico do ambiente"),
    ("doctor.status.ok", "OK"),
    ("doctor.status.fail", "FALHA"),
    ("doctor.status.info", "INFO"),
    ("doctor.status.warn", "AVISO"),
    ("doctor.action", ". Ação: {fix}"),
    ("doctor.summary.ok", "Tudo pronto. Digite nants para abrir o Nants no navegador."),
    ("doctor.summary.fail", "Falta algo obrigatório: siga a ação dos itens com FALHA e rode nants doctor de novo."),
    // nants doctor: rótulos pelo id estável do item.
    ("doctor.label.git", "Git"),
    ("doctor.label.docker_cli", "Docker CLI"),
    ("doctor.label.docker_daemon", "Docker daemon"),
    ("doctor.label.docker_permission", "Permissão no socket do Docker"),
    ("doctor.label.docker_compose", "Docker Compose v2"),
    ("doctor.label.port", "Porta do Nants"),
    ("doctor.label.disk", "Espaço em disco"),
    ("doctor.label.wsl", "WSL"),
    ("doctor.label.windows_port_redirect", "Acesso pela rede local"),
    ("doctor.label.playwright_images", "Imagens do Playwright"),
    ("doctor.label.version", "Versão do Nants"),
    // nants doctor: ação sugerida pelo id estável do item.
    ("doctor.fix.git", "Instale o Git: sudo apt install git (Debian/Ubuntu/WSL). Depois recarregue esta tela."),
    (
        "doctor.fix.docker_cli",
        "Instale o Docker Engine (https://docs.docker.com/engine/install/) ou, no Windows, o Docker Desktop com a integração WSL ligada (Settings > Resources > WSL integration). O Nants não instala o Docker.",
    ),
    (
        "doctor.fix.docker_daemon",
        "Inicie o Docker: sudo systemctl start docker (Linux) ou abra o Docker Desktop (Windows/WSL) e ligue a integração com a sua distribuição WSL.",
    ),
    (
        "doctor.fix.docker_permission",
        "Adicione seu usuário ao grupo docker: sudo usermod -aG docker $USER e reabra a sessão (feche e abra o terminal; no WSL, rode wsl --shutdown no Windows).",
    ),
    (
        "doctor.fix.docker_compose",
        "Instale o plugin Compose v2: sudo apt install docker-compose-plugin (ou atualize o Docker Desktop).",
    ),
    ("doctor.fix.port", "Feche o programa que usa essas portas ou escolha outra com nants --port N."),
    (
        "doctor.fix.disk",
        "Libere espaço em disco: o Nants precisa de pelo menos 5 GB livres para as imagens Docker e os projetos.",
    ),
    ("doctor.fix.version", "Para atualizar, rode: {command}"),
    (
        "doctor.fix.windows_port_redirect",
        "Abra Configurações > Acesso pela rede: ligue ou desligue o acesso pela rede local e rode o script indicado ali (configurar ou desfazer) no PowerShell como administrador.",
    ),
    // nants doctor: detalhes.
    ("doctor.detail.git_missing", "git não encontrado"),
    ("doctor.detail.docker_missing", "docker não encontrado"),
    ("doctor.detail.depends_on_cli", "depende do Docker CLI"),
    ("doctor.detail.daemon_unchecked_permission", "não verificado: sem permissão no socket"),
    ("doctor.detail.permission_denied", "permission denied ao acessar o socket"),
    ("doctor.detail.daemon_unreachable", "daemon inacessível"),
    ("doctor.detail.permission_unchecked", "não verificado: daemon inacessível"),
    ("doctor.detail.compose_missing", "plugin compose ausente"),
    ("doctor.detail.daemon_timeout", "não respondeu a tempo"),
    ("doctor.detail.port_auto", "porta escolhida pelo sistema ao iniciar"),
    ("doctor.detail.port_free", "a porta {port} está livre"),
    ("doctor.detail.port_fallback", "a porta {port} está ocupada (talvez o Nants já esteja aberto); o nants usará a {next}"),
    ("doctor.detail.port_redirected", "a porta {port} está redirecionada pelo Windows; o Nants usará a {next}"),
    ("doctor.detail.port_all_busy", "as portas {port} a {last} estão ocupadas"),
    ("doctor.detail.disk_unmeasured", "não foi possível medir o espaço livre"),
    ("doctor.detail.disk", "{list}"),
    ("doctor.detail.disk_free", "{size} livres em {path}"),
    (
        "doctor.detail.wsl_detected",
        "detectado: o Docker pode ser o Docker Desktop com a integração WSL ligada ou o Docker Engine dentro da distribuição",
    ),
    ("doctor.detail.wsl_absent", "não detectado (Linux)"),
    (
        "doctor.detail.env_wsl_nat",
        "WSL no modo de rede NAT (o padrão); o Docker pode ser o Docker Desktop com a integração WSL ligada ou o Docker Engine dentro da distribuição",
    ),
    (
        "doctor.detail.env_wsl_mirrored",
        "WSL no modo de rede espelhado; o Docker pode ser o Docker Desktop com a integração WSL ligada ou o Docker Engine dentro da distribuição",
    ),
    (
        "doctor.detail.env_wsl_unknown",
        "WSL (não foi possível conferir o modo de rede; tratado como NAT); o Docker pode ser o Docker Desktop com a integração WSL ligada ou o Docker Engine dentro da distribuição",
    ),
    (
        "doctor.detail.windows_redirect_on",
        "o Windows está redirecionando a porta {port} para o WSL (acesso pela rede configurado antes), mas o acesso pela rede local está desligado: no modo local, o navegador não abre a página nessa porta e o nants usa outra. Ligue o acesso pela rede local ou desfaça o redirecionamento",
    ),
    ("doctor.detail.windows_redirect_off", "acesso pela rede local desligado e o Windows não redireciona a porta {port}"),
    ("doctor.detail.windows_redirect_unknown", "não foi possível conferir pelo netsh.exe do Windows"),
    ("doctor.detail.firewall_unknown", "não foi possível conferir a regra de firewall pelo PowerShell do Windows"),
    ("doctor.detail.lan_on_ok", "acesso pela rede local ligado e o Windows configurado para a porta {port}; só aparelhos que você adicionar abrem o Nants"),
    (
        "doctor.detail.lan_on_mirrored_ok",
        "acesso pela rede local ligado e a regra de firewall Nants presente no Windows (modo espelhado, porta {port}); só aparelhos que você adicionar abrem o Nants",
    ),
    (
        "doctor.detail.lan_on_no_redirect",
        "acesso pela rede local ligado, mas o Windows não redireciona a porta {port} para o WSL: outros aparelhos não alcançam o Nants. Rode o script",
    ),
    (
        "doctor.detail.lan_on_other_target",
        "acesso pela rede local ligado, mas o Windows redireciona a porta {port} para outro endereço (o IP do WSL muda quando o Windows reinicia). Rode o script de novo",
    ),
    (
        "doctor.detail.lan_on_no_firewall",
        "acesso pela rede local ligado, mas falta a regra de firewall Nants no Windows: outros aparelhos não alcançam a porta {port}. Rode o script",
    ),
    (
        "doctor.detail.lan_will_adopt",
        "o Windows já redireciona a porta {port} para este WSL e tem a regra de firewall Nants: o próximo início do nants liga o acesso pela rede local",
    ),
    (
        "doctor.detail.lan_linux_on",
        "ligado: o Nants escuta na rede local (porta {port}) e só aparelhos que você adicionar abrem o Nants; se outro aparelho não abrir, libere a porta no firewall do Linux",
    ),
    (
        "doctor.detail.lan_linux_off",
        "desligado: o Nants só abre neste computador; para ligar, use Configurações > Acesso pela rede",
    ),
    (
        "doctor.detail.playwright_no_docker",
        "não verificado: Docker indisponível. O primeiro teste de navegador de um projeto baixa cerca de 2 GB",
    ),
    (
        "doctor.detail.playwright_list_failed",
        "não foi possível listar as imagens. O primeiro teste de navegador de um projeto baixa cerca de 2 GB",
    ),
    ("doctor.detail.playwright_none", "nenhuma baixada ainda: o primeiro teste de navegador de um projeto baixa cerca de 2 GB"),
    (
        "doctor.detail.playwright_some",
        "já baixadas: {tags}. Projetos com outra versão baixam cerca de 2 GB no primeiro teste de navegador",
    ),
    ("doctor.detail.version_unknown", "{local} instalada; não foi possível verificar se há versão nova ({why})"),
    ("doctor.detail.version_available", "{local} instalada; a {latest} está disponível"),
    ("doctor.detail.version_latest", "{local}, a mais recente"),
    ("doctor.why.bad_manifest_version", "manifesto com versão inválida"),
    ("doctor.why.no_url", "endereço das versões não configurado"),
    ("doctor.why.bad_url", "endereço das versões inválido"),
    ("doctor.why.no_client", "cliente HTTP indisponível"),
    ("doctor.why.no_connection", "sem conexão com o servidor de versões"),
    ("doctor.why.http", "manifesto indisponível, HTTP {status}"),
    ("doctor.why.incomplete", "manifesto incompleto"),
    ("doctor.why.too_large", "manifesto grande demais"),
    ("doctor.why.invalid", "manifesto inválido"),
    ("doctor.why.no_version", "manifesto sem versão"),
    // Relatório final: textos fixos montados pelo servidor.
    (
        "report.env_clean",
        "_Ambiente de testes limpo (containers, volumes e redes do Compose `-test` removidos)._",
    ),
    ("report.traceability.title", "## Rastreabilidade"),
    ("report.traceability.commit", "{id}, commit {hash}"),
    ("report.traceability.no_commit", "{id}, sem commit de integração"),
    ("report.traceability.no_task", "sem tarefa declarada"),
    // Runner: tarefa de correção criada pela verificação final (spec 107), gravada no Txx.md.
    ("runner.final_fix_title", "Correção da verificação final"),
    // Runner: mensagens do agente `tests` na conversa (spec 113 T04), no idioma do projeto.
    ("runner.tests.passed_command", "testes ✓{place} (`{command}`)"),
    ("runner.tests.passed_checks", "testes ✓{place} · {checks}"),
    ("runner.tests.on_base", " na base"),
    ("runner.tests.failed_cycle", "test_command falhou ({count}/{max})"),
    ("runner.tests.reused", "testes e checagens não repetidos na base: árvore idêntica à testada em {sha} (fora de .sdd/)"),
    ("runner.tests.reused_summary", "reaproveitado: árvore idêntica à testada em {sha}"),
    ("runner.checks.list", "checagens: {list}"),
    ("runner.checks.non_blocking", "{name} {mark} (não bloqueante)"),
    ("runner.checks.non_blocking_regression", "{name} {mark} (não bloqueante, regressão desta tarefa)"),
    ("runner.checks.not_run", "{name} não executado"),
    ("runner.checks.failed_on_base", "checagem {name} falhou na base:"),
    ("runner.final_base.prefix", "verificação final na base"),
    ("runner.final_base.line", "{prefix}: testes {mark}{checks}"),
    ("runner.final_base.not_run", "não executado"),
    // Conselheiro (ask_architect): pergunta e resposta mostradas na conversa.
    ("advisor.question", "Pergunta ao Arquiteto: {question}"),
    ("advisor.answer", "Resposta ao {who}: {answer}"),
    ("advisor.scope_added", "allowed_scope ampliado: {globs}"),
    ("advisor.criteria_rewritten", "Critérios de aceitação reescritos."),
    // Levantamento da arquitetura: aviso de falha na conversa.
    ("survey.failed", "Não consegui concluir o levantamento da arquitetura ({code}). O projeto segue normalmente: ao planejar a primeira especificação com o Arquiteto na conversa, ele lê o código e cria o ARCHITECTURE.md junto com o plano."),
    // Rótulos da interface citados ao modelo (o mesmo texto do botão na interface).
    ("ui.app_start", "Iniciar o app"),
    // Runner: formatação automática (spec 078) na conversa, agente `tests`.
    ("runner.format.failed", "formatação automática falhou (não bloqueia): {why}"),
    ("runner.format.tool_failed", "formatação automática ({name}) falhou (não bloqueia): {why}"),
    ("runner.format.applied", "formatação automática: {count} arquivos ({names})"),
    ("runner.format.timed_out", "tempo esgotado"),
    ("runner.format.exit", "saída {code}: {output}"),
    // Conselheiro: caminhos de scope_add que não entraram (mesmo texto do registro da tarefa).
    ("advisor.scope_covered", "`{path}` já coberto por `{by}`"),
    ("advisor.scope_limit", "`{path}` não ampliado: limite de {limit} entradas no allowed_scope"),
    ("advisor.scope_limit_notice", "o escopo não pôde ser ampliado (limite); peça ao Arquiteto outra saída ou declare o impedimento"),
    // Dispositivos autorizados (spec 112).
    ("devices.this_computer", "Este computador"),
    ("devices.fallback_name", "Dispositivo"),
    ("devices.browser", "Navegador"),
    ("devices.name_browser_os", "{browser} no {os}"),
    ("cli.entry_link", "link de entrada deste computador (uso único, vale 10 minutos): {url}"),
    ("cli.entry_link_failed", "não foi possível gerar o link de entrada; reinicie o nants"),
];
