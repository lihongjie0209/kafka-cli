#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
    echo "usage: $0 /absolute/path/to/kafka" >&2
    exit 2
fi

binary=$1
directory=$(dirname "$binary")

for name in \
    kafka-topics kafka-console-producer kafka-producer-perf-test kafka-e2e-latency kafka-console-consumer kafka-consumer-perf-test kafka-console-share-consumer \
    kafka-share-consumer-perf-test kafka-verifiable-share-consumer \
    kafka-consumer-groups kafka-groups kafka-share-groups kafka-streams-groups \
    kafka-streams-application-reset kafka-configs kafka-client-metrics \
    kafka-features kafka-transactions kafka-metadata-quorum \
    kafka-delegation-tokens kafka-get-offsets kafka-acls \
    kafka-reassign-partitions kafka-delete-records kafka-leader-election \
    kafka-log-dirs kafka-broker-api-versions kafka-cluster
do
    ln -sf "$(basename "$binary")" "$directory/$name"
    ln -sf "$(basename "$binary")" "$directory/$name.sh"
done
