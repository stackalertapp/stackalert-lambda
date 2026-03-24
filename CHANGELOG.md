# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-03-20

### Added

- Initial release
- Cost spike detection: alerts when service spend exceeds 7-day average by configurable threshold
- Daily cost digest: summary of top services by average daily spend
- Telegram alerts via Bot API
- Telegram bot token stored securely in SSM Parameter Store
- Single-account mode (Lambda's own credentials)
- Cross-account mode (optional IAM role assumption via STS)
- ARM64 (Graviton) Lambda for cost efficiency
- `cargo-deny` for dependency auditing (licenses, advisories, bans)
- Trivy security scanning in CI
- Renovate for automated dependency updates
