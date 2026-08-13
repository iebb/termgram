#!/usr/bin/env bash

set -euo pipefail

readonly REPOSITORY="iebb/termgram"
readonly RELEASES_API="https://api.github.com/repos/${REPOSITORY}/releases"

if (($# != 0)); then
  printf 'error: use CHANNEL and INSTALL_DIR environment variables; arguments are not supported\n' >&2
  exit 2
fi

channel="${CHANNEL:-stable}"
case "$channel" in
  stable | prerelease) ;;
  *)
    printf 'error: CHANNEL must be stable or prerelease\n' >&2
    exit 2
    ;;
esac

os_name="$(uname -s)"
architecture="$(uname -m)"
case "$os_name" in
  Linux)
    case "$architecture" in
      x86_64) platform="linux" ;;
      aarch64 | arm64) platform="linux-aarch64" ;;
      *)
        printf 'error: Linux release binaries require x86_64 or ARM64; detected %s\n' "$architecture" >&2
        exit 1
        ;;
    esac
    ;;
  Darwin)
    case "$architecture" in
      arm64 | aarch64) platform="macos" ;;
      x86_64) platform="macos-x86_64" ;;
      *)
        printf 'error: macOS release binaries require Apple silicon or Intel x86_64; detected %s\n' "$architecture" >&2
        exit 1
        ;;
    esac
    ;;
  *)
    printf 'error: Termgram release binaries do not support %s\n' "$os_name" >&2
    exit 1
    ;;
esac

if [[ -n "${INSTALL_DIR:-}" ]]; then
  install_dir="$INSTALL_DIR"
else
  if [[ -z "${HOME:-}" ]]; then
    printf 'error: HOME is unset; set INSTALL_DIR explicitly\n' >&2
    exit 1
  fi
  install_dir="$HOME/.local/bin"
fi

for command_name in curl tar mktemp; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    printf 'error: required command not found: %s\n' "$command_name" >&2
    exit 1
  fi
done

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/termgram-install.XXXXXXXX")"
stage_path=
cleanup() {
  if [[ -n "$stage_path" ]]; then
    rm -f -- "$stage_path"
  fi
  rm -rf -- "$work_dir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM

download() {
  local url="$1"
  local destination="$2"
  local max_bytes="$3"
  curl \
    --disable \
    --fail \
    --location \
    --silent \
    --show-error \
    --proto '=https' \
    --proto-redir '=https' \
    --tlsv1.2 \
    --connect-timeout 10 \
    --max-time 300 \
    --retry 3 \
    --max-filesize "$max_bytes" \
    --header 'Accept: application/vnd.github+json' \
    --header 'X-GitHub-Api-Version: 2022-11-28' \
    --output "$destination" \
    "$url"
}

best_tag=
best_height=-1
page=1
while :; do
  releases_json="$work_dir/releases-$page.json"
  download "${RELEASES_API}?per_page=100&page=$page" "$releases_json" 2097152

  release_count=0
  current_tag=
  current_height=
  while IFS= read -r field; do
    if [[ "$field" =~ ^[[:space:]]*\"tag_name\"[[:space:]]*: ]]; then
      release_count=$((release_count + 1))
      current_tag=
      current_height=
      if [[ "$field" =~ ^[[:space:]]*\"tag_name\"[[:space:]]*:[[:space:]]*\"(v0\.1\.(0|[1-9][0-9]*))\" ]]; then
        current_tag="${BASH_REMATCH[1]}"
        current_height="${BASH_REMATCH[2]}"
      fi
    elif [[ "$field" =~ ^[[:space:]]*\"prerelease\"[[:space:]]*:[[:space:]]*(true|false) ]]; then
      is_prerelease="${BASH_REMATCH[1]}"
      if [[ -n "$current_tag" ]] &&
        { [[ "$channel" == prerelease ]] || [[ "$is_prerelease" == false ]]; } &&
        ((10#$current_height > best_height)); then
        best_tag="$current_tag"
        best_height=$((10#$current_height))
      fi
      current_tag=
      current_height=
    fi
  done < <(tr ',' '\n' < "$releases_json")

  if ((release_count < 100)); then
    break
  fi
  page=$((page + 1))
done

if [[ -z "$best_tag" ]]; then
  printf 'error: no %s Termgram release is available\n' "$channel" >&2
  exit 1
fi

version="${best_tag#v}"
asset="termgram-${version}-${platform}.tar.gz"
release_url="https://github.com/${REPOSITORY}/releases/download/${best_tag}"
archive_path="$work_dir/$asset"
checksums_path="$work_dir/SHA256SUMS"

download "$release_url/SHA256SUMS" "$checksums_path" 65536
download "$release_url/$asset" "$archive_path" 134217728

expected_hash=
checksum_matches=0
while read -r checksum filename extra; do
  filename="${filename#\*}"
  filename="${filename%$'\r'}"
  if [[ "$filename" == "$asset" && -z "${extra:-}" ]]; then
    if [[ ! "$checksum" =~ ^[[:xdigit:]]{64}$ ]]; then
      printf 'error: invalid checksum for %s\n' "$asset" >&2
      exit 1
    fi
    expected_hash="$(printf '%s' "$checksum" | tr '[:upper:]' '[:lower:]')"
    checksum_matches=$((checksum_matches + 1))
  fi
done < "$checksums_path"

if ((checksum_matches != 1)); then
  printf 'error: SHA256SUMS must contain exactly one entry for %s\n' "$asset" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  actual_hash="$(sha256sum "$archive_path")"
elif command -v shasum >/dev/null 2>&1; then
  actual_hash="$(shasum -a 256 "$archive_path")"
else
  printf 'error: sha256sum or shasum is required to verify the download\n' >&2
  exit 1
fi
actual_hash="${actual_hash%% *}"
actual_hash="$(printf '%s' "$actual_hash" | tr '[:upper:]' '[:lower:]')"
if [[ "$actual_hash" != "$expected_hash" ]]; then
  printf 'error: checksum verification failed for %s\n' "$asset" >&2
  exit 1
fi

archive_members="$(tar -tzf "$archive_path")"
if [[ "$archive_members" != tg ]]; then
  printf 'error: release archive must contain only a root-level tg binary\n' >&2
  exit 1
fi

extract_dir="$work_dir/extracted"
mkdir "$extract_dir"
tar -xzf "$archive_path" -C "$extract_dir" tg
if [[ ! -f "$extract_dir/tg" || -L "$extract_dir/tg" ]]; then
  printf 'error: release archive did not contain a regular tg binary\n' >&2
  exit 1
fi

mkdir -p -- "$install_dir"
install_dir="$(cd -P -- "$install_dir" && pwd)"
target_path="$install_dir/tg"
if [[ -L "$target_path" ]] || { [[ -e "$target_path" ]] && [[ ! -f "$target_path" ]]; }; then
  printf 'error: install target is not a regular file: %s\n' "$target_path" >&2
  exit 1
fi

stage_path="$(mktemp "$install_dir/.tg.install.XXXXXXXX")"
cp "$extract_dir/tg" "$stage_path"
chmod 0755 "$stage_path"
mv -f "$stage_path" "$target_path"
stage_path=

printf 'Installed tg %s to %s\n' "$version" "$target_path"

path_contains_install_dir=false
IFS=: read -r -a path_entries <<< "${PATH:-}"
for path_entry in "${path_entries[@]}"; do
  if [[ "$path_entry" == "$install_dir" ]]; then
    path_contains_install_dir=true
    break
  fi
done
if [[ "$path_contains_install_dir" == false ]]; then
  printf 'Add %s to PATH, then run: tg\n' "$install_dir"
fi
