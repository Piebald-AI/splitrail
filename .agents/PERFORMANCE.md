# Performance Considerations

## Current Optimizations

### Parallel Loading

Analyzers load in parallel using `futures::join_all()`:

```rust
let results = futures::future::join_all(
    analyzers.iter().map(|a| a.get_stats())
).await;
```

### Parallel Parsing with Rayon

Use `.par_iter()` when parsing multiple files:

```rust
use rayon::prelude::*;

let messages: Vec<ConversationMessage> = files
    .par_iter()
    .flat_map(|file| parse_file(file))
    .collect();
```

### Fast JSON Parsing

Use `simd_json` instead of `serde_json` for performance:

```rust
let mut buffer = std::fs::read(path)?;
let data: YourType = simd_json::from_slice(&mut buffer)?;
```

### Fast Directory Walking

Use `jwalk` for parallel directory traversal:

```rust
use jwalk::WalkDir;

let files: Vec<PathBuf> = WalkDir::new(root)
    .into_iter()
    .filter_map(|e| e.ok())
    .filter(|e| e.path().extension() == Some("json"))
    .map(|e| e.path())
    .collect();
```

### Lazy Message Loading

TUI loads messages on-demand for session view to reduce memory usage.

## Known Issues

- High memory usage with large message counts (see `PROMPT.MD` for investigation notes)

## Profiling

Use `cargo flamegraph` for CPU profiling:

```bash
cargo install flamegraph
cargo flamegraph --bin splitrail
```

For memory profiling, consider `heaptrack` or `valgrind --tool=massif`.

## Guidelines

1. Prefer parallel processing for I/O-bound operations
2. Use `parking_lot` locks over `std::sync` for better performance
3. Avoid loading all messages into memory when not needed
4. Use `BTreeMap` for date-ordered data (sorted iteration)
