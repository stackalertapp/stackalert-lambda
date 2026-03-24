# Contributing to StackAlert Lambda

Thank you for your interest in contributing! This guide will help you get started.

## Getting Started

### Prerequisites

- [Rust](https://rustup.rs/) (stable, latest)
- [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) for dependency auditing

### Setup

```bash
git clone https://github.com/stackalertapp/stackalert-lambda.git
cd stackalert-lambda
cargo build
cargo test
```

### Running Checks Locally

Before submitting a PR, make sure all checks pass:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny check
```

## Making Changes

1. Fork the repository
2. Create a feature branch: `git checkout -b feat/your-feature`
3. Make your changes
4. Add or update tests
5. Run all checks (see above)
6. Commit with a descriptive message
7. Push and open a pull request

## Pull Request Guidelines

- Keep PRs focused — one feature or fix per PR
- Add tests for new functionality
- Update documentation if behavior changes
- All CI checks must pass

## Code Style

- Follow standard Rust formatting (`cargo fmt`)
- No clippy warnings
- Use `anyhow` for error handling in application code, `thiserror` for library errors
- Prefer `tracing` over `println!` for logging

## Reporting Issues

- Use GitHub Issues for bug reports and feature requests
- Include reproduction steps for bugs
- Check existing issues before creating a new one

## License

By contributing, you agree that your contributions will be licensed under the Apache License 2.0.
