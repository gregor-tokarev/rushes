#!/usr/bin/env bash
# Prepare a release of the checked-out commit, if it has anything new: the daily
# release workflow runs this on main, then pushes what it made.
#
#   scripts/daily-release.sh
#
# Prints nothing when no commit was made since the newest release tag. Otherwise it
# tags v<version> and prints that tag. When the workspace version in Cargo.toml has
# been released already, it first bumps its patch number in Cargo.toml and Cargo.lock
# and commits that; a version bumped by hand and not released yet is tagged as it is.
# Needs git with a committer identity and cargo (to update Cargo.lock).
set -euo pipefail

cd "$(dirname "$0")/.."

last=$(git tag --list 'v[0-9]*' --merged HEAD --sort=-v:refname | head -1)
if [[ -n "$last" && $(git rev-list --count "$last..HEAD") -eq 0 ]]; then
  echo "Nothing new since $last." >&2
  exit 0
fi

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if git rev-parse -q --verify "refs/tags/v$version" > /dev/null; then
  if [[ ! "$version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    echo "Can't bump version $version: expected <major>.<minor>.<patch>." >&2
    exit 1
  fi
  released=$version
  version=${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.$((BASH_REMATCH[3] + 1))
  sed -i "0,/^version = \"$released\"/s//version = \"$version\"/" Cargo.toml
  # Rewrites only the workspace's own packages in Cargo.lock.
  cargo update --workspace --quiet
  git commit --quiet -m "chore(release): $version" Cargo.toml Cargo.lock
  echo "Bumped $released to $version." >&2
fi

if git rev-parse -q --verify "refs/tags/v$version" > /dev/null; then
  echo "Tag v$version exists already, on another commit." >&2
  exit 1
fi
git tag -a "v$version" -m "VideoCRM $version"
echo "Tagged v$version." >&2
echo "v$version"
