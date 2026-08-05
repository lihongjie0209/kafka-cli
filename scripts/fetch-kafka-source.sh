#!/bin/sh
# Fetch or refresh a shallow sparse clone of Apache Kafka for local reference.
# Default target: sibling directory ../kafka relative to this repo, or KAFKA_SRC.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
default_target=$(CDPATH= cd -- "$repo_root/.." && pwd)/kafka
target=${1:-${KAFKA_SRC:-$default_target}}
remote_url=${KAFKA_GIT_URL:-https://github.com/apache/kafka.git}
branch=${KAFKA_GIT_BRANCH:-trunk}

sparse_paths='
bin
tools/src
clients/src/main/java/org/apache/kafka/clients
clients/src/main/java/org/apache/kafka/common
core/src/main
shell/src
server-common/src
server/src
group-coordinator/src
metadata/src
storage/src
connect/runtime/src
connect/mirror/src
streams/src/main
docs
'

if [ -d "$target/.git" ]; then
    echo "Refreshing existing clone at $target"
    git -C "$target" remote set-url origin "$remote_url"
    git -C "$target" fetch --depth 1 origin "$branch"
    git -C "$target" checkout -B "$branch" "FETCH_HEAD"
    git -C "$target" sparse-checkout set $sparse_paths
else
    echo "Cloning $remote_url ($branch) into $target"
    parent=$(dirname "$target")
    mkdir -p "$parent"
    git clone \
        --depth 1 \
        --single-branch \
        --branch "$branch" \
        --filter=blob:none \
        --sparse \
        "$remote_url" \
        "$target"
    git -C "$target" sparse-checkout set $sparse_paths
fi

echo "---"
echo "path:    $target"
echo "branch:  $(git -C "$target" rev-parse --abbrev-ref HEAD)"
echo "commit:  $(git -C "$target" rev-parse HEAD)"
echo "short:   $(git -C "$target" rev-parse --short HEAD)"
echo "size:    $(du -sh "$target" | cut -f1)"
echo "scripts: $(find "$target/bin" -name '*.sh' 2>/dev/null | wc -l | tr -d ' ')"
echo
echo "export KAFKA_SRC=$target"
