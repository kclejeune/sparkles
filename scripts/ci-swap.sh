#!/usr/bin/env bash
# Adds swap for a CI job on a Linux runner, or removes what `add` added.
#
#   scripts/ci-swap.sh add      SWAP_GIB of swap, 16 unless set
#   scripts/ci-swap.sh remove   the swap that add recorded in CI_SWAP
#
# A compile can exceed the physical memory of a 16 GB runner, so a job adds swap. A
# runner with 24 GiB of memory or more gets none. On other runners the swap is a file in
# the root file system, unless the root is on device mapper. Some of GitHub's VMs mount
# the root on a device-mapper snapshot, and there writing a swapped page to the file
# waits on the snapshot's copy pool from inside memory reclaim, while completing the copy
# needs memory, and the whole VM hangs. Such a runner gets a compressed swap device in
# memory (zram) instead, or no added swap when the kernel cannot add one. `add` prints the
# swap and memory it ends with, since GitHub's image may enable swap of its own, which the
# script leaves alone.
set -euo pipefail

# The root file system is on a device-mapper device. The test reads the root's device
# number from the kernel rather than a printed device path, since a snapshot root can be
# mounted as /dev/root. It treats every device-mapper root as a possible snapshot, so it
# needs no dmsetup and fails toward zram when the root cannot be identified.
root_is_device_mapper() {
  local devnum
  devnum=$(mountpoint -d /) || return 0
  [ -d "/sys/dev/block/$devnum/dm" ]
}

add_zram() {
  local gib=$1 id
  [ -e /sys/class/zram-control/hot_add ] || sudo modprobe zram num_devices=0 2> /dev/null || true
  if ! id=$(sudo cat /sys/class/zram-control/hot_add 2> /dev/null); then
    echo '::warning::the root is on device mapper and the kernel adds no zram device, so no swap was added'
    return 0
  fi
  echo zstd | sudo tee "/sys/block/zram$id/comp_algorithm" > /dev/null 2>&1 || true
  if echo "${gib}G" | sudo tee "/sys/block/zram$id/disksize" > /dev/null &&
    sudo mkswap "/dev/zram$id" > /dev/null && sudo swapon "/dev/zram$id"; then
    echo "added ${gib} GiB of zram swap on /dev/zram$id"
    echo "CI_SWAP=/dev/zram$id" >> "${GITHUB_ENV:-/dev/null}"
  else
    echo '::warning::the runner refused a zram swap device'
    echo "$id" | sudo tee /sys/class/zram-control/hot_remove > /dev/null 2>&1 || true
  fi
}

add_file() {
  local gib=$1 swapfile
  # A name of its own, so a persistent runner never touches another job's swap file
  swapfile="/swapfile-ci-${GITHUB_RUN_ID:-0}-${GITHUB_RUN_ATTEMPT:-0}-${GITHUB_JOB:-job}"
  if ! sudo fallocate -l "${gib}G" "$swapfile"; then
    echo '::warning::the runner refused a swap file'
  elif ! { sudo chmod 600 "$swapfile" && sudo mkswap "$swapfile" > /dev/null &&
    sudo swapon "$swapfile"; }; then
    echo '::warning::the runner refused a swap file'
    sudo rm -f "$swapfile" || true
  else
    echo "added a ${gib} GiB swap file at $swapfile"
    echo "CI_SWAP=$swapfile" >> "${GITHUB_ENV:-/dev/null}"
  fi
}

case ${1:-} in
  add)
    gib=${SWAP_GIB:-16}
    memory_kib=$(awk '/^MemTotal:/ { print $2 }' /proc/meminfo)
    if [ "$memory_kib" -ge $((24 * 1024 * 1024)) ]; then
      echo "$((memory_kib / 1024 / 1024)) GiB of memory, so no swap"
    elif root_is_device_mapper; then
      echo "the root file system is on a device-mapper device, so the swap is zram, not a file"
      add_zram "$gib"
    else
      add_file "$gib"
    fi
    swapon --show
    free -h
    ;;
  remove)
    swap=${CI_SWAP:-}
    [ -n "$swap" ] || exit 0
    sudo swapoff "$swap" || true
    case $swap in
      /dev/zram*) echo "${swap#/dev/zram}" | sudo tee /sys/class/zram-control/hot_remove > /dev/null || true ;;
      *) sudo rm -f "$swap" || true ;;
    esac
    ;;
  *)
    echo "usage: $0 add|remove" >&2
    exit 2
    ;;
esac
