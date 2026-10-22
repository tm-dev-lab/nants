#!/bin/sh
# Teste real do instalador num container limpo (spec 103, T04, critério G1).
#
# Uso: scripts/check-install.sh [caminho/do/nants] [imagem]
#   padrão: o executável portátil (target/x86_64-unknown-linux-gnu/release/nants) ou, sem ele,
#   target/release/nants; imagem ubuntu:24.04.
#
# O que faz:
#   1. empacota o executável como o scripts/package.sh e a Release do GitHub
#      (nants-X.Y.Z-x86_64-linux.tar.gz, com o SHA-256) e
#      gera o latest.json com as URLs locais;
#   2. serve o install.sh, o latest.json e o pacote por HTTP local (python3, porta
#      NANTS_CHECK_PORT, padrão 18765; nunca 3000 a 3010);
#   3. num container novo da imagem, só com o curl instalado, cria um usuário comum e roda
#      `curl -fsSL http://127.0.0.1:PORTA/install.sh | sh` como ele: confere a detecção, o
#      download, o SHA-256, o aviso de PATH, o nants --version e o doctor apontando a falta do
#      Docker; depois adultera o pacote e confere que a reinstalação recusa pelo SHA-256; por fim,
#      roda nants uninstall sem terminal (recusa sem apagar nada);
#   4. no fim, remove o container (--rm), a pasta temporária e a imagem, se ela não existia antes.
#
# Só precisa do Docker (sem sudo) e do python3 no computador. Sai com 0 se tudo conferir.
set -eu

check() {
  # $1 = descrição, $2 = texto procurado na saída.
  if printf '%s\n' "$OUT" | grep -qF -- "$2"; then
    echo "OK: $1"
  else
    echo "FALHA: $1 (não achei: $2)"
    STATUS=1
  fi
}

