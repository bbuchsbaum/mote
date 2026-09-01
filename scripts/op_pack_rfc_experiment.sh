#!/usr/bin/env bash
set -euo pipefail

# Reproduce the Git/index and two-worktree merge evidence in op_packs_rfc.md.
# This is a format experiment, not a pack implementation: fixture pack names
# use SHA-256 from Python's standard library, while the RFC specifies BLAKE3.

ops_count=${OPS_COUNT:-2157}
batch_count=${BATCH_COUNT:-300}
benchmark_runs=${BENCHMARK_RUNS:-9}
fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/mote-op-pack-rfc.XXXXXX")

cleanup() {
  if [[ ${KEEP_FIXTURE:-0} == 1 ]]; then
    printf 'fixture_root=%s\n' "$fixture_root"
  else
    rm -rf "$fixture_root"
  fi
}
trap cleanup EXIT

init_repo() {
  local repo=$1
  mkdir -p "$repo/ops" "$repo/packs"
  git -C "$repo" -c init.defaultBranch=main init -q
  git -C "$repo" config user.name mote-rfc-fixture
  git -C "$repo" config user.email mote-rfc-fixture@example.invalid
}

make_ops() {
  local directory=$1
  local first=$2
  local count=$3
  python3 - "$directory" "$first" "$count" <<'PY'
import hashlib
import json
import pathlib
import sys

directory = pathlib.Path(sys.argv[1])
first = int(sys.argv[2])
count = int(sys.argv[3])
directory.mkdir(parents=True, exist_ok=True)
for number in range(first, first + count):
    op_id = f"fixture-op-{number:06d}"
    seed = hashlib.sha256(f"mote-pack-fixture-{number}".encode()).hexdigest()
    value = {
        "actor": "fixture",
        "kind": "note",
        "op": op_id,
        "payload": (seed * 63)[:3990],
        "sequence": number,
        "ts": "2030-01-01T00:00:00Z",
        "v": 1,
    }
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode() + b"\n"
    (directory / f"{number:06d}.json").write_bytes(encoded)
PY
}

pack_range() {
  local repo=$1
  local first=$2
  local last=$3
  python3 - "$repo" "$first" "$last" <<'PY'
import hashlib
import json
import pathlib
import sys

repo = pathlib.Path(sys.argv[1])
first = int(sys.argv[2])
last = int(sys.argv[3])
ops = repo / "ops"
packs = repo / "packs"
packs.mkdir(parents=True, exist_ok=True)
selected = [ops / f"{number:06d}.json" for number in range(first, last + 1)]

payload = bytearray()
entries = []
for path in selected:
    encoded = path.read_bytes()
    offset = len(payload)
    payload.extend(encoded)
    entries.append({
        "length": len(encoded),
        "offset": offset,
        "op": json.loads(encoded)["op"],
        "sha256": hashlib.sha256(encoded).hexdigest(),
    })

pack_hash = hashlib.sha256(payload).hexdigest()
pack_name = f"fixture-pack-sha256-{pack_hash}.jsonl"
(packs / pack_name).write_bytes(payload)
manifest = {
    "entries": entries,
    "fixture_hash": "sha256",
    "kind": "mote_op_pack_fixture",
    "pack": pack_name,
    "pack_sha256": pack_hash,
    "v": 1,
}
manifest_bytes = json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode() + b"\n"
manifest_hash = hashlib.sha256(manifest_bytes).hexdigest()
(packs / f"fixture-manifest-sha256-{manifest_hash}.json").write_bytes(manifest_bytes)

for path in selected:
    path.unlink()
PY
}

median_ms() {
  local repo=$1
  local mode=$2
  python3 - "$repo" "$mode" "$benchmark_runs" <<'PY'
import statistics
import subprocess
import sys
import time

repo, mode, runs = sys.argv[1], sys.argv[2], int(sys.argv[3])
samples = []
for _ in range(runs):
    if mode == "add":
        subprocess.run(["git", "reset", "-q"], cwd=repo, check=True)
        command = ["git", "add", "-A"]
    else:
        command = ["git", "status", "--porcelain"]
    started = time.perf_counter_ns()
    subprocess.run(command, cwd=repo, check=True, stdout=subprocess.DEVNULL)
    samples.append((time.perf_counter_ns() - started) / 1_000_000)
if mode == "add":
    subprocess.run(["git", "reset", "-q"], cwd=repo, check=True)
print(f"{statistics.median(samples):.3f}")
PY
}

path_count() {
  local repo=$1
  git -C "$repo" ls-files | wc -l | tr -d ' '
}

dirty_count() {
  local repo=$1
  git -C "$repo" status --porcelain | wc -l | tr -d ' '
}

