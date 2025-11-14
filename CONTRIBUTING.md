# Contributing to Kafka CLI

Thank you for your interest in contributing to Kafka CLI! This document provides guidelines for contributing to the project.

## Getting Started

1. Fork the repository on GitHub
2. Clone your fork locally:
   ```bash
   git clone https://github.com/YOUR_USERNAME/kafka-cli.git
   cd kafka-cli
   ```

3. Add the upstream repository:
   ```bash
   git remote add upstream https://github.com/lihongjie0209/kafka-cli.git
   ```

## Development Setup

### Prerequisites

- Rust 1.70 or later
- Docker and Docker Compose (for testing)
- Git

### Platform-Specific Setup

#### Linux/macOS
```bash
# Install librdkafka
# Ubuntu/Debian
sudo apt-get install librdkafka-dev

# macOS
brew install librdkafka
```

#### Windows
```powershell
# Install librdkafka via vcpkg
vcpkg install librdkafka:x64-windows

# Set environment variable
$env:VCPKGRS_DYNAMIC = "1"
```

See [BUILD_SUCCESS.md](BUILD_SUCCESS.md) for detailed Windows build instructions.

### Building

```bash
cargo build
```

### Running Tests

Start Kafka:
```bash
cd docker/single-node
docker-compose up -d
cd ../..
```

Run tests:
```bash
# Unit tests
cargo test --lib

# Integration tests
cargo test --test integration_test -- --test-threads=1

# Functional tests
python tests/test_cli_functional.py
```

## Making Changes

1. Create a new branch for your feature or bugfix:
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. Make your changes and commit them:
   ```bash
   git add .
   git commit -m "Description of your changes"
   ```

3. Keep your branch up to date with upstream:
   ```bash
   git fetch upstream
   git rebase upstream/master
   ```

4. Push to your fork:
   ```bash
   git push origin feature/your-feature-name
   ```

5. Create a Pull Request on GitHub

## Code Style

- Follow Rust standard style guidelines
- Run `cargo fmt` before committing
- Run `cargo clippy` and fix any warnings
- Add tests for new functionality
- Update documentation as needed

## Commit Messages

- Use clear and descriptive commit messages
- Start with a verb in present tense (e.g., "Add", "Fix", "Update")
- Keep the first line under 72 characters
- Provide additional details in the commit body if needed

Example:
```
Add support for SASL authentication

- Implement SASL/PLAIN mechanism
- Add configuration options for username/password
- Update documentation with authentication examples
```

## Testing Guidelines

- Write unit tests for new functions
- Add integration tests for new features
- Ensure all tests pass before submitting PR
- Test on multiple platforms if possible (Windows, Linux, macOS)

### Test Coverage

We aim for high test coverage. Please include tests for:
- New features
- Bug fixes
- Edge cases
- Error handling

## Documentation

- Update README.md if adding new features
- Add inline documentation for public APIs
- Update user guides in the `docs/` directory
- Include examples in documentation

## Pull Request Process

1. Ensure your code builds and all tests pass
2. Update documentation as needed
3. Describe your changes in the PR description
4. Reference any related issues
5. Wait for code review and address feedback

## Code Review

All submissions require review. We use GitHub pull requests for this purpose.

Reviewers will check:
- Code quality and style
- Test coverage
- Documentation
- Performance implications
- Security considerations

## Reporting Bugs

When reporting bugs, please include:
- Clear description of the issue
- Steps to reproduce
- Expected vs actual behavior
- Environment details (OS, Rust version, Kafka version)
- Relevant logs or error messages

## Suggesting Features

We welcome feature suggestions! Please:
- Check if the feature has already been requested
- Describe the use case and benefits
- Provide examples of how it would work
- Consider implementation complexity

## Code of Conduct

- Be respectful and constructive
- Welcome newcomers and help them get started
- Focus on the code, not the person
- Accept feedback gracefully

## Questions?

If you have questions, feel free to:
- Open an issue on GitHub
- Join discussions in existing issues
- Check the documentation in the `docs/` directory

## License

By contributing, you agree that your contributions will be licensed under the MIT License.

Thank you for contributing to Kafka CLI! 🎉
