# Profiling

## Benchmarking (criterion)

`benches/pipeline.rs` and `benches/render.rs` give statistical baselines for
the hot pipeline stages: pixel rotation, JPEG encoding, and every stage of the
SVG render pipeline (Tera context build, template substitution, SVG parse,
rasterize, composite, background decode, premultiplied→straight conversion).

Run all benches:

```bash
cargo bench
```

Compare before/after a change:

```bash
cargo bench -- --save-baseline before
# ... make your change ...
cargo bench -- --baseline before
```

Criterion baselines are saved under `target/criterion` and are local-only —
they don't survive across machines. Cross-machine regression detection is
this benchmark suite's job; the scenario harness (below) captures
machine-specific, whole-daemon numbers instead.

<!-- Scenario harness usage (flamegraphs, dhat allocation profiles, RSS
     timelines) is documented here once scripts/profile.sh lands. -->
