#!/bin/bash
set -u

mkdir -p /sys/fs/cgroup/init

# Move every process out of the root cgroup (retry to catch races)
for _ in 1 2 3 4 5; do
    for pid in $(cat /sys/fs/cgroup/cgroup.procs 2>/dev/null); do
        echo "$pid" > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null || true
    done
done

remaining=$(wc -l < /sys/fs/cgroup/cgroup.procs)
if [ "$remaining" -gt 0 ]; then
    echo "warning: $remaining processes still in root cgroup:"
    cat /sys/fs/cgroup/cgroup.procs
fi

echo "+memory +cpu +pids" > /sys/fs/cgroup/cgroup.subtree_control \
    && echo "cgroup setup complete: $(cat /sys/fs/cgroup/cgroup.subtree_control)" \
    || echo "ERROR: failed to enable controllers (still procs in root cgroup)"
