# Performance Considerations

## Techniques Used

- **Parallel analyzer loading** - `futures::join_all()` for concurrent stats loading
- **Parallel file parsing** - `rayon` for parallel iteration over files
- **Fast JSON parsing** - `simd_json` instead of `serde_json`
- **Fast directory walking** - `jwalk` for parallel directory traversal
- **Lazy message loading** - TUI loads messages on-demand for session view

See existing analyzers in `src/analyzers/` for usage patterns.

## Guidelines

1. Prefer parallel processing for I/O-bound operations
2. Use `parking_lot` locks over `std::sync` for better performance
3. Avoid loading all messages into memory when not needed
4. Use `BTreeMap` for date-ordered data (sorted iteration)