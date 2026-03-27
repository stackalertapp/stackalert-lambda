## Rust Development Guidelines

- Always check `cargo clippy -- -D warnings` before suggesting code is complete
- Prefer `thiserror` for library errors, `anyhow` for binary/Lambda errors
- Use `tokio` async runtime; avoid `async-std`
- For AWS Lambda: use `lambda_runtime` + `aws-sdk-*` crates
- Run `cargo test` and `cargo fmt --check` in CI
- Use `OnceCell` / `OnceLock` for lazy static SDK clients (not `lazy_static`)
```

This matches your existing Lambda Rust patterns from earlier work.

---

## 📚 Learning Resources that work well with Claude

| Resource | How to use with Claude |
|---|---|
| **The Rust Book** (doc.rust-lang.org/book) | Ask Claude to walk through chapters with exercises |
| **Rustlings** (`cargo install rustlings`) | Paste failing exercises directly into Claude |
| **Rust by Example** | Ask Claude to extend examples for your use case |
| **Jon Gjengset's videos** (YouTube) | Watch, then ask Claude to explain specific concepts |
| **crates.io** + Context7 | Ask Claude "using the latest tokio API, write X" |

---

## 🔧 Recommended Stack for Rust + Claude Code
```
rust-mcp-server     → cargo toolchain (build/test/fmt/clippy)
rust-analyzer MCP   → LSP intelligence (refactoring, navigation)
Context7            → up-to-date crate docs
GitHub MCP          → CI/CD, PR management
filesystem MCP      → full project read/write access
