# Phase 4 project-hygiene baseline

Measured 2026-10-14 on the development host: NixOS/Linux 6.18.55, x86_64,
Intel Xeon E5-2420 v2 @ 2.20 GHz; Rust 1.95.0 and Git 2.54.0. This is a
portable-backend fixture baseline, not a macOS/APFS performance claim. The
installed macOS SDK and macOS runtime are unavailable on this host.

Reproduce with:

```sh
cargo run --release -p cleanrs-core --example project_hygiene -- 100 50 3
cargo run --release -p cleanrs-core --example project_hygiene -- 160 50 1
```

The example creates and removes a unique temporary tree. It never scans or
cleans the real home directory and never runs `--apply`. Fixture creation and
Git commits finish before timing starts. Every project has 50 tracked source
files, a package marker, a Git ignore file and 50 ignored `.next` output files.
The scanner includes Git admin metadata: the 100-project tree has 15,201
entries, while the 160-project tree has 24,321 entries. Repositories are clean,
have one recent commit and have no remote, so these fixtures exercise native
Git inspection and source protection without network access.

All runs are warm-cache observations: the fixture was just written, and no
attempt was made to flush filesystem caches. Repeated runs include ordinary
host scheduling noise. Raw results are in
[benchmarks-phase4.jsonl](benchmarks-phase4.jsonl).

For 100 repositories, each run inspected all 100 using 600 sequential Git
commands, with no omissions:

- Run 1: scan 69.6 ms; target detection 15.8 ms; project hygiene 5.506 s; process peak RSS 7,716 KiB.
- Run 2: scan 68.3 ms; target detection 14.1 ms; project hygiene 5.447 s; process peak RSS 8,352 KiB.
- Run 3: scan 36.5 ms; target detection 7.0 ms; project hygiene 4.989 s; process peak RSS 10,400 KiB.

Median project-hygiene time was 5.447 s.
Each repository uses one config-safety query plus status, remote names, last
commit time, worktree relationships and artifact-scoped tracked paths. Config
inspection disables content filters before status and rejects external includes
and partial-clone metadata. A cheaper process poll interval is used for these
short commands; there is no worker or subprocess per repository.

The 160-repository budget fixture inspected 128 repositories
using exactly 768 commands and visibly omitted
32. It took 3.949 s for hygiene,
112.7 ms for scanning and 13.3 ms for
target detection; process peak RSS was 9,808 KiB. The
omitted repositories remain unknown and cannot authorize artifact cleanup.
The normal bounds are 128 repositories, 768 commands and a 15-second budget
checked between commands; each command additionally has a 2-second timeout
and 4 MiB output limit. Time exhaustion can stop inspection before count limits.

RSS is a high-water mark for the benchmark process, including fixture setup,
and excludes child Git processes. Linux uses `/proc/self/status` VmHWM;
`getrusage` can retain a launching process's old high-water mark across exec.
Other supported Unix benchmark builds use `getrusage`, converting macOS bytes
to KiB. Child process count is bounded, but those child processes' RSS is not
represented by this number. Entries are rescanned for each timed run; no
persistent index is involved.

For regression comparison, run the same release command with the same fixture
parameters and tool versions on the same machine, retain the JSONL, and compare
median hygiene time, scan time, peak RSS, commands and omitted counts. Investigate
material changes rather than treating noisy wall-clock thresholds as tests.
A macOS run should be recorded separately, preserving the same parameters and
measurement boundaries. The benchmark does not claim a cold-cache result or
support for scanning a real home tree.
