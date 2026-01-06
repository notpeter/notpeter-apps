# CONL

CONL is a post-minimal format for configuration files. It is easy to read, easy to edit, and easy to parse.

CONL provides a standard data model of maps, lists and scalars; uses indentation to define structure; and defers type-assignment to parse time.

```conl
; a conl document defines key value pairs
; values are usually scalars
port = 8080

; but can also be lists of values
watch_directories
  = ~/go
  = ~/rust

; or maps of keys to values
env
  REGION = us-east1
  QUEUE_NAME = example-queue

; multiline scalars work like markdown
; with an optional syntax hint
init_script = """bash
  #!/usr/bin/env bash
  echo "hello world!"
```

In CONL there is no special syntax for numbers or booleans. Instead the parser interprets the scalar as required by the context. This avoids the need for quotes around strings in TOML, and the "Norway" problem in YAML. It encourages adding units to numbers, so that the intent is clear.

Scalars may be quoted, but this is rarely needed in practice. For example the empty scalar is represented as "". The quote marks do not affect the type of the scalar.

```conl
; keys and values can contain anything
; except ; (and = for keys).
welcome message = Whatcha "%n"!

; types are deferred until parse time;
; the app knows what it wants.
enabled = yes
country_code = no

; units are encouraged for numeric values
timeout = 500ms

; if you need an empty string or
; escape sequences, use quotes.
empty_string = ""
indent = "\t"

; the following escape sequences
; work inside quoted literals
escape_sequences
  = "\\" ; '\'
  = "\"" ; '"'
  = "\t" ; tab
  = "\n" ; newline
  = "\r" ; carriage return
  = "\{1F321}" ; 🐱 (or any codepoint)
```

CONL uses indentation for structure to make it trivial to comment out sections of the document. This also avoids needing to balance braces or brackets as in JSON. The rules around indentation are simpler than YAML: a map or a list always starts on a new indented line.

In the case a key is present, but not followed by a scalar or an indented map or list, it is said to have no value. This acts as a default for maps and lists, and is typically an error when the parser expects a scalar.

```conl
; values can be left undefined or empty
; to request a sensible default
theme
  ; background = #fff

; a list of maps
hosts
  =
    hostname = nix1
    port = 80
  =
    hostname = nix2
    port = 8080

; a map of lists
allow_list
  domains
    = example.com
    = example.dev
  ips
    = ::1
```
