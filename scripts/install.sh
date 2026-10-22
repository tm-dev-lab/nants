#!/bin/sh
# Instalador do Nants (spec 103, decisão 5). Uso:
#
#   curl -fsSL https://nants.tmdevlab.com/install.sh | sh
#   curl -fsSL https://nants.tmdevlab.com/install.sh | NANTS_VERSION=0.1.0 sh
#   curl -fsSL https://nants.tmdevlab.com/install.sh | NANTS_VERSION=0.1.0-rc.1 sh
#
# O que faz, sem perguntar nada e sem sudo:
#   1. confere o sistema (Linux ou WSL2; x86_64 ou aarch64) e as ferramentas (curl, tar, gzip,
#      sha256sum);
#   2. lê o manifesto ${NANTS_RELEASES_URL:-https://nants.tmdevlab.com}/latest.json e escolhe o
#      pacote da arquitetura (com NANTS_VERSION=X.Y.Z ou X.Y.Z-rc.N, a versão pedida, no mesmo
#      endereço);
#   3. baixa o pacote para uma pasta temporária, confere o SHA-256 e só então grava
#      ~/.local/bin/nants (rodar de novo atualiza);
#   4. avisa se ~/.local/bin não está no PATH e roda nants doctor.
#
# Nunca instala Docker nem Git e nunca mexe fora de ~/.local/bin.
# Todo o corpo fica em funções; main é chamada só na última linha, então um download interrompido
# não executa um script pela metade.
set -eu

DEFAULT_RELEASES_URL="https://nants.tmdevlab.com"
INSTALL_CMD="curl -fsSL https://nants.tmdevlab.com/install.sh | sh"

# Idioma das mensagens: a mesma regra do executável (LC_ALL > LC_MESSAGES > LANG; o primeiro não
# vazio decide; pt dá pt-BR, qualquer outro valor ou nenhum dá en).
detect_lang() {
  for v in "${LC_ALL:-}" "${LC_MESSAGES:-}" "${LANG:-}"; do
    [ -n "$v" ] || continue
    primary=$(printf '%s\n' "$v" | sed 's/[_.@-].*$//' | tr '[:upper:]' '[:lower:]')
    if [ "$primary" = "pt" ]; then
      echo pt
    else
      echo en
    fi
    return 0
  done
  echo en
}

