#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
fixture="$(mktemp -d "${TMPDIR:-/tmp}/muse-clean-test.XXXXXX")"
cleanup() {
  rm -rf -- "${fixture}"
}
trap cleanup EXIT

mkdir -p "${fixture}/scripts" "${fixture}/node_modules/.vite" \
  "${fixture}/dist" "${fixture}/coverage" \
  "${fixture}/.agent-vp-data/sessions" "${fixture}/target/debug"
cp "${repo_root}/scripts/clean-build-artifacts.sh" "${fixture}/scripts/"
printf '[workspace]\nmembers = []\n' >"${fixture}/Cargo.toml"
printf '{"private":true}\n' >"${fixture}/package.json"
printf 'keep\n' >"${fixture}/.agent-vp-data/sessions/runtime.jsonl"
printf 'remove\n' >"${fixture}/target/debug/output"
printf 'remove\n' >"${fixture}/dist/index.html"

bash "${fixture}/scripts/clean-build-artifacts.sh" >/dev/null
[[ ! -e "${fixture}/target" ]]
[[ ! -e "${fixture}/dist" ]]
[[ ! -e "${fixture}/node_modules/.vite" ]]
[[ ! -e "${fixture}/coverage" ]]
[[ -f "${fixture}/.agent-vp-data/sessions/runtime.jsonl" ]]

mkdir -p "${fixture}/outside"
ln -s "${fixture}/outside" "${fixture}/dist"
if bash "${fixture}/scripts/clean-build-artifacts.sh" >/dev/null 2>&1; then
  echo "符号链接构建目录必须被拒绝。" >&2
  exit 1
fi
[[ -d "${fixture}/outside" ]]

rm "${fixture}/dist"
mv "${fixture}/node_modules" "${fixture}/node_modules-real"
ln -s "${fixture}/node_modules-real" "${fixture}/node_modules"
if bash "${fixture}/scripts/clean-build-artifacts.sh" >/dev/null 2>&1; then
  echo "符号链接父目录必须被拒绝。" >&2
  exit 1
fi
[[ -f "${fixture}/package.json" ]]

if bash "${fixture}/scripts/clean-build-artifacts.sh" unexpected >/dev/null 2>&1; then
  echo "清理脚本必须拒绝任意参数。" >&2
  exit 1
fi

printf '构建产物清理脚本测试通过。\n'