main() {
  root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
  bin=${1:-}
  if [ -z "$bin" ]; then
    bin=$root/target/x86_64-unknown-linux-gnu/release/nants
    [ -x "$bin" ] || bin=$root/target/release/nants
  fi
  image=${2:-ubuntu:24.04}
  port=${NANTS_CHECK_PORT:-18765}
  case $port in 300[0-9] | 3010) echo "check-install: a porta $port é reservada ao Nants; use outra" >&2; exit 1 ;; esac
  [ -x "$bin" ] || { echo "check-install: executável não encontrado: $bin" >&2; exit 1; }
  command -v python3 >/dev/null 2>&1 || { echo "check-install: precisa do python3 para servir os arquivos" >&2; exit 1; }

  version=$("$bin" --version | sed -n 's/^nants //p')
  [ -n "$version" ] || { echo "check-install: não consegui ler a versão de $bin" >&2; exit 1; }
  work=$(mktemp -d)
  server_pid=""
  had_image=0
  docker image inspect "$image" >/dev/null 2>&1 && had_image=1
  cleanup() {
    [ -z "$server_pid" ] || kill "$server_pid" 2>/dev/null || true
    rm -rf "$work"
    if [ "$had_image" -eq 0 ]; then
      docker rmi "$image" >/dev/null 2>&1 || true
    fi
  }
  trap cleanup EXIT
  trap 'exit 130' INT TERM

  # 1. Pacote e manifesto, como o scripts/package.sh.
  www=$work/www
  mkdir -p "$www/v$version" "$work/stage"
  cp "$bin" "$work/stage/nants"
  chmod 755 "$work/stage/nants"
  file="nants-$version-x86_64-linux.tar.gz"
  tar --owner=0 --group=0 --numeric-owner -C "$work/stage" -cf - nants | gzip -n -9 >"$www/v$version/$file"
  sum=$(sha256sum "$www/v$version/$file" | awk '{print $1}')
  printf '%s  %s\n' "$sum" "$file" >"$www/v$version/$file.sha256"
  url="http://127.0.0.1:$port"
  cat >"$www/latest.json" <<EOF
{
  "version": "$version",
  "date": "2026-10-21T12:00:00Z",
  "assets": {
    "x86_64-linux": { "url": "$url/v$version/$file", "sha256": "$sum" }
  }
}
EOF
  cp "$root/scripts/install.sh" "$www/install.sh"
  chmod -R a+rwX "$www"
  echo "check-install: nants $version, pacote $file (sha256 $sum), imagem $image, $url"

  # 2. Servidor local.
  python3 -m http.server "$port" --bind 127.0.0.1 --directory "$www" >"$work/http.log" 2>&1 &
  server_pid=$!
  i=0
  until curl -fsS "$url/latest.json" >/dev/null 2>&1; do
    i=$((i + 1))
    [ "$i" -lt 50 ] || { echo "check-install: o servidor local não subiu"; cat "$work/http.log"; exit 1; }
    sleep 0.1
  done

  # 3. Container limpo, só com o curl.
  STATUS=0
  OUT=$(docker run --rm --network host --label nants.project=check-install \
    -v "$www:/www" -e "URL=$url" -e "FILE=v$version/$file" "$image" sh -c '
      set -u
      export DEBIAN_FRONTEND=noninteractive
      apt-get update -qq >/dev/null && apt-get install -y -qq --no-install-recommends curl ca-certificates >/dev/null
      useradd -m -s /bin/bash pessoa
      echo "=== sem docker no container: $(command -v docker || echo ausente)"
      echo "=== instalação"
      su pessoa -c "cd ~ && LANG=pt_BR.UTF-8 NANTS_RELEASES_URL=$URL curl -fsSL $URL/install.sh | LANG=pt_BR.UTF-8 NANTS_RELEASES_URL=$URL sh"
      echo "=== código de saída da instalação: $?"
      echo "=== executável: $(ls -l /home/pessoa/.local/bin/nants)"
      echo "=== versão: $(su pessoa -c "/home/pessoa/.local/bin/nants --version")"
      echo "=== conteúdo de .local/bin: $(ls -A /home/pessoa/.local/bin | tr "\n" " ")"
      echo "=== pacote adulterado"
      printf "lixo" >> "/www/$FILE"
      su pessoa -c "LANG=pt_BR.UTF-8 NANTS_RELEASES_URL=$URL curl -fsSL $URL/install.sh | LANG=pt_BR.UTF-8 NANTS_RELEASES_URL=$URL sh"
      echo "=== código de saída com SHA errado: $?"
      echo "=== uninstall sem terminal"
      su pessoa -c "LANG=pt_BR.UTF-8 /home/pessoa/.local/bin/nants uninstall </dev/null"
      echo "=== código de saída do uninstall: $?; executável ainda lá: $(test -x /home/pessoa/.local/bin/nants && echo sim || echo nao)"
    ' 2>&1) || STATUS=1
  printf '%s\n' "$OUT"
  echo "=== conferência"
  check "detecção do sistema" "Sistema: Linux, x86_64"
  check "download e SHA-256" "SHA-256 conferido."
  check "instalado em ~/.local/bin" "Nants $version instalado em /home/pessoa/.local/bin/nants"
  check "aviso de PATH" "/home/pessoa/.local/bin não está no PATH"
  check "linha do PATH" 'export PATH="$HOME/.local/bin:$PATH"'
  check "doctor rodou" "diagnóstico do ambiente"
  check "doctor aponta a falta do Docker" "FALHA Docker CLI"
  check "doctor vê a versão publicada" "$version, a mais recente"
  check "convite para digitar nants" "Digite nants"
  check "nants --version" "=== versão: nants $version"
  check "só o executável em ~/.local/bin" "=== conteúdo de .local/bin: nants "
  check "SHA errado recusado" "O SHA-256 do pacote não confere"
  check "SHA errado sai com erro" "=== código de saída com SHA errado: 1"
  check "uninstall sem terminal recusa" "Sem terminal para confirmar"
  check "uninstall não apagou nada" "executável ainda lá: sim"
  if [ "$STATUS" -eq 0 ]; then
    echo "check-install: OK"
  else
    echo "check-install: FALHA"
  fi
  exit "$STATUS"
}

main "$@"
