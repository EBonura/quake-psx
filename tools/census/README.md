# Original-data feature census

This dependency-free Rust tool reads user-provided original PACK archives, BSP entity fields and progs.dat function/field/global names. It never extracts executable statements or copies original source. PAK0 contains nine campaign levels and twelve pickup/prop BSP assets; registered maps require PAK1. Original executable names expose additional monster and attack contracts even when no available BSP spawns them. Names establish contract presence, not semantics or parity.

Run from the repo:

```
CARGO_TARGET_DIR=/absolute/private/target cargo run --locked --manifest-path tools/census/Cargo.toml -- /absolute/output tools/census/evidence.tsv . /absolute/original/PAK0.PAK
```

Additional PACK paths may follow. Duplicate map names fail rather than silently overriding data. Outputs contain every original entity with authored fields and location, executable metadata indices, conservative feature rows and a deterministically ranked queue. Missing implementation, implemented without a journey receipt, and unverified evidence have separate counts.

The six TSV evidence columns are feature, status, prose evidence, journey file::assertion or `-`, receipt path or `-`, and source file::symbol or `-`. Implemented rows require an existing source file and named symbol. This is an evidence registry, not automatic semantic analysis: maintain it when implementation changes. A classname table alone is not evidence. Contract and map registry entries must exist in supplied originals; stale entries fail.

A journey exercise requires a tracked assertion file and a passing receipt bound to unchanged tracked source HEAD and the current journey SHA256. Receipt TSV fields are `feature`, `assertion`, `pass` (`true`), `source_sha` (full git HEAD), and `journey_sha256`. Unbound, absent or mismatched receipts fail. Valid exercise changes a row to implemented-exercised and removes it from the pending queue. The default journey file's presence is checked at each run; presence alone proves no exercise.

This first registry was audited against runtime main 56efc0308cd0669908fac1cce817c6ed037217ab. Its shipping journey is absent. Historical diagnostic gates are not source-bound shipping survival receipts. All generated reports belong outside the source worktree. No original assets or extracted original data should be committed.
