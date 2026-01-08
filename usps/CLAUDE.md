## Rules

- Never delete the `cache` folder
- Ask before adding project dependencies
- You are coding something in rust -- you may use other languages, shell, shell commands, etc to explore data, but all programmatic output should ultimately become rust code.

## CONL Format

For details of the CONL configuration file format see the conl/ directory:

- conl/spec.md
- conl/spec.conl
- conl/example.conl

CONL Schema is documented here:

- conl/schema.md
- conl/schema.conl

Multiline strings can be prefixed by an optional `"""{filetype}` prefix.
Never include a `"""` trailer. Example:

```conl
about = """md
  A description
  On multiple lines
```

Rust Crate: [`conl`](https://crates.io/crates/conl).
