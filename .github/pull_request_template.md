## Summary

## Checklist

- [ ] `cargo test --release` passes (root, `tools/model_infer`, `tools/d5_probe`) and `cd python && python -m pytest` passes
- [ ] No serial numbers, user names, paths, tokens or telemetry in the diff
- [ ] Safety-relevant changes (writes, bounds, fallback) have a test and are described above
- [ ] Docs updated
