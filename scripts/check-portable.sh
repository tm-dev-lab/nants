#!/bin/sh
# Confere o executável portátil do Nants em distribuições antigas e atuais (spec 103, decisão 4).
#
# Uso: scripts/check-portable.sh [caminho/do/nants] [imagem ...]
#   padrão: target/x86_64-unknown-linux-gnu/release/nants em ubuntu:20.04 e debian:stable.
#
# Em cada imagem: `ldd --version` (glibc da distro), `nants --version` e uma chamada HTTPS real
# (o item de versão do `nants doctor` consulta NANTS_CHECK_HTTPS_URL, padrão https://www.google.com;
# um "HTTP 404" no detalhe prova que a conexão TLS foi feita). Só precisa do Docker, sem sudo.
set -eu

main() {
  root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
  bin=${1:-$root/target/x86_64-unknown-linux-gnu/release/nants}
  [ $# -gt 0 ] && shift
  if [ $# -eq 0 ]; then
    set -- ubuntu:20.04 debian:stable
  fi
  if [ ! -x "$bin" ]; then
    echo "check-portable: executável não encontrado: $bin" >&2
    exit 1
  fi
  bin=$(CDPATH= cd -- "$(dirname -- "$bin")" && pwd)/$(basename -- "$bin")
  https_url=${NANTS_CHECK_HTTPS_URL:-https://www.google.com}
  status=0
  for img in "$@"; do
    echo "=== $img"
    out=$(docker run --rm -e "NANTS_RELEASES_URL=$https_url" -v "$bin:/usr/local/bin/nants:ro" "$img" \
      sh -c 'ldd --version 2>&1 | head -n 1; nants --version; nants doctor 2>&1 | grep "Versão do Nants" || true' 2>&1) || {
      echo "$out"
      echo "FALHA: o executável não rodou em $img"
      status=1
      continue
    }
    echo "$out"
    case "$out" in
      *"sem conexão com o servidor de versões"*)
        echo "FALHA: a chamada HTTPS não completou em $img"
        status=1
        ;;
      *"Versão do Nants"*) echo "OK: $img" ;;
      *)
        echo "FALHA: saída inesperada em $img"
        status=1
        ;;
    esac
  done
  exit "$status"
}

main "$@"
