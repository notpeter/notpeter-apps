# CONL Schema v1.0

A CONL schema provides validation and auto-completion for CONL documents.

It embeds the philosophy of CONL: post-minimal, easy to read, easy to edit, and easy to parse.

A CONL schema is itself a CONL document with two top-level keys. definitions is a map of named definitions, and root names the definition that should be at the root of the document.

For example:

```conl
root = <server>
definitions
  server
    required keys
      type = server
    keys
      listen = <addr>

  addr
    keys
      host = .*
      port = \d+
Matches the CONL document:

type = server
listen
  host = localhost
  port = 8080
```

The expressivity of a CONL schema roughly matches that of a regular expression. If you pair it with an re2 based regular expression engine, you can avoid the exponential matching time possible with JSON-schema.

## Definitions

There are four kinds of definition:

- To match a map (with required keys and/or keys)
- To match a list (with required items and/or items)
- To match a scalar (with scalar)
- To match a mix (with any of)

## Scalar

A scalar definition requires that the value is a scalar, and it is matched by the given matcher.

```conl
definitions
  example
    scalar = [[matcher]]
```

## Any of

An any of definition requires that the value matches one (or more) of the given matchers.

```conl
definitions
  example
    any of
      = [[matcher]]
      = [[matcher]]
      = ...
```

## List

A list definition requires that the value is a list.

Specifying required items allows you to match a tuple of a fixed length where different positions have different types. Each value in the list must match the corresponding matcher.

Specifying items matches a list of arbitrary length where values are of the same type. Every item in the list must match the matcher.

Specifying both lets you require a prefix that matches the required items, followed by any number of values that match the items matcher.

```conl
definitions
  example
    required items
      = [[matcher]]
      = [[matcher]]
      = ...
    items = [[matcher]]
```

## Map

A map definition requires that the value is a map. As in CONL, each key in the map must be unique.

If required keys are specified, then the value must contain exactly one key-value pair that matches every required key-value pair.

If keys are specified, then the value may contain any number (including zero) key-value pairs that match the key-value pairs.

The value cannot contain any keys that are not present in either. If there are no required keys, then the value may be empty.

```conl
definitions
  example
    keys
      [[matcher]] = [[matcher]]
      [[matcher]] = [[matcher]]
      ...
    required keys
      [[matcher]] = [[matcher]]
      [[matcher]] = [[matcher]]
      ...
```

It is worth calling out that matching considers both the key and the values. This lets you (in combination with an any of definition) match multiple different types of value in the same position.

```conl
definitions
  example
    any of
      = <server>
      = <client>

  server
    required keys
      type = server
    keys
      ...

  client
    required keys
      type = client
    keys
      ...
```

## Matchers

A matcher describes how a value should be matched. There are two possible kinds of matcher: references (<.\*>) refer to other definitions, and patterns (regular expressions) which match scalars and map keys.

A matcher is typically represented as a CONL scalar, but it can also be paired with markdown documentation, in which case it is expressed as a CONL map with the matches and a documentation.

```conl
; matches any scalar value
definitions
  example
    scalar = .*

; equivalent to the above, but with docs
definitions
  example
    scalar
      matches = .*
      docs = """markdown
        Matches any scalar
```

## References

If the matcher starts with an < and ends with > it references an existing definition.

```conl
definitions
  example
    required keys
      mode = <mode>

  mode
    one of
      = server
      = client
```

This allows the example’s mode key to be set to either server or client (which are themselves pattern matchers).

While it is possible to use this to build up recursive structures (where a map can have values that are the same type). It is an error to set up a cyclical definition where one definition is defined in terms of itself, for example:

```conl
definitions ; invalid schema
  a
    scalar = <b>
  b
    one of = <a>
```

## Patterns

Patterns in CONL are regular expressions. To make it possible to match strings easily, the pattern must match the entire value, and `.` matches any unicode code-point (including \n). For example:

```conl
definitions
  example
    any of
      = one      ; "one" (not "done")
      = (?i)two  ; two, Two, TWO, etc.
      = .*       ; "", "a\nb", etc.
      = [^\r\n]+ ; "any one-line scalar"
```

Currently the exact details of regular expression matching are implementation defined (to make it easy to use different regex engines as a schema implementor). It is recommended to stick to the basics when it comes to regular expressions to ensure your schemas work with future validators that use different regex engines. The reference implementation uses Go's regexp package.

It is also worth noting that while CONL has three ways to represent scalars, they are not distinguishable. A matcher that matches the string one will match all of these equivalently:

```conl
= one
= "one"
= """markdown
  one
```

## Future extensions

CONL schema may evolve over the future to support features beyond just validating the struture of the document. For example it might be useful to know if a scalar should be represented in JSON as a string or a number; or whether certain values or keys are deprecated. As such, CONL schema parsers should not error if they encounter unknown keys in maps representing matchers.

We do not expect to make changes that would cause validators to be unable to validate existing schemas.
