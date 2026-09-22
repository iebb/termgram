#!/usr/bin/env bash
# Run after publishing, including plans without a new stable release.
set -euo pipefail

: "${GITHUB_REPOSITORY:?}"
: "${DEFAULT_BRANCH:?}"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
metadata_dir="$(mktemp -d)"
# shellcheck disable=SC2329 # Invoked by the EXIT trap.
cleanup() {
  if [[ -d "$metadata_dir/checkout" ]]; then
    git worktree remove --force "$metadata_dir/checkout"
  fi
  rm -rf -- "$metadata_dir"
}
trap 'cleanup' EXIT

# Already-published versions are omitted from later CI plans. Read releases
# independently so a failed metadata push can be repaired by any successful run.
gh api --paginate "repos/$GITHUB_REPOSITORY/releases?per_page=100" \
  --jq '.[] | select(.draft == false and .prerelease == false) | .tag_name | select(test("^v[0-9]+\\.[0-9]+\\.[0-9]+$"))' \
  > "$metadata_dir/stable-tags"
tag="$(sort -V "$metadata_dir/stable-tags" | tail -n 1)"
if [[ -z "$tag" ]]; then
  echo "No published stable release; leaving package metadata unchanged."
  exit 0
fi
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]

# Two CI runs can rebuild the same version with different archive timestamps.
# Only the checksums attached to the published release describe installed bytes.
gh release download "$tag" --repo "$GITHUB_REPOSITORY" \
  --pattern SHA256SUMS --dir "$metadata_dir"
python3 "$script_dir/update-package-metadata.py" \
  "${tag#v}" "$metadata_dir/SHA256SUMS" --output-dir "$metadata_dir/generated"

for attempt in 1 2 3; do
  git fetch origin "refs/heads/$DEFAULT_BRANCH"
  base_sha="$(git rev-parse FETCH_HEAD)"
  if [[ -d "$metadata_dir/checkout" ]]; then
    git -C "$metadata_dir/checkout" reset --hard "$base_sha"
  else
    git worktree add --detach "$metadata_dir/checkout" "$base_sha"
  fi
  mkdir -p "$metadata_dir/checkout/Formula" "$metadata_dir/checkout/bucket"
  cp "$metadata_dir/generated/Formula/termgram.rb" "$metadata_dir/checkout/Formula/termgram.rb"
  cp "$metadata_dir/generated/bucket/termgram.json" "$metadata_dir/checkout/bucket/termgram.json"
  git -C "$metadata_dir/checkout" add Formula/termgram.rb bucket/termgram.json
  if git -C "$metadata_dir/checkout" diff --cached --quiet; then
    echo "Package metadata already matches $tag."
    exit 0
  fi
  git -C "$metadata_dir/checkout" \
    -c user.name='github-actions[bot]' \
    -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
    commit -m "chore(release): Update package metadata for $tag"
  if git -C "$metadata_dir/checkout" push \
      "git@github.com:$GITHUB_REPOSITORY.git" "HEAD:refs/heads/$DEFAULT_BRANCH"; then
    exit 0
  fi
  echo "Metadata push attempt $attempt failed; retrying from the current branch." >&2
done

echo "Could not update package metadata after three attempts." >&2
exit 1