# Texto da mensagem `$1` no idioma ativo; os demais argumentos preenchem os %s.
msg() {
  key=$1
  shift
  if [ "$NANTS_LANG" = pt ]; then
    case $key in
      system) f='Sistema: %s, %s' ;;
      wsl) f='WSL detectado: o Nants roda dentro da sua distribuição Linux.' ;;
      windows) f='O Nants roda no Windows dentro do WSL2. No PowerShell, rode wsl --install, abra o Ubuntu e, nele, rode: %s' ;;
      macos) f='O Nants ainda não tem versão para macOS. Esta versão roda no Linux e no WSL2 do Windows.' ;;
      os_unsupported) f='Sistema não suportado: %s. O Nants roda no Linux e no WSL2 do Windows.' ;;
      arch_unsupported) f='Arquitetura não suportada: %s. O Nants roda em x86_64 e aarch64.' ;;
      missing_tool) f='Falta o comando %s. Instale-o (ex.: sudo apt install %s) e rode o instalador de novo.' ;;
      bad_version) f='NANTS_VERSION inválida: %s. Use o formato X.Y.Z ou X.Y.Z-rc.N, ex.: NANTS_VERSION=0.1.0' ;;
      manifest) f='Lendo as versões em %s' ;;
      manifest_failed) f='Não foi possível ler %s. Confira a conexão e tente de novo.' ;;
      manifest_bad) f='O manifesto %s está incompleto ou inválido. Tente de novo mais tarde.' ;;
      no_base) f='Não foi possível montar o endereço da versão %s a partir do manifesto.' ;;
      downloading) f='Baixando o Nants %s (%s)' ;;
      download_failed) f='Não foi possível baixar %s. A versão existe? Confira a conexão e tente de novo.' ;;
      sha_failed) f='O SHA-256 do pacote não confere (esperado %s, obtido %s). Nada foi instalado.' ;;
      sha_ok) f='SHA-256 conferido.' ;;
      extract_failed) f='O pacote baixado está corrompido. Nada foi instalado.' ;;
      install_failed) f='Não foi possível gravar %s.' ;;
      installed) f='Nants %s instalado em %s' ;;
      updated) f='Nants atualizado em %s (antes: %s, agora: %s)' ;;
      replaced) f='Nants %s instalado em %s, no lugar do executável anterior (que também diz %s).' ;;
      prerelease) f='Esta é uma pré-release: o nants --version mostra %s.' ;;
      path_missing) f='%s não está no PATH. Para usar agora, rode:' ;;
      path_forever) f='Para valer sempre, inclua a linha no seu %s e feche e reabra o terminal:' ;;
      doctor) f='Conferindo o ambiente (nants doctor):' ;;
      done) f='Pronto. Digite nants para abrir o Nants no navegador.' ;;
      *) f=$key ;;
    esac
  else
    case $key in
      system) f='System: %s, %s' ;;
      wsl) f='WSL detected: Nants runs inside your Linux distribution.' ;;
      windows) f='On Windows, Nants runs inside WSL2. In PowerShell, run wsl --install, open Ubuntu and run there: %s' ;;
      macos) f='Nants has no macOS version yet. This version runs on Linux and on Windows WSL2.' ;;
      os_unsupported) f='Unsupported system: %s. Nants runs on Linux and on Windows WSL2.' ;;
      arch_unsupported) f='Unsupported architecture: %s. Nants runs on x86_64 and aarch64.' ;;
      missing_tool) f='The %s command is missing. Install it (e.g. sudo apt install %s) and run the installer again.' ;;
      bad_version) f='Invalid NANTS_VERSION: %s. Use the X.Y.Z or X.Y.Z-rc.N format, e.g. NANTS_VERSION=0.1.0' ;;
      manifest) f='Reading the versions at %s' ;;
      manifest_failed) f='Could not read %s. Check your connection and try again.' ;;
      manifest_bad) f='The manifest %s is incomplete or invalid. Try again later.' ;;
      no_base) f='Could not build the address of version %s from the manifest.' ;;
      downloading) f='Downloading Nants %s (%s)' ;;
      download_failed) f='Could not download %s. Does the version exist? Check your connection and try again.' ;;
      sha_failed) f='The package SHA-256 does not match (expected %s, got %s). Nothing was installed.' ;;
      sha_ok) f='SHA-256 verified.' ;;
      extract_failed) f='The downloaded package is corrupted. Nothing was installed.' ;;
      install_failed) f='Could not write %s.' ;;
      installed) f='Nants %s installed at %s' ;;
      updated) f='Nants updated at %s (before: %s, now: %s)' ;;
      replaced) f='Nants %s installed at %s, replacing the previous executable (which also reports %s).' ;;
      prerelease) f='This is a pre-release: nants --version shows %s.' ;;
      path_missing) f='%s is not in your PATH. To use it now, run:' ;;
      path_forever) f='To keep it, add the line to your %s and close and reopen the terminal:' ;;
      doctor) f='Checking the environment (nants doctor):' ;;
      done) f='Done. Type nants to open Nants in the browser.' ;;
      *) f=$key ;;
    esac
  fi
  # shellcheck disable=SC2059 # o formato vem só da tabela acima
  printf "$f\n" "$@"
}

say() { printf 'nants: %s\n' "$(msg "$@")"; }

die() {
  printf 'nants: %s\n' "$(msg "$@")" >&2
  exit 1
}

# Sistema e arquitetura (uname); recusa com a orientação certa fora do Linux.
detect_platform() {
  os=$(uname -s 2>/dev/null || echo unknown)
  case $os in
    Linux) ;;
    Darwin) die macos ;;
    MINGW* | MSYS* | CYGWIN* | Windows_NT) die windows "$INSTALL_CMD" ;;
    *) die os_unsupported "$os" ;;
  esac
  machine=$(uname -m 2>/dev/null || echo unknown)
  case $machine in
    x86_64 | amd64) ARCH=x86_64 ;;
    aarch64 | arm64) ARCH=aarch64 ;;
    *) die arch_unsupported "$machine" ;;
  esac
  TARGET="$ARCH-linux"
  say system "Linux" "$ARCH"
  # WSL: só informativo.
  kernel=$(uname -r 2>/dev/null || true)
  case $kernel in
    *[Mm]icrosoft* | *WSL*) say wsl ;;
    *) [ -z "${WSL_DISTRO_NAME:-}" ] || say wsl ;;
  esac
}

