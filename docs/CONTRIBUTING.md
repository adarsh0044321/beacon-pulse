# Contributing to Beacon & Pulse

First off, thank you for considering contributing to Beacon & Pulse! We appreciate your time and effort.

## How Can I Contribute?

### Reporting Bugs
- Check if the bug has already been reported in our Issues section.
- Open a new issue using the "Bug Report" template.
- Include OS version, logs (`appdata/LANShare/logs`), and clear reproduction steps.

### Suggesting Enhancements
- Use the "Feature Request" template.
- Explain the current behavior and your proposed changes.

### Pull Requests
1. Fork the repo and create your branch from `main`.
2. Ensure you have run `cargo fmt` and `cargo clippy`.
3. Test your changes manually to ensure the streaming latency hasn't degraded.
4. Update documentation if necessary.
5. Issue a Pull Request with a clear title and description.

## Styleguides

### Git Commit Messages
- Use the present tense ("Add feature" not "Added feature").
- Limit the first line to 72 characters.
- Reference issues and pull requests liberally.

### Code Style
- We follow standard `rustfmt` formatting.
- Please use descriptive variable names and comment complex logic, especially around unsafe API blocks (like WinAPI).
