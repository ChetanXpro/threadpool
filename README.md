# Rust Thread Pool Implementation

A thread pool implementation to learn Rust concurrency fundamentals. built to understand how to handle non-determinism in concurrent programs and explore Rust's concurrency primitives.

## What I Learned

### Concurrency Challenges

- Thread interleaving makes concurrent programs non-deterministic
- Memory reordering affects how threads see shared memory
- Weak vs strong memory ordering and their performance implications

### Rust's Concurrency APIs

- Lock-based (`Mutex`, `Condvar`) vs lock-free (`Atomic`) synchronization
- Message passing with channels
- Thread safety through ownership and type system
- Atomic operations and memory ordering

### Some Takeaways

- When to use different synchronization methods
- How to handle panics across thread boundaries
- Testing concurrent code is tricky - race conditions aren't always visible
- Performance tradeoffs in concurrent design

## Features

- Configurable thread count and shutdown timeout
- Metrics tracking (completed jobs, failures, etc.)
- Graceful shutdown handling
- Panic recovery in worker threads

## Usage Example

```rust
let pool = Threadpool::new().unwrap();

// Execute a task
pool.execute(|| {
    println!("Running in thread pool!");
}).unwrap();

// Get metrics
let metrics = pool.get_metrics();
println!("Completed jobs: {}", metrics.completed_jobs());
```

## Future Work

- Job priorities
- Dynamic pool resizing
- Backpressure mechanisms