require_tools() {
  for tool in curl tar gzip sha256sum; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      pkg=$tool
      [ "$tool" = sha256sum ] && pkg=coreutils
      die missing_tool "$tool" "$pkg"
    fi
  done
}

fetch() {
  # $1 = URL, $2 = destino. Com o endereço das versões em https (o normal), nem a URL nem um
  # redirecionamento podem cair para http. Base http só no teste local (NANTS_RELEASES_URL).
  case $RELEASES_URL in
    https://*) curl -fsSL --proto '=https' --proto-redir '=https' --connect-timeout 20 --retry 2 -o "$2" "$1" ;;
    *) curl -fsSL --connect-timeout 20 --retry 2 -o "$2" "$1" ;;
  esac
}

# Valor de texto da chave `$2` no JSON `$1` (o latest.json tem formato fixo, sem aspas escapadas
# nos valores; não depende do jq).
json_str() {
  printf '%s\n' "$1" | sed -n "s/.*\"$2\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p"
}

# Corpo do objeto `"$2": { ... }` no JSON `$1` (um nível, sem chaves aninhadas).
json_obj() {
  printf '%s\n' "$1" | sed -n "s/.*\"$2\"[[:space:]]*:[[:space:]]*{\([^}]*\)}.*/\1/p"
}

# X.Y.Z ou a pré-release X.Y.Z-rc.N, e nada além disso.
is_version() {
  # Só dígitos, pontos, "-", "r" e "c", numa linha só (o grep abaixo olharia linha a linha).
  case $1 in
    '' | *[!0-9.rc-]*) return 1 ;;
  esac
  printf '%s\n' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$'
}

# Número que o próprio executável mostra no --version: o da pré-release X.Y.Z-rc.N é o X.Y.Z
# (o Cargo.toml não tem o sufixo).
core_version() {
  printf '%s\n' "${1%%-rc.*}"
}

is_sha256() {
  printf '%s\n' "$1" | grep -Eq '^[0-9a-f]{64}$'
}

# URL aceita para o pacote: https sempre; http só quando o próprio manifesto veio por http (teste
# local com NANTS_RELEASES_URL).
url_ok() {
  case $1 in
    https://*) return 0 ;;
    http://*) case $RELEASES_URL in http://*) return 0 ;; esac ;;
  esac
  return 1
}

# Lê o latest.json e define VERSION, URL e SHA (com NANTS_VERSION, a versão pedida).
resolve_package() {
  manifest_url="$RELEASES_URL/latest.json"
  say manifest "$manifest_url"
  fetch "$manifest_url" "$TMP/latest.json" || die manifest_failed "$manifest_url"
  json=$(tr -d '\r\n' <"$TMP/latest.json")
  asset=$(json_obj "$json" "$TARGET")
  latest_version=$(json_str "$json" version)
  latest_url=$(json_str "$asset" url)
  latest_sha=$(json_str "$asset" sha256)
  if ! is_version "$latest_version" || ! url_ok "$latest_url"; then
    die manifest_bad "$manifest_url"
  fi
  if [ -z "$WANTED" ]; then
    is_sha256 "$latest_sha" || die manifest_bad "$manifest_url"
    VERSION=$latest_version
    URL=$latest_url
    SHA=$latest_sha
    return 0
  fi
  # Versão específica: mesma base de endereço do pacote atual (o que vem antes de
  # /vA.B.C/arquivo ou /vA.B.C-rc.N/arquivo, ex.: https://github.com/tm-dev-lab/nants/releases/download),
  # em vX.Y.Z/ (ou vX.Y.Z-rc.N/), com o .sha256 ao lado.
  base=$(printf '%s\n' "$latest_url" | sed 's#/v[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\(-rc\.[0-9][0-9]*\)\{0,1\}/[^/]*$##')
  [ -n "$base" ] && [ "$base" != "$latest_url" ] || die no_base "$WANTED"
  VERSION=$WANTED
  URL="$base/v$VERSION/nants-$VERSION-$TARGET.tar.gz"
  fetch "$URL.sha256" "$TMP/pkg.sha256" || die download_failed "$URL.sha256"
  line=$(head -n 1 "$TMP/pkg.sha256")
  SHA=${line%% *}
  is_sha256 "$SHA" || die download_failed "$URL.sha256"
}

