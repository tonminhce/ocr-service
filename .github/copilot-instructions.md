# Repository rules

- Use the existing Rust workspace, architecture, bounded concurrency and public contracts. Prefer small changes and existing abstractions; do not add speculative configuration, threads or plugins.
- Do not introduce C-style block comments (`/* ... */`, `/** ... */`, `/*! ... */`) in maintained source. Use clear names and small functions. Retain concise line comments where they explain safety, invariants or non-obvious decisions; public API documentation uses line doc comments. Run `python3 scripts/check_no_block_comments.py` before committing.
- Do not remove or change executable behavior, SQL bodies or published migration bytes just to remove a comment. Preserve the intent of regression fixtures with an explicit behavior-preserving change.
- Preserve raw OCR and evidence. Keep conflicts unselected, uncalibrated confidence null, automatic acceptance disabled and schemas backward compatible.
- Keep uploads, real OCR results, secrets, artifacts and `.agent-handoff` out of Git. Use synthetic data and disposable services for tests. Do not print secret-bearing environment, expanded Compose configuration or private document text.
- Preserve the live stack and volumes. No push, live cutover, credential rotation, destructive retention or real-model concurrency increase without the required authorization and measured gates.
- Read `AGENTS.md` when present. Use the installed GitNexus impact analysis before modifying existing symbols and `detect_changes` before committing; report high-risk blast radius. Treat index/tool limitations honestly and inspect actual source.
- Follow `docs/production-execution-plan.md`. Provide traceable test evidence and limitations; synthetic tests do not prove production OCR quality or throughput.
