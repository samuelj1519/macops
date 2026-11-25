#!/bin/sh
set -eu

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

mode=source
case "$#:${1-}" in
  0:|1:--source) ;;
  1:--release) mode=release ;;
  1:--help|1:-h)
    printf '%s\n' '用法：./install.sh [--source | --release]' \
      '  默认从当前源码编译安装。' \
      '  --release 安装 GitHub 上最近一次正式发布。' \
      '  适用于 Apple Silicon Mac，安装到当前用户目录。'
    exit 0 ;;
  *) fail '请运行 ./install.sh --help 查看用法。' ;;
esac

[ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] ||
  fail '请在 Apple Silicon Mac 的原生终端运行此脚本。'
[ -n "${HOME-}" ] && [ -d "$HOME" ] || fail '找不到当前用户的主目录。'
user_home=$(CDPATH='' cd -P -- "$HOME" && pwd)
local_dir="$user_home/.local"
bin="$local_dir/bin"
libexec="$local_dir/libexec"
target="$libexec/macops"

umask 077
work=$(mktemp -d "${TMPDIR:-/tmp}/macops-install.XXXXXXXX") || fail '无法创建安装用的临时目录。'
staged=''
cleanup() {
  if [ -n "$staged" ]; then
    rm -f -- "$staged"
  fi
  rm -rf -- "$work"
}
trap cleanup 0
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

printf '%s\n' '#!/bin/sh' 'exec "$HOME/.local/libexec/macops" "$@"' > "$work/wrapper"

known_alias() {
  if [ -L "$1" ]; then
    link=$(readlink "$1") || return 1
    [ "$link" = ../libexec/macops ] || [ "$link" = "$target" ]
  else
    [ -f "$1" ] && [ -x "$1" ] && cmp -s "$1" "$work/wrapper"
  fi
}

check_paths() {
  for directory in "$local_dir" "$bin" "$libexec"; do
    if [ -L "$directory" ] || { [ -e "$directory" ] && [ ! -d "$directory" ]; }; then
      fail "安装需要一个普通目录：$directory"
    fi
  done
  if [ -L "$target" ] || { [ -e "$target" ] && [ ! -f "$target" ]; }; then
    fail "这个位置不能用来替换程序：$target"
  fi
  for name in mo macops; do
    alias_path="$bin/$name"
    if [ -e "$alias_path" ] || [ -L "$alias_path" ]; then
      known_alias "$alias_path" || fail "这个命令名已经被占用：${alias_path}。请先检查它的用途。"
    fi
  done
  if [ -e "$target" ] && ! known_alias "$bin/mo" && ! known_alias "$bin/macops"; then
    fail "这个位置已有文件，但找不到对应的 mo 或 macops 命令：${target}。请先检查这个文件的用途。"
  fi
}

fetch() {
  curl --disable --fail --silent --show-error --location --proto '=https' \
    --proto-redir '=https' --connect-timeout 15 --max-time 300 --output "$2" "$1"
}

check_paths
expected_version=''
if [ "$mode" = release ]; then
  releases=https://github.com/samuelj1519/macops/releases
  printf '%s\n' '正在下载正式发布包。'
  fetch "$releases/latest/download/SHA256SUMS" "$work/SHA256SUMS" ||
    fail '没能下载校验文件。请检查网络后再试。'
  checksum_record=$(LC_ALL=C awk '
    NF == 2 && length($1) == 64 && $1 ~ /^[0-9a-f]+$/ &&
      $2 ~ /^macops-v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-darwin-arm64\.tar\.gz$/ {
        checksum = $1; asset = $2; matches++
      }
    END { if (matches != 1) exit 1; print checksum, asset }
  ' "$work/SHA256SUMS") || fail '找不到唯一可校验的 Apple Silicon 安装包。'
  expected_hash=${checksum_record%% *}
  archive=${checksum_record#* }
  tag=${archive#macops-}
  tag=${tag%-darwin-arm64.tar.gz}
  expected_version="macops ${tag#v}"
  fetch "$releases/download/$tag/$archive" "$work/$archive" ||
    fail '没能下载正式安装包。请检查网络后再试。'
  checksum=$(shasum -a 256 < "$work/$archive") || fail '没能校验安装包。'
  [ "${checksum%% *}" = "$expected_hash" ] || fail '安装包校验失败。请重新下载。'
  binary="$work/macops"
  tar -xOzf "$work/$archive" macops > "$binary" || fail '没能从安装包取出 macops。'
  chmod u+x "$binary"
else
  source_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
  [ -f "$source_dir/Cargo.toml" ] && [ -f "$source_dir/rust-toolchain.toml" ] ||
    fail '找不到源码。请从源码目录运行 ./install.sh，或用 --release 安装正式发布。'
  command -v cargo >/dev/null 2>&1 || fail '从源码安装需要 Rust 和 Cargo。也可以用 --release 安装正式发布。'
  printf '%s\n' '正在编译当前源码。'
  (cd "$source_dir" && cargo build --release --locked --bin macops --target-dir "$work/target") ||
    fail '编译没有完成。请查看上面的错误。'
  binary="$work/target/release/macops"
fi

file -b "$binary" | grep -Fq 'Mach-O 64-bit executable arm64' ||
  fail '拿到的程序不适用于 Apple Silicon Mac。'
version=$("$binary" --version) || fail '新程序没能运行。'
case "$version" in
  'macops '[0-9]*) ;;
  *) fail '没能确认新程序是 macops。' ;;
esac
[ -z "$expected_version" ] || [ "$version" = "$expected_version" ] ||
  fail '安装包里的版本和发布版本不一致。'

check_paths
mkdir -p -- "$bin" "$libexec" || fail '无法创建用户安装目录。'
staged=$(mktemp "$libexec/.macops.XXXXXXXX") || fail '无法准备新程序。'
install -m 755 "$binary" "$staged" || fail '无法准备新程序。'
check_paths
for name in mo macops; do
  alias_path="$bin/$name"
  if ! known_alias "$alias_path"; then
    ln -sh -- ../libexec/macops "$alias_path" 2>/dev/null || known_alias "$alias_path" ||
      fail "没能建立命令 ${alias_path}。请检查这个位置后重跑安装。"
  fi
done
mv -fh -- "$staged" "$target" || fail '没能放好新程序。请重跑安装。'
for name in mo macops; do
  alias_path="$bin/$name"
  [ "$("$alias_path" --version)" = "$version" ] || fail "没能验证命令 ${alias_path}。"
done

printf '安装完成 · %s\n' "$version"
if [ "$(command -v mo || true)" != "$bin/mo" ]; then
  printf '%s\n' '要在当前终端直接用 mo，运行：' '  export PATH="$HOME/.local/bin:$PATH"'
fi
