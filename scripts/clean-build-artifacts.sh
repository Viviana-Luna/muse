#!/usr/bin/env bash
# 清理仓库内可再生成的 Rust 与前端构建产物，不接受外部路径参数。

set -euo pipefail

if [[ "$#" -ne 0 ]]; then
  echo "该脚本不接受参数，只清理当前 Muse 仓库的固定构建目录。" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd "${script_dir}/.." && pwd -P)"

if [[ ! -f "${repo_root}/Cargo.toml" || ! -f "${repo_root}/package.json" ]]; then
  echo "拒绝清理：脚本所在目录不是完整的 Muse 仓库。" >&2
  exit 3
fi

size_kib() {
  du -sk "$1" 2>/dev/null | awk '{print $1}' || printf '0\n'
}

assert_owned_directory() {
  local path="$1"
  case "${path}" in
    "${repo_root}"/*) ;;
    *)
      echo "拒绝清理仓库外路径：${path}" >&2
      exit 4
      ;;
  esac
  local relative="${path#"${repo_root}/"}"
  local current="${repo_root}"
  local component
  IFS='/' read -r -a components <<<"${relative}"
  for component in "${components[@]}"; do
    current="${current}/${component}"
    if [[ -L "${current}" ]]; then
      echo "拒绝清理包含符号链接的目录：${current}" >&2
      exit 5
    fi
  done
}

targets=(
  "${repo_root}/target"
  "${repo_root}/dist"
  "${repo_root}/node_modules/.vite"
  "${repo_root}/coverage"
)

before_kib=0
for target in "${targets[@]}"; do
  assert_owned_directory "${target}"
  before_kib=$((before_kib + $(size_kib "${target}")))
done

if [[ -d "${repo_root}/target" ]]; then
  CARGO_TARGET_DIR="${repo_root}/target" \
    cargo clean --manifest-path "${repo_root}/Cargo.toml"
fi
for target in "${targets[@]:1}"; do
  if [[ -d "${target}" ]]; then
    rm -rf -- "${target}"
  fi
done

after_kib=0
for target in "${targets[@]}"; do
  after_kib=$((after_kib + $(size_kib "${target}")))
done
released_kib=$((before_kib - after_kib))

printf '清理前构建产物：%s KiB\n' "${before_kib}"
printf '清理后构建产物：%s KiB\n' "${after_kib}"
printf '本次释放空间：%s KiB\n' "${released_kib}"