index_bytes_after_add() {
  local repo=$1
  git -C "$repo" add -A
  wc -c < "$repo/.git/index" | tr -d ' '
  git -C "$repo" reset -q
}

tree_bytes() {
  local directory=$1
  python3 - "$directory" <<'PY'
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
print(sum(path.stat().st_size for path in root.rglob("*") if path.is_file()))
PY
}

logical_union() {
  local repo=$1
  python3 - "$repo" <<'PY'
import json
import pathlib
import sys

repo = pathlib.Path(sys.argv[1])
seen = {}
duplicates = 0
conflicts = 0
sources = sorted((repo / "ops").glob("*.json")) + sorted((repo / "packs").glob("*.jsonl"))
for source in sources:
    for encoded in source.read_bytes().splitlines(keepends=True):
        op_id = json.loads(encoded)["op"]
        previous = seen.get(op_id)
        if previous is None:
            seen[op_id] = encoded
        elif previous == encoded:
            duplicates += 1
        else:
            conflicts += 1
print(f"merge.logical_ops={len(seen)}")
print(f"merge.exact_duplicate_entries={duplicates}")
print(f"merge.conflicting_duplicate_entries={conflicts}")
PY
}

loose_repo="$fixture_root/loose"
packed_repo="$fixture_root/packed"
init_repo "$loose_repo"
make_ops "$loose_repo/ops" 1 "$ops_count"
git -C "$loose_repo" add -A
git -C "$loose_repo" commit -q -m 'loose baseline'
make_ops "$loose_repo/ops" "$((ops_count + 1))" "$batch_count"

init_repo "$packed_repo"
make_ops "$packed_repo/ops" 1 "$ops_count"
pack_range "$packed_repo" 1 "$ops_count"
git -C "$packed_repo" add -A
git -C "$packed_repo" commit -q -m 'packed baseline'
make_ops "$packed_repo/ops" "$((ops_count + 1))" "$batch_count"
pack_range "$packed_repo" "$((ops_count + 1))" "$((ops_count + batch_count))"

printf 'benchmark.ops=%s\n' "$ops_count"
printf 'benchmark.new_ops=%s\n' "$batch_count"
printf 'benchmark.runs=%s\n' "$benchmark_runs"
printf 'loose.tracked_paths=%s\n' "$(path_count "$loose_repo")"
printf 'packed.tracked_paths=%s\n' "$(path_count "$packed_repo")"
printf 'loose.dirty_paths=%s\n' "$(dirty_count "$loose_repo")"
printf 'packed.dirty_paths=%s\n' "$(dirty_count "$packed_repo")"
printf 'loose.status_median_ms=%s\n' "$(median_ms "$loose_repo" status)"
printf 'packed.status_median_ms=%s\n' "$(median_ms "$packed_repo" status)"
printf 'loose.add_median_ms=%s\n' "$(median_ms "$loose_repo" add)"
printf 'packed.add_median_ms=%s\n' "$(median_ms "$packed_repo" add)"
printf 'loose.index_bytes_after_add=%s\n' "$(index_bytes_after_add "$loose_repo")"
printf 'packed.index_bytes_after_add=%s\n' "$(index_bytes_after_add "$packed_repo")"
printf 'loose.logical_bytes=%s\n' "$(tree_bytes "$loose_repo/ops")"
printf 'packed.logical_bytes=%s\n' "$(tree_bytes "$packed_repo/packs")"

merge_repo="$fixture_root/merge"
worktree_a="$fixture_root/worktree-a"
worktree_b="$fixture_root/worktree-b"
init_repo "$merge_repo"
make_ops "$merge_repo/ops" 1 10
git -C "$merge_repo" add -A
git -C "$merge_repo" commit -q -m 'merge base'
git -C "$merge_repo" branch pack-a
git -C "$merge_repo" branch pack-b
git -C "$merge_repo" worktree add -q "$worktree_a" pack-a
git -C "$merge_repo" worktree add -q "$worktree_b" pack-b

pack_range "$worktree_a" 1 6
git -C "$worktree_a" add -A
git -C "$worktree_a" commit -q -m 'pack overlapping range A'

pack_range "$worktree_b" 4 9
make_ops "$worktree_b/ops" 11 1
git -C "$worktree_b" add -A
git -C "$worktree_b" commit -q -m 'pack overlapping range B and publish loose op'

git -C "$worktree_a" merge -q --no-edit pack-b
printf 'merge.conflicts=%s\n' "$(git -C "$worktree_a" ls-files --unmerged | wc -l | tr -d ' ')"
printf 'merge.loose_files=%s\n' "$(find "$worktree_a/ops" -type f -name '*.json' | wc -l | tr -d ' ')"
printf 'merge.pack_files=%s\n' "$(find "$worktree_a/packs" -type f -name '*.jsonl' | wc -l | tr -d ' ')"
logical_union "$worktree_a"
