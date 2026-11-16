# Yukino LSP

Official Language Server Protocol implementation and command-line tooling for the [Yukino](https://github.com/yukitsuri/yukino) programming language.

## Installation

### Via lacie

Install [lacie](https://github.com/shareui/lacie/tree/main/docs/install) first, then run:

```bash
lacie add github:yukitsuri/yukino-lsp
```

### Via Nix

```bash
nix profile add github:yukitsuri/yukino-lsp
```

Or run directly without installing:

```bash
nix run github:yukitsuri/yukino-lsp -- lsp
```

## CLI Usage

```bash
# Run language server over stdio
yukino-lsp lsp

# Check syntax and diagnostics
yukino-lsp check # <path>

# Format a source file
yukino-lsp format # <file.ykn>

# Show symbol documentation
yukino-lsp doc # <symbol>
```

## License

MIT License (c) 2026 shareui