download_and_verify() {
  say downloading "$VERSION" "$TARGET"
  fetch "$URL" "$TMP/pkg.tar.gz" || die download_failed "$URL"
  sum=$(sha256sum "$TMP/pkg.tar.gz")
  sum=${sum%% *}
  [ "$sum" = "$SHA" ] || die sha_failed "$SHA" "$sum"
  say sha_ok
  mkdir -p "$TMP/pkg"
  tar -xzf "$TMP/pkg.tar.gz" -C "$TMP/pkg" nants 2>/dev/null || die extract_failed
  [ -f "$TMP/pkg/nants" ] && [ -s "$TMP/pkg/nants" ] || die extract_failed
}

install_binary() {
  BIN_DIR="$HOME/.local/bin"
  dest="$BIN_DIR/nants"
  previous=""
  if [ -x "$dest" ]; then
    previous=$("$dest" --version 2>/dev/null | sed -n 's/^nants //p' | head -n 1) || previous=""
  fi
  mkdir -p "$BIN_DIR" || die install_failed "$dest"
  # Cópia ao lado e troca atômica: um Nants aberto continua rodando com o executável antigo.
  staged="$BIN_DIR/.nants.install.$$"
  if ! cp "$TMP/pkg/nants" "$staged" || ! chmod 755 "$staged" || ! mv -f "$staged" "$dest"; then
    rm -f "$staged"
    die install_failed "$dest"
  fi
  # O --version da pré-release mostra o X.Y.Z dela: sem comparar rc, só diz o que se sabe. Se o
  # anterior diz o mesmo número (a mesma versão de novo, ou a rc e a final dela), só registra a troca.
  core=$(core_version "$VERSION")
  if [ -z "$previous" ]; then
    say installed "$VERSION" "$dest"
  elif [ "$previous" = "$core" ]; then
    say replaced "$VERSION" "$dest" "$previous"
  else
    say updated "$dest" "$previous" "$VERSION"
  fi
  [ "$core" = "$VERSION" ] || say prerelease "$core"
}

# Arquivo de inicialização do shell da pessoa, para a linha do PATH.
shell_rc() {
  case ${SHELL:-} in
    */zsh) echo "~/.zshrc" ;;
    */bash) echo "~/.bashrc" ;;
    *) echo "~/.profile" ;;
  esac
}

path_hint() {
  case ":${PATH:-}:" in
    *":$BIN_DIR:"*) return 0 ;;
  esac
  # shellcheck disable=SC2016 # a linha é para a pessoa copiar, com $HOME e $PATH literais
  line='export PATH="$HOME/.local/bin:$PATH"'
  echo
  say path_missing "$BIN_DIR"
  printf '  %s\n' "$line"
  say path_forever "$(shell_rc)"
  printf '  %s\n' "$line"
}

run_doctor() {
  echo
  say doctor
  # O doctor sai com erro quando falta algo obrigatório (Docker, Git): a instalação já terminou e o
  # próprio relatório diz o que fazer.
  "$BIN_DIR/nants" doctor || true
  echo
  say done
}

main() {
  NANTS_LANG=$(detect_lang)
  RELEASES_URL=${NANTS_RELEASES_URL:-$DEFAULT_RELEASES_URL}
  RELEASES_URL=${RELEASES_URL%/}
  WANTED=${NANTS_VERSION:-}
  WANTED=${WANTED#v}
  detect_platform
  require_tools
  if [ -n "$WANTED" ] && ! is_version "$WANTED"; then
    die bad_version "$WANTED"
  fi
  [ -n "${HOME:-}" ] || die install_failed "~/.local/bin/nants"
  TMP=$(mktemp -d)
  trap 'rm -rf "$TMP"' EXIT
  trap 'exit 130' INT TERM
  resolve_package
  download_and_verify
  install_binary
  path_hint
  run_doctor
}

main "$@"
