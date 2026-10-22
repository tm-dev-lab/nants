#!/usr/bin/env bash
# Roda um comando de teste com uma pasta temporária só dele, apagada no fim, passe ou falhe.
#
# Uso: scripts/test.sh [comando...]   (sem argumentos: cargo test)
#   scripts/test.sh
#   scripts/test.sh cargo test --lib runs::
#   (cd web && ../scripts/test.sh npx vitest run --maxWorkers=2)
#
# Por quê: os testes criam pastas em std::env::temp_dir() / os.tmpdir() e não as apagam. Em
# 2026-10-21 isso acumulou 1,13 milhão de pastas (cerca de 178 GB) em /tmp e travou o WSL.
# Os dois respeitam TMPDIR, então cada rodada ganha a própria pasta, removida na saída.
# A pasta fica fora do repositório de propósito: dentro dele, testes que rodam git numa pasta
# temporária enxergariam o repositório do Nants.
set -u

BASE="${NANTS_TEST_TMP_BASE:-/tmp/nants-test-runs}"
mkdir -p "$BASE"

# Sobras de rodadas mortas à força (kill, travamento) com mais de 1 dia.
find "$BASE" -mindepth 1 -maxdepth 1 -type d -mmin +1440 -exec rm -rf {} + 2>/dev/null

RUN_DIR="$(mktemp -d "$BASE/run-XXXXXX")"
cleanup() { rm -rf "$RUN_DIR"; }
trap cleanup EXIT
trap 'exit 130' INT TERM

export TMPDIR="$RUN_DIR"
export PATH="$HOME/.cargo/bin:$PATH"

if [ "$#" -eq 0 ]; then
  set -- cargo test
fi
"$@"
